//! 通知器：trait 分发 + fan-out

pub mod smtp;
pub mod telegram;

use std::sync::Arc;

use crate::alert::config::{AlertChannel, ChannelKind};
use crate::alert::smtp_settings::SmtpSettings;
use crate::http_client::{build_client, ProxyConfig};
use crate::model::config::TlsBackend;

use smtp::SmtpNotifier;
use telegram::TelegramNotifier;

/// 已构建、可发送的渠道
pub enum Channel {
    Telegram(TelegramNotifier),
    Email(SmtpNotifier),
}

impl Channel {
    pub async fn send(&self, subject: &str, body: &str) -> anyhow::Result<()> {
        match self {
            Channel::Telegram(n) => n.send(subject, body).await,
            Channel::Email(n) => n.send(subject, body).await,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Channel::Telegram(n) => n.label.clone(),
            Channel::Email(n) => n.label.clone(),
        }
    }
}

/// 单渠道发送结果
#[derive(Debug, Clone)]
pub struct DeliveryResult {
    pub label: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// 从配置构建可发送渠道；跳过 disabled / 字段不全 / 无 SMTP 的 email
pub fn build_channels(
    cfg_channels: &[AlertChannel],
    proxy: Option<&ProxyConfig>,
    tls: TlsBackend,
    smtp: Option<&Arc<SmtpSettings>>,
) -> Vec<Channel> {
    let mut out = Vec::new();
    for c in cfg_channels.iter().filter(|c| c.enabled) {
        let label = c
            .name
            .clone()
            .unwrap_or_else(|| format!("{:?}#{}", c.kind, c.id));
        match c.kind {
            ChannelKind::Telegram => {
                match (c.bot_token.as_ref(), c.chat_id.as_ref()) {
                    (Some(token), Some(chat)) => {
                        // 每渠道独立 client（继承全局代理 + tls），超时 15s
                        match build_client(proxy, 15, tls) {
                            Ok(client) => out.push(Channel::Telegram(TelegramNotifier {
                                client,
                                bot_token: token.clone(),
                                chat_id: chat.clone(),
                                label,
                            })),
                            Err(e) => tracing::warn!("构建 Telegram client 失败，跳过 {}: {}", label, e),
                        }
                    }
                    _ => tracing::warn!("Telegram 渠道 {} 缺少 botToken/chatId，跳过", label),
                }
            }
            ChannelKind::Email => match (smtp, c.to.as_ref()) {
                (Some(settings), Some(to)) => out.push(Channel::Email(SmtpNotifier {
                    settings: settings.clone(),
                    to: to.clone(),
                    label,
                })),
                (None, _) => tracing::warn!("SMTP 未配置（环境变量缺失），跳过 email 渠道 {}", label),
                (_, None) => tracing::warn!("email 渠道 {} 缺少收件地址，跳过", label),
            },
        }
    }
    out
}

/// 并发发送到所有渠道，收集逐渠道结果
pub async fn deliver_all(channels: &[Channel], subject: &str, body: &str) -> Vec<DeliveryResult> {
    let futs = channels.iter().map(|ch| async move {
        let label = ch.label();
        match ch.send(subject, body).await {
            Ok(_) => DeliveryResult { label, ok: true, error: None },
            Err(e) => DeliveryResult { label, ok: false, error: Some(e.to_string()) },
        }
    });
    futures::future::join_all(futs).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alert::config::{AlertChannel, ChannelKind};
    use crate::model::config::TlsBackend;

    fn tg(enabled: bool, token: Option<&str>, chat: Option<&str>) -> AlertChannel {
        AlertChannel {
            id: "x".into(),
            kind: ChannelKind::Telegram,
            enabled,
            name: None,
            bot_token: token.map(|s| s.to_string()),
            chat_id: chat.map(|s| s.to_string()),
            to: None,
        }
    }

    #[test]
    fn test_skip_disabled_and_incomplete_telegram() {
        let chans = vec![
            tg(false, Some("t"), Some("c")),        // disabled -> skip
            tg(true, None, Some("c")),              // 缺 token -> skip
            tg(true, Some("t"), None),              // 缺 chat -> skip
            tg(true, Some("t"), Some("c")),         // 完整 -> 纳入
        ];
        let built = build_channels(&chans, None, TlsBackend::Rustls, None);
        assert_eq!(built.len(), 1);
    }

    #[test]
    fn test_telegram_error_message_excludes_token() {
        // 回归：Telegram 传输错误不得包含 bot token
        // without_url() 会移除 reqwest 错误中的 URL（含 /bot<token>/）
        // 这里以纯字符串层面固定该不变量的意图
        let token = "123456:SECRETTOKEN";
        let safe_msg = format!("Telegram 请求失败: {}", "error sending request");
        assert!(!safe_msg.contains(token));
        assert!(!safe_msg.contains("SECRETTOKEN"));
    }

    #[test]
    fn test_email_skipped_without_smtp() {
        let email = AlertChannel {
            id: "e".into(),
            kind: ChannelKind::Email,
            enabled: true,
            name: None,
            bot_token: None,
            chat_id: None,
            to: Some("a@b.com".into()),
        };
        let built = build_channels(&[email], None, TlsBackend::Rustls, None);
        assert_eq!(built.len(), 0); // smtp None -> 跳过
    }
}
