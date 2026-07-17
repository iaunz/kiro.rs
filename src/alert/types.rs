//! Alert Admin API DTO

use serde::{Deserialize, Serialize};

use super::config::{AlertConfig, ChannelKind};

pub fn mask_token(token: &str) -> String {
    if token.is_ascii() && token.len() > 12 {
        format!("{}...{}", &token[..6], &token[token.len() - 2..])
    } else {
        "***".to_string()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelResponse {
    pub id: String,
    pub kind: ChannelKind,
    pub enabled: bool,
    pub name: Option<String>,
    pub masked_bot_token: Option<String>,
    pub chat_id: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertConfigResponse {
    pub enabled: bool,
    pub threshold_remaining: f64,
    pub poll_interval_secs: u64,
    pub subject_prefix: Option<String>,
    pub channels: Vec<ChannelResponse>,
    pub smtp_configured: bool,
}

impl AlertConfigResponse {
    pub fn from_config(cfg: &AlertConfig, smtp_configured: bool) -> Self {
        let channels = cfg
            .channels
            .iter()
            .map(|c| ChannelResponse {
                id: c.id.clone(),
                kind: c.kind,
                enabled: c.enabled,
                name: c.name.clone(),
                masked_bot_token: c.bot_token.as_deref().map(mask_token),
                chat_id: c.chat_id.clone(),
                to: c.to.clone(),
            })
            .collect();
        Self {
            enabled: cfg.enabled,
            threshold_remaining: cfg.threshold_remaining,
            poll_interval_secs: cfg.poll_interval_secs,
            subject_prefix: cfg.subject_prefix.clone(),
            channels,
            smtp_configured,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateConfigRequest {
    pub enabled: Option<bool>,
    pub threshold_remaining: Option<f64>,
    pub poll_interval_secs: Option<u64>,
    pub subject_prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelRequest {
    pub kind: ChannelKind,
    pub enabled: Option<bool>,
    pub name: Option<String>,
    pub bot_token: Option<String>,
    pub chat_id: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusResponse {
    pub fired: bool,
    pub last_total_remaining: Option<f64>,
    pub last_evaluated_at: Option<i64>,
    pub last_threshold: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestChannelResult {
    pub label: String,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResponse {
    pub results: Vec<TestChannelResult>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_token() {
        assert_eq!(mask_token("1234567890:ABCDEFGH"), "123456...GH");
        assert_eq!(mask_token("short"), "***");
    }
}
