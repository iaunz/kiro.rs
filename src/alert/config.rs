//! Alert 配置类型与 JSON 持久化

use std::path::Path;

use serde::{Deserialize, Serialize};

/// 通知渠道类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Telegram,
    Email,
}

/// 单个通知渠道
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertChannel {
    /// 渠道唯一 ID（uuid v4）
    pub id: String,
    pub kind: ChannelKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Telegram bot token（仅 telegram）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_token: Option<String>,
    /// Telegram chat id（仅 telegram）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    /// 收件邮箱（仅 email）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// 预警配置（持久化到 alert_config.json）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_threshold")]
    pub threshold_remaining: f64,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_prefix: Option<String>,
    #[serde(default)]
    pub channels: Vec<AlertChannel>,
}

fn default_true() -> bool {
    true
}

fn default_threshold() -> f64 {
    1000.0
}

fn default_poll_interval() -> u64 {
    1800
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_remaining: default_threshold(),
            poll_interval_secs: default_poll_interval(),
            subject_prefix: None,
            channels: Vec::new(),
        }
    }
}

impl AlertConfig {
    /// 从文件加载；文件不存在或解析失败时返回默认配置
    pub fn load(path: &Path) -> Self {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };
        match serde_json::from_str(&content) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("解析 alert 配置失败，使用默认配置: {}", e);
                Self::default()
            }
        }
    }

    /// 保存到文件
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let c = AlertConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.threshold_remaining, 1000.0);
        assert_eq!(c.poll_interval_secs, 1800);
        assert!(c.channels.is_empty());
    }

    #[test]
    fn test_save_then_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("alert_cfg_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("alert_config.json");

        let mut c = AlertConfig::default();
        c.enabled = true;
        c.threshold_remaining = 500.0;
        c.channels.push(AlertChannel {
            id: "abc".to_string(),
            kind: ChannelKind::Telegram,
            enabled: true,
            name: Some("bot".to_string()),
            bot_token: Some("123:XYZ".to_string()),
            chat_id: Some("-100".to_string()),
            to: None,
        });
        c.save(&path).unwrap();

        let loaded = AlertConfig::load(&path);
        assert!(loaded.enabled);
        assert_eq!(loaded.threshold_remaining, 500.0);
        assert_eq!(loaded.channels.len(), 1);
        assert_eq!(loaded.channels[0].kind, ChannelKind::Telegram);
        assert_eq!(loaded.channels[0].bot_token.as_deref(), Some("123:XYZ"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_load_missing_returns_default() {
        let path = std::env::temp_dir().join("definitely_missing_alert_cfg.json");
        std::fs::remove_file(&path).ok();
        let c = AlertConfig::load(&path);
        assert!(!c.enabled);
    }
}
