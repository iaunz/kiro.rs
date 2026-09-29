//! 按上游反馈的尺寸限制缩小图片，保留合规图片原始编码。

use std::io::Cursor;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageDecoder, ImageError, ImageFormat, ImageReader, Limits};

use crate::kiro::model::requests::conversation::{ConversationState, KiroImage, Message};

const MAX_BASE64_BYTES: usize = 64 * 1024 * 1024;
const MAX_DECODED_BYTES: u64 = 128 * 1024 * 1024;
const MAX_DECODE_DIMENSION: u32 = 32768;

/// 在转换完成的请求上统计图片，确保工具截图和历史图片使用同一套限制。
pub(crate) fn normalize_images(
    state: &mut ConversationState,
    max_dimension: u32,
) -> Result<bool, String> {
    if max_dimension == 0 {
        return Err("image dimension limit must be greater than zero".to_string());
    }
    let image_count = state.current_message.user_input_message.images.len()
        + state
            .history
            .iter()
            .map(|message| match message {
                Message::User(message) => message.user_input_message.images.len(),
                Message::Assistant(_) => 0,
            })
            .sum::<usize>();

    let mut resized_count = 0;
    for (index, image) in state
        .current_message
        .user_input_message
        .images
        .iter_mut()
        .enumerate()
    {
        let location = format!("currentMessage.userInputMessage.images[{index}]");
        resized_count += usize::from(normalize_image(
            image,
            &location,
            image_count,
            max_dimension,
        )?);
    }
    for (history_index, message) in state.history.iter_mut().enumerate() {
        if let Message::User(message) = message {
            for (index, image) in message.user_input_message.images.iter_mut().enumerate() {
                let location = format!("history[{history_index}].userInputMessage.images[{index}]");
                resized_count += usize::from(normalize_image(
                    image,
                    &location,
                    image_count,
                    max_dimension,
                )?);
            }
        }
    }
    tracing::debug!(
        image_count,
        resized_count,
        max_dimension,
        "已按上游限制检查图片尺寸"
    );
    Ok(resized_count > 0)
}

fn decoding_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_DECODE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODED_BYTES);
    limits
}

/// 不透传解码库错误，以免错误中的签名、元数据或字节片段泄露图片内容。
fn decoding_error(location: &str, error: ImageError) -> String {
    let reason = match error {
        ImageError::Limits(_) => "image exceeds decoding safety limits",
        _ => "invalid or unsupported image data",
    };
    format!("{location}: {reason}")
}

