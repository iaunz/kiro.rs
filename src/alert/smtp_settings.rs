//! 从环境变量读取 SMTP 连接设置

/// SMTP TLS 模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpTls {
    StartTls,
    Implicit,
    None,
}

/// SMTP 连接设置（来自环境变量，启动时读取一次）
#[derive(Debug, Clone)]
pub struct SmtpSettings {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: String,
    pub tls: SmtpTls,
}

pub fn parse_tls(s: &str) -> SmtpTls {
    match s.to_ascii_lowercase().as_str() {
        "implicit" => SmtpTls::Implicit,
        "none" => SmtpTls::None,
        _ => SmtpTls::StartTls,
    }
}

pub fn default_port(tls: SmtpTls) -> u16 {
    match tls {
        SmtpTls::Implicit => 465,
        _ => 587,
    }
}

fn non_empty(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|s| !s.trim().is_empty())
}

impl SmtpSettings {
    /// 从环境变量构建；host/from 缺失或为空时返回 None（视为未配置）
    pub fn from_env() -> Option<SmtpSettings> {
        let host = non_empty("ALERT_SMTP_HOST")?;
        let from = non_empty("ALERT_SMTP_FROM")?;
        let tls = std::env::var("ALERT_SMTP_TLS")
            .ok()
            .map(|s| parse_tls(&s))
            .unwrap_or(SmtpTls::StartTls);
        let port = non_empty("ALERT_SMTP_PORT")
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or_else(|| default_port(tls));
        Some(SmtpSettings {
            host,
            port,
            username: non_empty("ALERT_SMTP_USERNAME"),
            password: non_empty("ALERT_SMTP_PASSWORD"),
            from,
            tls,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tls() {
        assert_eq!(parse_tls("implicit"), SmtpTls::Implicit);
        assert_eq!(parse_tls("IMPLICIT"), SmtpTls::Implicit);
        assert_eq!(parse_tls("none"), SmtpTls::None);
        assert_eq!(parse_tls("starttls"), SmtpTls::StartTls);
        assert_eq!(parse_tls("anything"), SmtpTls::StartTls);
    }

    #[test]
    fn test_default_port_by_tls() {
        assert_eq!(default_port(SmtpTls::Implicit), 465);
        assert_eq!(default_port(SmtpTls::StartTls), 587);
        assert_eq!(default_port(SmtpTls::None), 587);
    }
}
