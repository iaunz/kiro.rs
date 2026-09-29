//! Recover only from an explicit image dimension limit reported by the Kiro endpoint.

use reqwest::StatusCode;

use super::image_normalization::normalize_images;
use super::model::requests::kiro::KiroRequest;

/// Do not infer Kiro limits from the model name, image count or another provider's docs.
pub(super) fn image_dimension_limit(status: StatusCode, error_body: &str) -> Option<u32> {
    if status != StatusCode::BAD_REQUEST {
        return None;
    }
    let error: serde_json::Value = serde_json::from_str(error_body).ok()?;
    if error.get("reason")?.as_str()? != "IMAGE_DIMENSION_EXCEEDED" {
        return None;
    }
    let message = error.get("message")?.as_str()?.to_ascii_lowercase();
    let (_, limit) = message.split_once("max allowed size")?;
    let limit = limit
        .strip_prefix(" for many-image requests:")
        .or_else(|| limit.strip_prefix(':'))?;
    let mut words = limit.split_whitespace();
    let pixels: u32 = words.next()?.parse().ok()?;
    let unit = words.next()?.trim_end_matches(['.', ',', ';']);
    (pixels > 0 && unit == "pixels").then_some(pixels)
}

/// The request is our serialized KiroRequest, before endpoint-specific transformations.
/// Decode and resize off the async networking threads; never cache a limit across requests.
pub(super) async fn resize_request_images(
    request_body: String,
    max_dimension: u32,
) -> anyhow::Result<Option<String>> {
    tokio::task::spawn_blocking(move || {
        let mut request: KiroRequest = serde_json::from_str(&request_body)
            .map_err(|_| anyhow::anyhow!("cannot read the Kiro request for image recovery"))?;
        let changed = normalize_images(&mut request.conversation_state, max_dimension)
            .map_err(anyhow::Error::msg)?;
        if !changed {
            return Ok(None);
        }
        serde_json::to_string(&request)
            .map(Some)
            .map_err(|_| anyhow::anyhow!("cannot encode the resized Kiro request"))
    })
    .await
    .map_err(|_| anyhow::anyhow!("image recovery worker failed"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn error(message: &str) -> String {
        json!({"reason": "IMAGE_DIMENSION_EXCEEDED", "message": message}).to_string()
    }

    #[test]
    fn uses_the_explicit_kiro_limit_instead_of_a_fixed_claude_limit() {
        for limit in [1000, 2000, 3072, 8000] {
            let message = format!(
                "messages.72.content.1.image.source.base64.data: At least one of the image dimensions exceed max allowed size for many-image requests: {limit} pixels"
            );
            assert_eq!(
                image_dimension_limit(StatusCode::BAD_REQUEST, &error(&message)),
                Some(limit)
            );
        }
        assert_eq!(
            image_dimension_limit(
                StatusCode::BAD_REQUEST,
                &error("image dimensions exceed max allowed size: 8000 pixels.")
            ),
            Some(8000)
        );
    }

    #[test]
    fn unknown_errors_and_ambiguous_limits_are_not_retried() {
        let message = "max allowed size for many-image requests: 2000 pixels";
        for body in [
            "not json".to_string(),
            json!({"reason": "IMAGE_TOO_LARGE", "message": message}).to_string(),
            json!({"message": message}).to_string(),
            error("image dimensions exceeded"),
            error("image is 2000 pixels wide"),
            error("max allowed size for many-image requests: 5 MB"),
            error("max allowed size for many-image requests: 0 pixels"),
            error("max allowed size for many-image requests: -2000 pixels"),
            error("max allowed size for many-image requests: 4294967296 pixels"),
        ] {
            assert_eq!(
                image_dimension_limit(StatusCode::BAD_REQUEST, &body),
                None,
                "{body}"
            );
        }
        for status in [
            StatusCode::OK,
            StatusCode::FORBIDDEN,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            assert_eq!(image_dimension_limit(status, &error(message)), None);
        }
    }
}