fn normalize_image(
    image: &mut KiroImage,
    location: &str,
    image_count: usize,
    max_dimension: u32,
) -> Result<bool, String> {
    if image.source.bytes.len() > MAX_BASE64_BYTES {
        return Err(format!(
            "{location}: base64 image exceeds safety size limit"
        ));
    }
    let format = match image.format.to_ascii_lowercase().as_str() {
        "png" => ImageFormat::Png,
        "jpeg" | "jpg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::WebP,
        _ => return Err(format!("{location}: unsupported image format")),
    };
    let bytes = STANDARD
        .decode(&image.source.bytes)
        .map_err(|_| format!("{location}: invalid base64 image data"))?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(decoding_limits());
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| decoding_error(location, error))?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return Err(format!("{location}: invalid image dimensions"));
    }
    if width <= max_dimension && height <= max_dimension {
        // 只读取头部，不重新编码或解码合规图片的像素流。
        return Ok(false);
    }

    // 与 ImageReader::decode 一样，先给输出像素缓冲区预留预算，再限制解码器。
    let mut limits = decoding_limits();
    limits
        .reserve(decoder.total_bytes())
        .map_err(|error| decoding_error(location, error))?;
    decoder
        .set_limits(limits)
        .map_err(|error| decoding_error(location, error))?;
    let orientation = decoder
        .orientation()
        .map_err(|error| decoding_error(location, error))?;
    let mut decoded =
        DynamicImage::from_decoder(decoder).map_err(|error| decoding_error(location, error))?;
    // JPEG 重编码会移除 EXIF；先应用方向，避免旋转后的内容发生变化。
    decoded.apply_orientation(orientation);
    let resized = decoded.resize(
        max_dimension,
        max_dimension,
        image::imageops::FilterType::Lanczos3,
    );
    let (output_format, output_name) = match format {
        ImageFormat::Jpeg => (ImageFormat::Jpeg, "jpeg"),
        ImageFormat::WebP => (ImageFormat::WebP, "webp"),
        // GIF 解码得到首帧；用 PNG 保存，保留透明通道且无需重新量化调色板。
        _ => (ImageFormat::Png, "png"),
    };
    let mut encoded = Cursor::new(Vec::new());
    resized
        .write_to(&mut encoded, output_format)
        .map_err(|_| format!("{location}: cannot encode resized image"))?;
    image.source.bytes = STANDARD.encode(encoded.into_inner());
    image.format = output_name.to_string();
    tracing::debug!(
        image_count,
        max_dimension,
        location,
        original_width = width,
        original_height = height,
        resized_width = resized.width(),
        resized_height = resized.height(),
        output_format = output_name,
        "已按上游限制缩小超限图片"
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::model::requests::conversation::{HistoryAssistantMessage, HistoryUserMessage};

    fn encoded_image(width: u32, height: u32, format: ImageFormat) -> KiroImage {
        let rgba = image::RgbaImage::from_pixel(width, height, image::Rgba([31, 93, 157, 127]));
        let image = if format == ImageFormat::Jpeg {
            DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(rgba).to_rgb8())
        } else {
            DynamicImage::ImageRgba8(rgba)
        };
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        KiroImage::from_base64(
            match format {
                ImageFormat::Jpeg => "jpeg",
                ImageFormat::Gif => "gif",
                ImageFormat::WebP => "webp",
                _ => "png",
            },
            STANDARD.encode(bytes.into_inner()),
        )
    }

    fn decode(image: &KiroImage) -> DynamicImage {
        image::load_from_memory(&STANDARD.decode(&image.source.bytes).unwrap()).unwrap()
    }

    fn state_with_current(image: KiroImage, count: usize) -> ConversationState {
        let mut state = ConversationState::new("image-normalization-test");
        state.current_message.user_input_message.images = vec![image];
        let mut historical = HistoryUserMessage::new("screenshots", "test-model");
        historical.user_input_message.images =
            vec![encoded_image(1, 1, ImageFormat::Png); count - 1];
        state.history = vec![
            Message::Assistant(HistoryAssistantMessage::new("screenshot captured")),
            Message::User(historical),
        ];
        state
    }

    #[test]
    fn normalization_is_independent_of_image_count() {
        for count in [1, 20, 21] {
            let mut state = state_with_current(encoded_image(2001, 2, ImageFormat::Png), count);
            assert!(normalize_images(&mut state, 2000).unwrap());
            let resized = decode(&state.current_message.user_input_message.images[0]);
            assert_eq!((resized.width(), resized.height()), (2000, 2));
            // 第二次没有任何图片需要修改，调用方不应再发送无效重试。
            assert!(!normalize_images(&mut state, 2000).unwrap());
        }
    }

    #[test]
    fn limit_is_supplied_by_the_upstream_response() {
        let mut state = state_with_current(encoded_image(4096, 32, ImageFormat::Png), 1);
        assert!(normalize_images(&mut state, 3072).unwrap());
        let resized = decode(&state.current_message.user_input_message.images[0]);
        assert_eq!((resized.width(), resized.height()), (3072, 24));
    }

    #[test]
    fn empty_requests_are_unchanged_and_zero_dimension_is_rejected() {
        let mut state = ConversationState::new("empty");
        assert!(!normalize_images(&mut state, 2000).unwrap());
        assert_eq!(
            normalize_images(&mut state, 0).unwrap_err(),
            "image dimension limit must be greater than zero"
        );
    }

    #[test]
    fn current_and_historical_images_share_the_limit_and_keep_valid_data_exactly() {
        let mut original = encoded_image(2000, 2, ImageFormat::Png);
        original.format = "PNG".into();
        let mut state = state_with_current(original.clone(), 21);
        let Message::User(history) = &mut state.history[1] else {
            panic!("expected user")
        };
        history.user_input_message.images[7] = encoded_image(2, 2001, ImageFormat::Png);
        normalize_images(&mut state, 2000).unwrap();
        assert_eq!(
            state.current_message.user_input_message.images[0].format,
            original.format
        );
        assert_eq!(
            state.current_message.user_input_message.images[0]
                .source
                .bytes,
            original.source.bytes
        );
        let Message::User(history) = &state.history[1] else {
            panic!("expected user")
        };
        let resized = decode(&history.user_input_message.images[7]);
        assert_eq!((resized.width(), resized.height()), (2, 2000));
        assert_eq!(history.user_input_message.images.len(), 20);
    }

    #[test]
    fn resize_preserves_aspect_ratio_in_both_orientations() {
        for (width, height, expected) in [(2400, 24, (2000, 20)), (24, 2400, (20, 2000))] {
            let mut state = state_with_current(encoded_image(width, height, ImageFormat::Png), 21);
            normalize_images(&mut state, 2000).unwrap();
            let image = decode(&state.current_message.user_input_message.images[0]);
            assert_eq!((image.width(), image.height()), expected);
        }
    }

    #[test]
    fn supported_formats_remain_decodable_and_alpha_is_preserved() {
        for (format, output) in [
            (ImageFormat::Png, "png"),
            (ImageFormat::Jpeg, "jpeg"),
            (ImageFormat::WebP, "webp"),
            (ImageFormat::Gif, "png"),
        ] {
            let original = encoded_image(2001, 2, format);
            let original_alpha = decode(&original).to_rgba8().get_pixel(0, 0).0[3];
            let mut state = state_with_current(original, 21);
            normalize_images(&mut state, 2000).unwrap();
            let result = &state.current_message.user_input_message.images[0];
            assert_eq!(result.format, output);
            let decoded = decode(result);
            assert_eq!((decoded.width(), decoded.height()), (2000, 2));
            assert_eq!(decoded.to_rgba8().get_pixel(0, 0).0[3], original_alpha);
        }
    }

    #[test]
    fn animated_gif_uses_first_frame_and_preserves_transparency() {
        let mut first = image::RgbaImage::from_pixel(2001, 2, image::Rgba([0, 0, 0, 0]));
        for y in 0..2 {
            for x in 0..1000 {
                first.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
        let second = image::RgbaImage::from_pixel(2001, 2, image::Rgba([0, 0, 255, 255]));
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder.encode_frame(image::Frame::new(first)).unwrap();
            encoder.encode_frame(image::Frame::new(second)).unwrap();
        }
        let mut state =
            state_with_current(KiroImage::from_base64("gif", STANDARD.encode(bytes)), 21);
        normalize_images(&mut state, 2000).unwrap();
        let result = &state.current_message.user_input_message.images[0];
        assert_eq!(result.format, "png");
        let pixels = decode(result).to_rgba8();
        assert_eq!(pixels.get_pixel(20, 0).0, [255, 0, 0, 255]);
        assert_eq!(pixels.get_pixel(1990, 0).0[3], 0);
    }

    #[test]
    fn jpeg_orientation_is_applied_before_exif_is_removed() {
        let mut original = encoded_image(2001, 2, ImageFormat::Jpeg);
        let bytes = STANDARD.decode(&original.source.bytes).unwrap();
        let mut with_exif = bytes[..2].to_vec();
        // APP1 with a little-endian TIFF orientation tag (6 = rotate 90 degrees).
        with_exif.extend_from_slice(&[
            0xff, 0xe1, 0, 34, b'E', b'x', b'i', b'f', 0, 0, b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0,
            0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
        ]);
        with_exif.extend_from_slice(&bytes[2..]);
        original.source.bytes = STANDARD.encode(with_exif);
        let mut state = state_with_current(original, 1);
        assert!(normalize_images(&mut state, 2000).unwrap());
        let result = decode(&state.current_message.user_input_message.images[0]);
        assert_eq!((result.width(), result.height()), (2, 2000));
    }

    #[test]
    fn invalid_base64_has_safe_current_image_location() {
        let secret = "private-image-payload%%%";
        let mut state = state_with_current(KiroImage::from_base64("png", secret), 21);
        let error = normalize_images(&mut state, 2000).unwrap_err();
        assert_eq!(
            error,
            "currentMessage.userInputMessage.images[0]: invalid base64 image data"
        );
        assert!(!error.contains(secret));
    }

    #[test]
    fn invalid_image_encoding_has_safe_history_image_location() {
        let secret = "private image bytes";
        let mut state = state_with_current(encoded_image(1, 1, ImageFormat::Png), 21);
        let Message::User(history) = &mut state.history[1] else {
            panic!("expected user")
        };
        history.user_input_message.images[3] =
            KiroImage::from_base64("png", STANDARD.encode(secret));
        let error = normalize_images(&mut state, 2000).unwrap_err();
        assert_eq!(
            error,
            "history[1].userInputMessage.images[3]: invalid or unsupported image data"
        );
        assert!(!error.contains(secret));
        assert!(!error.contains(&STANDARD.encode(secret)));
    }

    #[test]
    fn oversized_corrupt_pixel_data_is_not_forwarded() {
        let mut image = encoded_image(2001, 2, ImageFormat::Png);
        let mut bytes = STANDARD.decode(&image.source.bytes).unwrap();
        bytes.truncate(41); // PNG header is present but its pixel stream is missing.
        image.source.bytes = STANDARD.encode(bytes);
        let mut state = state_with_current(image, 21);
        let error = normalize_images(&mut state, 2000).unwrap_err();
        assert!(error.starts_with("currentMessage.userInputMessage.images[0]:"));
        assert!(error.contains("invalid or unsupported image data"));
    }

    #[test]
    fn excessive_image_dimensions_or_decoded_size_are_rejected_before_allocation() {
        for (width, height) in [(40000u32, 2u32), (10000, 10000)] {
            let mut image = encoded_image(2, 2, ImageFormat::Png);
            let mut bytes = STANDARD.decode(&image.source.bytes).unwrap();
            bytes[16..20].copy_from_slice(&width.to_be_bytes());
            bytes[20..24].copy_from_slice(&height.to_be_bytes());
            let checksum = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC).checksum(&bytes[12..29]);
            bytes[29..33].copy_from_slice(&checksum.to_be_bytes());
            image.source.bytes = STANDARD.encode(bytes);
            let mut state = state_with_current(image, 21);
            let error = normalize_images(&mut state, 2000).unwrap_err();
            assert_eq!(
                error,
                "currentMessage.userInputMessage.images[0]: image exceeds decoding safety limits"
            );
        }
    }
}
