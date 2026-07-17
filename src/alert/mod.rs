//! Credit 预警子系统

pub mod config;
pub mod notify;
pub mod poller;
pub mod smtp_settings;
pub mod state;

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use futures::StreamExt;
use parking_lot::Mutex;

use crate::http_client::ProxyConfig;
use crate::kiro::token_manager::MultiTokenManager;
use crate::model::config::TlsBackend;

use config::{AlertChannel, AlertConfig};
use notify::{build_channels, deliver_all, DeliveryResult};
use smtp_settings::SmtpSettings;
use state::{decide, fingerprint, AlertState, Decision};

/// 一轮评估的汇总
#[derive(Debug, Clone)]
pub struct EvalSummary {
    pub total: f64,
    pub included: usize,
    pub skipped_error: usize,
    pub skipped_unreportable: usize,
    pub all_failed: bool,
    pub reportable_ids: Vec<u64>,
}

/// 预警服务（协调配置、状态、评估、通知）
pub struct AlertService {
    token_manager: Arc<MultiTokenManager>,
    config: Mutex<AlertConfig>,
    state: Mutex<AlertState>,
    config_path: Option<PathBuf>,
    state_path: Option<PathBuf>,
    proxy: Option<ProxyConfig>,
    tls: TlsBackend,
    smtp: Option<Arc<SmtpSettings>>,
}

impl AlertService {
    pub fn new(
        token_manager: Arc<MultiTokenManager>,
        cache_dir: Option<PathBuf>,
        proxy: Option<ProxyConfig>,
        tls: TlsBackend,
        smtp: Option<SmtpSettings>,
    ) -> Self {
        let config_path = cache_dir.as_ref().map(|d| d.join("alert_config.json"));
        let state_path = cache_dir.as_ref().map(|d| d.join("alert_state.json"));
        let config = config_path
            .as_deref()
            .map(AlertConfig::load)
            .unwrap_or_default();
        let state = state_path
            .as_deref()
            .map(AlertState::load)
            .unwrap_or_default();
        Self {
            token_manager,
            config: Mutex::new(config),
            state: Mutex::new(state),
            config_path,
            state_path,
            proxy,
            tls,
            smtp: smtp.map(Arc::new),
        }
    }

    pub fn config_snapshot(&self) -> AlertConfig {
        self.config.lock().clone()
    }

    pub fn state_snapshot(&self) -> AlertState {
        self.state.lock().clone()
    }

    fn persist_state(&self) {
        if let Some(path) = &self.state_path {
            let snap = self.state.lock().clone();
            if let Err(e) = snap.save(path) {
                tracing::warn!("保存 alert 状态失败: {}", e);
            }
        }
    }

    pub const MASK_PLACEHOLDER: &'static str = "__unchanged__";

    fn persist_config(&self) {
        if let Some(path) = &self.config_path {
            let snap = self.config.lock().clone();
            if let Err(e) = snap.save(path) {
                tracing::warn!("保存 alert 配置失败: {}", e);
            }
        }
    }

    /// 合并被脱敏的密钥字段（占位符表示未改动，保留原值）
    pub fn merge_channel_secrets(mut incoming: AlertChannel, original: &AlertChannel) -> AlertChannel {
        if incoming.bot_token.as_deref() == Some(Self::MASK_PLACEHOLDER) {
            incoming.bot_token = original.bot_token.clone();
        }
        incoming
    }

    pub fn update_config(
        &self,
        enabled: Option<bool>,
        threshold: Option<f64>,
        poll_interval_secs: Option<u64>,
        subject_prefix: Option<Option<String>>,
    ) -> AlertConfig {
        {
            let mut cfg = self.config.lock();
            if let Some(v) = enabled {
                cfg.enabled = v;
            }
            if let Some(v) = threshold {
                cfg.threshold_remaining = v;
            }
            if let Some(v) = poll_interval_secs {
                cfg.poll_interval_secs = v;
            }
            if let Some(v) = subject_prefix {
                cfg.subject_prefix = v.filter(|s| !s.trim().is_empty());
            }
        }
        self.persist_config();
        self.config_snapshot()
    }

    pub fn add_channel(&self, mut ch: AlertChannel) -> AlertChannel {
        if ch.id.trim().is_empty() {
            ch.id = uuid::Uuid::new_v4().to_string();
        }
        self.config.lock().channels.push(ch.clone());
        self.persist_config();
        ch
    }

