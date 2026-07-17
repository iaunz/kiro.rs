//! Telegram 通知器

use reqwest::Client;

/// Telegram sendMessage 通知器
pub struct TelegramNotifier {
    pub client: Client,
    pub bot_token: String,
    pub chat_id: String,
    pub label: String,
}

impl TelegramNotifier {
    pub async fn send(&self, subject: &str, body: &str) -> anyhow::Result<()> {
        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            self.bot_token
        );
        let text = format!("{}\n\n{}", subject, body);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "text": text,
                "parse_mode": "HTML",
            }))
            .send()
            .await
            // without_url() 移除 reqwest 错误中的 URL（含 /bot<token>/），避免 bot token 泄漏到错误日志
            .map_err(|e| anyhow::anyhow!("Telegram 请求失败: {}", e.without_url()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let msg = resp.text().await.unwrap_or_default();
            anyhow::bail!("Telegram 返回 {}: {}", status, msg);
        }
        Ok(())
    }
}
