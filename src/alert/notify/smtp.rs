//! SMTP 邮件通知器

use std::sync::Arc;

use lettre::message::Message;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Tokio1Executor};

use crate::alert::smtp_settings::{SmtpSettings, SmtpTls};

/// SMTP 邮件通知器（每次发送建立一个连接）
pub struct SmtpNotifier {
    pub settings: Arc<SmtpSettings>,
    pub to: String,
    pub label: String,
}

impl SmtpNotifier {
    pub async fn send(&self, subject: &str, body: &str) -> anyhow::Result<()> {
        let email = Message::builder()
            .from(self.settings.from.parse()?)
            .to(self.to.parse()?)
            .subject(subject)
            .body(body.to_string())?;

        let mut builder = match self.settings.tls {
            SmtpTls::Implicit => {
                AsyncSmtpTransport::<Tokio1Executor>::relay(&self.settings.host)?
            }
            SmtpTls::StartTls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.settings.host)?
            }
            SmtpTls::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
                &self.settings.host,
            ),
        };
        builder = builder.port(self.settings.port);
        if let (Some(user), Some(pass)) =
            (&self.settings.username, &self.settings.password)
        {
            builder = builder.credentials(Credentials::new(user.clone(), pass.clone()));
        }
        let transport = builder.build();
        transport.send(email).await?;
        Ok(())
    }
}