    pub fn update_channel(&self, id: &str, incoming: AlertChannel) -> anyhow::Result<AlertChannel> {
        let mut cfg = self.config.lock();
        let pos = cfg
            .channels
            .iter()
            .position(|c| c.id == id)
            .ok_or_else(|| anyhow::anyhow!("渠道不存在: {}", id))?;
        let mut merged = Self::merge_channel_secrets(incoming, &cfg.channels[pos]);
        merged.id = id.to_string(); // 保留原 id
        cfg.channels[pos] = merged.clone();
        drop(cfg);
        self.persist_config();
        Ok(merged)
    }

    pub fn delete_channel(&self, id: &str) -> anyhow::Result<()> {
        let mut cfg = self.config.lock();
        let before = cfg.channels.len();
        cfg.channels.retain(|c| c.id != id);
        if cfg.channels.len() == before {
            anyhow::bail!("渠道不存在: {}", id);
        }
        drop(cfg);
        self.persist_config();
        Ok(())
    }

    pub async fn evaluate_now(&self) {
        self.evaluate_once().await;
    }

    /// 只计算总额与计数（不发送、不改状态）
    pub async fn evaluate_summary(&self) -> EvalSummary {
        let snap = self.token_manager.snapshot();
        let reportable: Vec<u64> = snap
            .entries
            .iter()
            .filter(|e| !e.disabled && e.auth_method.as_deref() != Some("api_key"))
            .map(|e| e.id)
            .collect();
        let skipped_unreportable = snap.entries.len() - reportable.len();

        let results = futures::stream::iter(reportable.iter().copied())
            .map(|id| async move {
                self.token_manager
                    .get_usage_limits_for(id)
                    .await
                    .map(|u| (u.usage_limit() - u.current_usage()).max(0.0))
            })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;

        let mut total = 0.0;
        let mut included = 0usize;
        let mut skipped_error = 0usize;
        for r in results {
            match r {
                Ok(rem) => {
                    total += rem;
                    included += 1;
                }
                Err(_) => skipped_error += 1,
            }
        }
        let all_failed = !reportable.is_empty() && included == 0;
        EvalSummary {
            total,
            included,
            skipped_error,
            skipped_unreportable,
            all_failed,
            reportable_ids: reportable,
        }
    }

    /// 构造告警消息（关联函数，便于单测）
    pub fn build_message_parts(cfg: &AlertConfig, summary: &EvalSummary) -> (String, String) {
        let prefix = cfg
            .subject_prefix
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let subject = match prefix {
            Some(p) => format!(
                "⚠️ {} Kiro Credit 预警：剩余 {:.2} 低于阈值 {:.2}",
                p, summary.total, cfg.threshold_remaining
            ),
            None => format!(
                "⚠️ Kiro Credit 预警：剩余 {:.2} 低于阈值 {:.2}",
                summary.total, cfg.threshold_remaining
            ),
        };
        let header = prefix.map(|p| format!("[{}] ", p)).unwrap_or_default();
        let body = format!(
            "{}当前总剩余额度：{:.2}\n预警阈值：{:.2}\n统计凭据：纳入 {} 个，跳过 {} 个（查询失败），排除 {} 个（不可上报）\n时间：{}",
            header,
            summary.total,
            cfg.threshold_remaining,
            summary.included,
            summary.skipped_error,
            summary.skipped_unreportable,
            Utc::now().to_rfc3339(),
        );
        (subject, body)
    }

    pub fn build_message(&self, cfg: &AlertConfig, summary: &EvalSummary) -> (String, String) {
        Self::build_message_parts(cfg, summary)
    }

    /// 发送测试消息到所有启用渠道（忽略状态机）
    pub async fn send_test(&self) -> Vec<DeliveryResult> {
        let cfg = self.config_snapshot();
        let channels = build_channels(&cfg.channels, self.proxy.as_ref(), self.tls, self.smtp.as_ref());
        let subject = {
            let prefix = cfg
                .subject_prefix
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            match prefix {
                Some(p) => format!("✅ {} Kiro Credit 预警测试", p),
                None => "✅ Kiro Credit 预警测试".to_string(),
            }
        };
        deliver_all(&channels, &subject, "这是一条测试消息，配置生效。").await
    }

    /// 核心：评估一轮并按需告警
    pub async fn evaluate_once(&self) {
        let cfg = self.config_snapshot();
        if !cfg.enabled {
            return;
        }

        let summary = self.evaluate_summary().await;
        if summary.all_failed {
            tracing::warn!("本轮所有可上报凭据查询失败，跳过预警评估");
            return;
        }

        // 无可上报凭据，跳过评估（避免总额为 0 触发误报）
        if summary.reportable_ids.is_empty() {
            tracing::debug!("无可上报凭据，跳过本轮预警评估");
            return;
        }

        // 指纹与求和使用同一凭据集，避免中途凭据变动导致不一致
        let fingerprint_now = fingerprint(&summary.reportable_ids);

        let decision = {
            let state = self.state.lock();
            decide(&state, summary.total, cfg.threshold_remaining, &fingerprint_now)
        };

        match decision {
            Decision::Fire => {
                let (subject, body) = self.build_message(&cfg, &summary);
                let channels = build_channels(
                    &cfg.channels,
                    self.proxy.as_ref(),
                    self.tls,
                    self.smtp.as_ref(),
                );
                let results = deliver_all(&channels, &subject, &body).await;
                let any_ok = results.iter().any(|r| r.ok);
                for r in &results {
                    if !r.ok {
                        tracing::warn!("预警发送失败 [{}]: {:?}", r.label, r.error);
                    }
                }
                if any_ok {
                    self.state.lock().fired = true;
                    tracing::info!("已发送 credit 预警，总剩余 {:.2}", summary.total);
                } else {
                    tracing::warn!("所有渠道发送失败，保持 armed 下轮重试");
                }
            }
            Decision::Rearm => {
                self.state.lock().fired = false;
                tracing::info!("credit 恢复，预警已重新 arm");
            }
            Decision::Nothing => {}
        }

        {
            let mut state = self.state.lock();
            state.last_total_remaining = Some(summary.total);
            state.last_evaluated_at = Some(Utc::now().timestamp());
            state.last_threshold = Some(cfg.threshold_remaining);
            state.credential_fingerprint = Some(fingerprint_now);
        }
        self.persist_state();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_message_includes_prefix_and_counts() {
        // 构造一个不依赖 token_manager 的最小 service 仅用于 build_message：
        // build_message 只读 cfg + summary，可用关联函数形式测试。
        let cfg = crate::alert::config::AlertConfig {
            enabled: true,
            threshold_remaining: 1000.0,
            poll_interval_secs: 1800,
            subject_prefix: Some("PROD-东京".to_string()),
            channels: vec![],
        };
        let summary = EvalSummary {
            total: 842.5,
            included: 3,
            skipped_error: 1,
            skipped_unreportable: 2,
            all_failed: false,
            reportable_ids: vec![1, 2, 3],
        };
        let (subject, body) = AlertService::build_message_parts(&cfg, &summary);
        assert!(subject.contains("PROD-东京"));
        assert!(subject.contains("842"));
        assert!(body.contains("1000"));
        assert!(body.contains("纳入 3 个")); // included
        assert!(body.contains("跳过 1 个（查询失败）")); // skipped_error
    }

    #[test]
    fn test_build_message_no_prefix_clean() {
        let cfg = crate::alert::config::AlertConfig {
            enabled: true,
            threshold_remaining: 500.0,
            poll_interval_secs: 1800,
            subject_prefix: None,
            channels: vec![],
        };
        let summary = EvalSummary {
            total: 100.0,
            included: 1,
            skipped_error: 0,
            skipped_unreportable: 0,
            all_failed: false,
            reportable_ids: vec![1],
        };
        let (subject, _) = AlertService::build_message_parts(&cfg, &summary);
        assert!(!subject.contains("  ")); // 无多余双空格
        assert!(subject.contains("Kiro Credit 预警"));
    }

    #[test]
    fn test_merge_masked_token_keeps_original() {
        // 抽出的纯函数：传入新渠道与原渠道，合并被脱敏的字段
        let original = crate::alert::config::AlertChannel {
            id: "1".into(),
            kind: crate::alert::config::ChannelKind::Telegram,
            enabled: true,
            name: None,
            bot_token: Some("REAL-TOKEN".into()),
            chat_id: Some("c".into()),
            to: None,
        };
        let mut incoming = original.clone();
        incoming.bot_token = Some(AlertService::MASK_PLACEHOLDER.to_string());
        let merged = AlertService::merge_channel_secrets(incoming, &original);
        assert_eq!(merged.bot_token.as_deref(), Some("REAL-TOKEN"));
    }

    #[test]
    fn test_merge_real_token_overrides() {
        let original = crate::alert::config::AlertChannel {
            id: "1".into(),
            kind: crate::alert::config::ChannelKind::Telegram,
            enabled: true,
            name: None,
            bot_token: Some("OLD".into()),
            chat_id: Some("c".into()),
            to: None,
        };
        let mut incoming = original.clone();
        incoming.bot_token = Some("NEW".into());
        let merged = AlertService::merge_channel_secrets(incoming, &original);
        assert_eq!(merged.bot_token.as_deref(), Some("NEW"));
    }
}
