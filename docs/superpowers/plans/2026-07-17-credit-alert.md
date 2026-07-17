# Credit 预警功能 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 后台定时轮询所有可上报凭据的剩余额度，汇总后低于用户设定阈值时通过 Telegram/Email 一次性告警。

**Architecture:** 新增自包含 `src/alert/` 模块，封装配置持久化、arm/fire 状态机、Telegram/SMTP 通知器；由 `main.rs` spawn 一个后台轮询任务，复用现有 `MultiTokenManager::get_usage_limits_for` 取余额。Admin API 新增 `/api/admin/alerts/*` 路由，前端 dashboard 新增「预警设置」区。

**Tech Stack:** Rust (edition 2024, axum 0.8, tokio, parking_lot, reqwest, serde, chrono, uuid, sha2, lettre)；前端 React + TS + tanstack-query + shadcn。

**Design spec:** `docs/superpowers/specs/2026-07-17-credit-alert-design.md`

## Global Constraints

- 无本地 Rust 工具链。编译/测试通过 Docker：`docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo check --release --no-default-features --offline"`。测试用 `cargo test --no-default-features --offline`。
- 前端构建：在 `admin-ui/` 下 `pnpm build`，产物写入 `admin-ui/dist`（rust-embed 编译期读取，必须存在）。
- 所有对外 JSON 序列化字段使用 `#[serde(rename_all = "camelCase")]`（与现有 admin API 一致）。
- 密钥永不明文回传前端：Telegram `botToken` GET 时脱敏（复用 `前4...后4` 风格）；SMTP 密码永不返回。
- 新增依赖固定精确版本（不使用范围）。
- 出站告警复用全局 `config.proxy_url`（Telegram）；SMTP 直连不走代理。
- 代码注释与用户可见文案使用中文（与现有代码库一致）。
- 提交信息以 `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` 结尾。

## File Structure

**新建（后端）：**
- `src/alert/mod.rs` — 模块声明 + `AlertService`（协调器）+ 公开 re-export。
- `src/alert/config.rs` — `AlertConfig`、`AlertChannel`、`ChannelKind`；JSON 加载/保存。
- `src/alert/state.rs` — `AlertState`（持久化运行时状态）+ `decide()` 纯函数状态机 + `Decision`。
- `src/alert/smtp_settings.rs` — 从环境变量读取 `SmtpSettings`。
- `src/alert/notify/mod.rs` — `Notifier` trait + `deliver_all` fan-out。
- `src/alert/notify/telegram.rs` — `TelegramNotifier`。
- `src/alert/notify/smtp.rs` — `SmtpNotifier`。
- `src/alert/poller.rs` — `spawn_poller`（后台 tokio 任务）。
- `src/alert/types.rs` — admin API DTO（脱敏请求/响应）。
- `src/alert/handlers.rs` — alert 相关 axum handler。
- `admin-ui/src/api/alerts.ts` — 前端 API 层。
- `admin-ui/src/hooks/use-alerts.ts` — tanstack-query hooks。
- `admin-ui/src/components/alert-settings.tsx` — 设置卡片 + 状态行。
- `admin-ui/src/components/alert-channel-dialog.tsx` — 渠道增/改对话框。

**修改（后端）：**
- `Cargo.toml` — 新增 `lettre` 依赖。
- `src/main.rs` — 声明 `mod alert`；构建 `AlertService`；spawn poller；注入 alert 路由到 admin router。
- `src/admin/router.rs` — 挂载 alert 路由（或在 main 注入，见 Task 9）。

**修改（前端）：**
- `admin-ui/src/types/api.ts` — 新增 alert 相关类型。
- `admin-ui/src/components/dashboard.tsx` — 引入 `AlertSettings` 区块。

---

### Task 1: Alert 配置类型与 JSON 持久化

**Files:**
- Create: `src/alert/config.rs`
- Create: `src/alert/mod.rs`（本任务仅先声明 `pub mod config;`）

**Interfaces:**
- Produces:
  - `enum ChannelKind { Telegram, Email }`（serde `rename_all = "snake_case"`）
  - `struct AlertChannel { id: String, kind: ChannelKind, enabled: bool, name: Option<String>, bot_token: Option<String>, chat_id: Option<String>, to: Option<String> }`
  - `struct AlertConfig { enabled: bool, threshold_remaining: f64, poll_interval_secs: u64, subject_prefix: Option<String>, channels: Vec<AlertChannel> }`
  - `impl AlertConfig`: `fn default() -> Self`（enabled=false, threshold_remaining=1000.0, poll_interval_secs=1800, subject_prefix=None, channels=vec![]）; `fn load(path: &Path) -> Self`（文件不存在或解析失败返回 default，记录 warning）; `fn save(&self, path: &Path) -> anyhow::Result<()>`（`serde_json::to_string_pretty` 写盘）。

- [ ] **Step 1: 写失败测试**

在 `src/alert/config.rs` 末尾：

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline alert::config"`
Expected: 编译失败（`AlertConfig` 未定义）。

- [ ] **Step 3: 写最小实现**

`src/alert/config.rs` 顶部：

```rust
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
```

`src/alert/mod.rs`：

```rust
//! Credit 预警子系统

pub mod config;
```

- [ ] **Step 4: 在 main.rs 声明模块并运行测试**

在 `src/main.rs` 顶部模块声明处（`mod admin;` 附近）加入 `mod alert;`。

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline alert::config"`
Expected: PASS（3 个测试通过）。

- [ ] **Step 5: 提交**

```bash
git add src/alert/config.rs src/alert/mod.rs src/main.rs
git commit -m "feat(alert): 配置类型与 JSON 持久化

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: 状态机（arm/fire/re-arm/迟滞）

**Files:**
- Create: `src/alert/state.rs`
- Modify: `src/alert/mod.rs`（加 `pub mod state;`）

**Interfaces:**
- Consumes: 无（纯逻辑，输入为参数）。
- Produces:
  - `struct AlertState { fired: bool, last_total_remaining: Option<f64>, last_evaluated_at: Option<i64>, last_threshold: Option<f64>, credential_fingerprint: Option<String> }`（serde camelCase；`load(&Path)->Self` 缺失返回全 None/false；`save(&Path)`）。
  - `fn fingerprint(ids: &[u64]) -> String`：对排序去重后的 id 集合算 sha256 hex。
  - `enum Decision { Fire, Rearm, Nothing }`
  - `fn decide(state: &AlertState, total: f64, threshold: f64, fingerprint_now: &str) -> Decision`：纯函数，实现设计文档决策表 + 迟滞 `max(threshold*0.05, 50.0)` + re-arm 触发（指纹变化 / 恢复 / 阈值变化）。
  - 说明：`decide` 只决定动作，不改状态；调用方按 `Decision` 更新 `AlertState` 字段并持久化。re-arm 触发在 fire 判定之前评估（若触发 re-arm 后仍 `total < threshold`，则本轮返回 `Fire`）。

- [ ] **Step 1: 写失败测试**

`src/alert/state.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> AlertState {
        AlertState::default()
    }

    #[test]
    fn test_fingerprint_stable_and_order_independent() {
        assert_eq!(fingerprint(&[3, 1, 2]), fingerprint(&[1, 2, 3]));
        assert_ne!(fingerprint(&[1, 2]), fingerprint(&[1, 2, 3]));
    }

    #[test]
    fn test_fire_when_armed_and_below() {
        let mut s = base();
        s.last_threshold = Some(1000.0);
        s.credential_fingerprint = Some(fingerprint(&[1]));
        let d = decide(&s, 800.0, 1000.0, &fingerprint(&[1]));
        assert_eq!(d, Decision::Fire);
    }

    #[test]
    fn test_nothing_when_armed_and_above() {
        let mut s = base();
        s.last_threshold = Some(1000.0);
        s.credential_fingerprint = Some(fingerprint(&[1]));
        let d = decide(&s, 1200.0, 1000.0, &fingerprint(&[1]));
        assert_eq!(d, Decision::Nothing);
    }

    #[test]
    fn test_nothing_when_fired_and_still_below() {
        let mut s = base();
        s.fired = true;
        s.last_threshold = Some(1000.0);
        s.credential_fingerprint = Some(fingerprint(&[1]));
        let d = decide(&s, 800.0, 1000.0, &fingerprint(&[1]));
        assert_eq!(d, Decision::Nothing);
    }

    #[test]
    fn test_rearm_on_recovery_above_hysteresis() {
        let mut s = base();
        s.fired = true;
        s.last_threshold = Some(1000.0);
        s.credential_fingerprint = Some(fingerprint(&[1]));
        // 迟滞 = max(1000*0.05, 50) = 50，需 >= 1050 才 re-arm
        let d = decide(&s, 1060.0, 1000.0, &fingerprint(&[1]));
        assert_eq!(d, Decision::Rearm);
    }

    #[test]
    fn test_no_rearm_inside_hysteresis_band() {
        let mut s = base();
        s.fired = true;
        s.last_threshold = Some(1000.0);
        s.credential_fingerprint = Some(fingerprint(&[1]));
        let d = decide(&s, 1020.0, 1000.0, &fingerprint(&[1]));
        assert_eq!(d, Decision::Nothing);
    }

    #[test]
    fn test_new_credential_rearms_and_fires_when_below() {
        let mut s = base();
        s.fired = true;
        s.last_threshold = Some(1000.0);
        s.credential_fingerprint = Some(fingerprint(&[1]));
        // 指纹变化（新增凭据）+ 仍低于阈值 => 重新 Fire
        let d = decide(&s, 800.0, 1000.0, &fingerprint(&[1, 2]));
        assert_eq!(d, Decision::Fire);
    }

    #[test]
    fn test_threshold_change_rearms_and_fires_when_below() {
        let mut s = base();
        s.fired = true;
        s.last_threshold = Some(1000.0);
        s.credential_fingerprint = Some(fingerprint(&[1]));
        // 阈值调高到 2000，total=1500 仍低于新阈值 => 重新 Fire
        let d = decide(&s, 1500.0, 2000.0, &fingerprint(&[1]));
        assert_eq!(d, Decision::Fire);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline alert::state"`
Expected: 编译失败（`decide` 未定义）。

- [ ] **Step 3: 写最小实现**

`src/alert/state.rs`：

```rust
//! Alert 运行时状态与决策状态机

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 持久化的运行时状态
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertState {
    /// 单次告警是否已触发
    #[serde(default)]
    pub fired: bool,
    #[serde(default)]
    pub last_total_remaining: Option<f64>,
    #[serde(default)]
    pub last_evaluated_at: Option<i64>,
    #[serde(default)]
    pub last_threshold: Option<f64>,
    #[serde(default)]
    pub credential_fingerprint: Option<String>,
}

impl AlertState {
    pub fn load(path: &Path) -> Self {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };
        serde_json::from_str(&content).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}

/// 对凭据 id 集合计算指纹（排序去重后 sha256）
pub fn fingerprint(ids: &[u64]) -> String {
    let mut sorted: Vec<u64> = ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let joined = sorted
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut hasher = Sha256::new();
    hasher.update(joined.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 一轮评估的决策结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 触发告警并置 fired=true
    Fire,
    /// 重新 arm（fired=false），不发送
    Rearm,
    /// 无动作
    Nothing,
}

/// 迟滞裕度
fn hysteresis(threshold: f64) -> f64 {
    (threshold * 0.05).max(50.0)
}

/// 决策状态机（纯函数，不修改状态）
pub fn decide(state: &AlertState, total: f64, threshold: f64, fingerprint_now: &str) -> Decision {
    let below = total < threshold;

    // re-arm 触发：新增凭据 / 阈值变化（在 fire 判定之前评估）
    let fingerprint_changed = state
        .credential_fingerprint
        .as_deref()
        .map(|f| f != fingerprint_now)
        .unwrap_or(true);
    let threshold_changed = state
        .last_threshold
        .map(|t| (t - threshold).abs() > f64::EPSILON)
        .unwrap_or(true);

    // 已 fired 时的处理
    if state.fired {
        // 触发 re-arm 条件之一：指纹变化或阈值变化
        if fingerprint_changed || threshold_changed {
            // re-arm 后若仍低于阈值，立即重新 Fire
            return if below { Decision::Fire } else { Decision::Rearm };
        }
        // 恢复到阈值 + 迟滞以上 => re-arm
        if total >= threshold + hysteresis(threshold) {
            return Decision::Rearm;
        }
        return Decision::Nothing;
    }

    // 未 fired：低于阈值则 Fire
    if below {
        Decision::Fire
    } else {
        Decision::Nothing
    }
}
```

在 `src/alert/mod.rs` 加入 `pub mod state;`。

- [ ] **Step 4: 运行测试确认通过**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline alert::state"`
Expected: PASS（全部测试通过）。

- [ ] **Step 5: 提交**

```bash
git add src/alert/state.rs src/alert/mod.rs
git commit -m "feat(alert): arm/fire 状态机与指纹

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---
### Task 3: SMTP 环境变量设置

**Files:**
- Create: `src/alert/smtp_settings.rs`
- Modify: `src/alert/mod.rs`（加 `pub mod smtp_settings;`）

**Interfaces:**
- Produces:
  - `enum SmtpTls { StartTls, Implicit, None }`
  - `struct SmtpSettings { host: String, port: u16, username: Option<String>, password: Option<String>, from: String, tls: SmtpTls }`
  - `fn SmtpSettings::from_env() -> Option<SmtpSettings>`：读取 `ALERT_SMTP_HOST`/`ALERT_SMTP_PORT`/`ALERT_SMTP_USERNAME`/`ALERT_SMTP_PASSWORD`/`ALERT_SMTP_FROM`/`ALERT_SMTP_TLS`。`host` 或 `from` 缺失/空 → 返回 `None`（视为未配置）。`port` 缺省按 tls 推断（implicit=465, 其它=587）。`tls` 解析：`implicit`→Implicit, `none`→None, 其它/缺省→StartTls。
  - `fn parse_tls(s: &str) -> SmtpTls`（辅助，供测试）。

- [ ] **Step 1: 写失败测试**

`src/alert/smtp_settings.rs` 末尾：

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline alert::smtp_settings"`
Expected: 编译失败（`parse_tls` 未定义）。

- [ ] **Step 3: 写最小实现**

`src/alert/smtp_settings.rs`：

```rust
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
```

在 `src/alert/mod.rs` 加入 `pub mod smtp_settings;`。

- [ ] **Step 4: 运行测试确认通过**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline alert::smtp_settings"`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/alert/smtp_settings.rs src/alert/mod.rs
git commit -m "feat(alert): SMTP 环境变量设置

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: 通知器 trait + Telegram + SMTP + fan-out

**Files:**
- Modify: `Cargo.toml`（新增 `lettre`）
- Create: `src/alert/notify/mod.rs`
- Create: `src/alert/notify/telegram.rs`
- Create: `src/alert/notify/smtp.rs`
- Modify: `src/alert/mod.rs`（加 `pub mod notify;`）

**Interfaces:**
- Consumes: `crate::http_client::{ProxyConfig, build_client}`；`crate::model::config::TlsBackend`；`super::config::AlertChannel`；`super::smtp_settings::SmtpSettings`。
- Produces:
  - `trait Notifier { async fn send(&self, subject: &str, body: &str) -> anyhow::Result<()>; fn label(&self) -> String; }`（用 `async_trait`? 见实现说明——用原生 async trait，edition 2024 支持 `impl Trait` in trait 但对象安全需 `Box`；为简化 fan-out，采用枚举分发而非 trait 对象）。
  - 实现方式（避免 dyn async 复杂度）：定义 `enum Channel { Telegram(TelegramNotifier), Email(SmtpNotifier) }`，`impl Channel { async fn send(...); fn label(...) }` 内部 match。
  - `struct TelegramNotifier { client: reqwest::Client, bot_token: String, chat_id: String }`，`async fn send(&self, subject, body)`：POST `https://api.telegram.org/bot{token}/sendMessage`，JSON `{chat_id, text: "{subject}\n\n{body}", parse_mode: "HTML"}`，非 2xx 返回 Err。
  - `struct SmtpNotifier { settings: Arc<SmtpSettings>, to: String }`，`async fn send(...)`：用 lettre 发送。
  - `struct DeliveryResult { label: String, ok: bool, error: Option<String> }`
  - `async fn deliver_all(channels: &[Channel], subject: &str, body: &str) -> Vec<DeliveryResult>`：并发发送，收集逐渠道结果，任一失败不影响其它。
  - `fn build_channels(cfg_channels: &[AlertChannel], proxy: Option<&ProxyConfig>, tls: TlsBackend, smtp: Option<&Arc<SmtpSettings>>) -> Vec<Channel>`：仅纳入 `enabled` 且字段完整的渠道；email 渠道在 smtp 为 None 时跳过并记录 warning。

**实现说明（lettre）：** 使用 `lettre` 精确版本 `0.11.18`，features：`["tokio1-rustls-tls", "smtp-transport", "builder"]`，`default-features = false`。异步发送用 `AsyncSmtpTransport::<Tokio1Executor>`。TLS：Implicit→`::relay(host)`（默认 465 隐式 TLS）；StartTls→`::starttls_relay(host)`；None→`::builder_dangerous(host)`。端口用 `.port(settings.port)`。凭据用 `.credentials(Credentials::new(user, pass))`（仅当 username+password 都存在）。

- [ ] **Step 1: 加依赖**

在 `Cargo.toml` `[dependencies]` 末尾添加：

```toml
lettre = { version = "0.11.18", default-features = false, features = ["tokio1-rustls-tls", "smtp-transport", "builder", "hostname"] }
```

- [ ] **Step 2: 写失败测试**

`src/alert/notify/mod.rs` 末尾（先只测 build_channels 的过滤逻辑，网络发送不在单测覆盖）：

```rust
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
```

- [ ] **Step 3: 运行测试确认失败**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline alert::notify"`
Expected: 编译失败（若 lettre 未在离线缓存中，此步可能因下载失败——见备注）。

> **离线依赖备注：** 新增 `lettre` 首次需联网拉取。按 kiro-rs-docker-build 记忆，用 `docker build --build-arg BUILD_PROXY=socks5h://host.docker.internal:10808` 走代理，或在能联网的 `docker run`（去掉 `--offline`，加 `BUILD_PROXY` 等价的 `ALL_PROXY` 环境变量）下先 `cargo fetch` 填充 `kiro_cargo_registry` 卷，之后再切回 `--offline`。执行者需先完成一次联网 fetch 再继续本任务。

- [ ] **Step 4: 写实现**

`src/alert/notify/telegram.rs`：

```rust
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
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let msg = resp.text().await.unwrap_or_default();
            anyhow::bail!("Telegram 返回 {}: {}", status, msg);
        }
        Ok(())
    }
}
```

`src/alert/notify/smtp.rs`：

```rust
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
```

`src/alert/notify/mod.rs`（顶部）：

```rust
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
```

在 `src/alert/mod.rs` 加入 `pub mod notify;`。

- [ ] **Step 5: 运行测试确认通过 + 编译**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline alert::notify && cargo check --release --no-default-features --offline"`
Expected: 测试 PASS，`cargo check` 通过。

- [ ] **Step 6: 提交**

```bash
git add Cargo.toml Cargo.lock src/alert/notify src/alert/mod.rs
git commit -m "feat(alert): Telegram/SMTP 通知器与 fan-out

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---
### Task 5: AlertService 协调器（evaluate_once + 求和 + 消息）

**Files:**
- Modify: `src/alert/mod.rs`（新增 `AlertService` 及其实现，re-export）

**Interfaces:**
- Consumes: `crate::kiro::token_manager::MultiTokenManager`（`Arc`）；`config::AlertConfig`；`state::{AlertState, Decision, decide, fingerprint}`；`notify::{build_channels, deliver_all, DeliveryResult}`；`smtp_settings::SmtpSettings`；`http_client::ProxyConfig`；`model::config::TlsBackend`。
- Produces（`AlertService` 公开方法）：
  - `fn new(token_manager: Arc<MultiTokenManager>, cache_dir: Option<PathBuf>, proxy: Option<ProxyConfig>, tls: TlsBackend, smtp: Option<SmtpSettings>) -> Self`
  - `fn config_snapshot(&self) -> AlertConfig`（clone 出锁）
  - `fn state_snapshot(&self) -> AlertState`
  - `async fn evaluate_once(&self)`：核心流程（见下）
  - `async fn evaluate_summary(&self) -> EvalSummary`：只算总额与计数，供状态展示与测试（不发送）。
  - `struct EvalSummary { total: f64, included: usize, skipped_error: usize, skipped_unreportable: usize, all_failed: bool }`
  - `fn build_message(&self, cfg: &AlertConfig, summary: &EvalSummary) -> (String, String)`（返回 subject, body）
  - `async fn send_test(&self) -> Vec<DeliveryResult>`（Task 8 用）
  - config/channel 写方法（Task 7 用，签名在 Task 7 定义，本任务先建字段与锁）

  内部字段：`token_manager: Arc<MultiTokenManager>`、`config: Mutex<AlertConfig>`、`state: Mutex<AlertState>`、`config_path: Option<PathBuf>`、`state_path: Option<PathBuf>`、`proxy: Option<ProxyConfig>`、`tls: TlsBackend`、`smtp: Option<Arc<SmtpSettings>>`。

**evaluate_once 流程（严格按设计文档）：**
1. `cfg = config_snapshot()`；若 `!cfg.enabled` 直接 return。
2. 从 `token_manager.snapshot().entries` 取「`!disabled` 且 `auth_method != Some("api_key")`」的 id 列表 = reportable。`skipped_unreportable` = 其余数量。
3. 对每个 reportable id 调 `token_manager.get_usage_limits_for(id)`，并发上限 4（`futures::stream::iter(...).map(...).buffer_unordered(4)`）。成功者 `remaining = (usage.usage_limit() - usage.current_usage()).max(0.0)` 累加到 total，`included += 1`；失败者 `skipped_error += 1`。
4. 若 reportable 非空但 `included == 0`（全失败）→ `all_failed = true`，记录 warning，**跳过决策与状态写入**，return。
5. `fingerprint_now = fingerprint(&reportable_ids)`。
6. `decision = decide(&state_snapshot(), total, cfg.threshold_remaining, &fingerprint_now)`。
7. 依 decision：
   - `Fire`：`build_message` → `build_channels` → `deliver_all`；若至少一个 `ok` 则 `fired=true`，否则保持 `fired`（不消费单次机会），记录 warning。
   - `Rearm`：`fired=false`。
   - `Nothing`：不改 fired。
8. 无论 Fire/Rearm/Nothing（除步骤 4 跳过外）都更新并持久化 `state`：`last_total_remaining=Some(total)`、`last_evaluated_at=Some(Utc::now().timestamp())`、`last_threshold=Some(cfg.threshold_remaining)`、`credential_fingerprint=Some(fingerprint_now)`，以及上面对 `fired` 的修改。

- [ ] **Step 1: 写失败测试**

在 `src/alert/mod.rs` 末尾新增测试模块（只测 `build_message` 纯逻辑，避免网络）：

```rust
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
        };
        let (subject, body) = AlertService::build_message_parts(&cfg, &summary);
        assert!(subject.contains("PROD-东京"));
        assert!(subject.contains("842"));
        assert!(body.contains("1000"));
        assert!(body.contains("3"));   // included
        assert!(body.contains("跳过") || body.contains("1")); // skipped_error
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
        };
        let (subject, _) = AlertService::build_message_parts(&cfg, &summary);
        assert!(!subject.contains("  ")); // 无多余双空格
        assert!(subject.contains("Kiro Credit 预警"));
    }
}
```

> 说明：把消息构造实现为**关联函数** `AlertService::build_message_parts(cfg, summary) -> (String, String)`（不借用 `self`），实例方法 `build_message` 内部转调它，这样单测无需构造 `token_manager`。

- [ ] **Step 2: 运行测试确认失败**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline alert::tests"`
Expected: 编译失败（`AlertService` 未定义）。

- [ ] **Step 3: 写实现**

在 `src/alert/mod.rs` 顶部补充模块声明与 imports，然后加入 `AlertService`：

```rust
//! Credit 预警子系统

pub mod config;
pub mod notify;
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

use config::AlertConfig;
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

        // 重新收集 reportable 指纹（evaluate_summary 未返回 id 列表，这里独立计算）
        let snap = self.token_manager.snapshot();
        let reportable_ids: Vec<u64> = snap
            .entries
            .iter()
            .filter(|e| !e.disabled && e.auth_method.as_deref() != Some("api_key"))
            .map(|e| e.id)
            .collect();
        let fingerprint_now = fingerprint(&reportable_ids);

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
```

- [ ] **Step 4: 运行测试确认通过 + 编译**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline alert:: && cargo check --release --no-default-features --offline"`
Expected: 测试 PASS，`cargo check` 通过。

- [ ] **Step 5: 提交**

```bash
git add src/alert/mod.rs
git commit -m "feat(alert): AlertService 协调器与 evaluate_once

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---
### Task 6: 后台轮询任务

**Files:**
- Create: `src/alert/poller.rs`
- Modify: `src/alert/mod.rs`（加 `pub mod poller;`）

**Interfaces:**
- Consumes: `AlertService`（`Arc`）。
- Produces: `fn spawn_poller(service: Arc<AlertService>)`：`tokio::spawn` 一个循环任务。首轮在启动后等待一个（base+jitter）间隔再评估（不在启动瞬间告警）。每轮：读 `config_snapshot()` 取 `poll_interval_secs` 作为 base，`jitter = fastrand::u64(300..=600)` 秒，`sleep(base + jitter)`，若 `enabled` 则 `evaluate_once().await`。

- [ ] **Step 1: 写实现（本任务无单测，逻辑为时间循环；由 Task 12 集成验证）**

`src/alert/poller.rs`：

```rust
//! 后台轮询任务

use std::sync::Arc;
use std::time::Duration;

use super::AlertService;

/// 启动后台轮询任务。首轮在一个间隔之后才评估。
pub fn spawn_poller(service: Arc<AlertService>) {
    tokio::spawn(async move {
        loop {
            let base = service.config_snapshot().poll_interval_secs;
            let jitter = fastrand::u64(300..=600); // 5-10 分钟
            tokio::time::sleep(Duration::from_secs(base + jitter)).await;

            if service.config_snapshot().enabled {
                service.evaluate_once().await;
            }
        }
    });
    tracing::info!("Credit 预警后台轮询已启动");
}
```

在 `src/alert/mod.rs` 加入 `pub mod poller;`。

- [ ] **Step 2: 编译**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo check --release --no-default-features --offline"`
Expected: 通过。

- [ ] **Step 3: 提交**

```bash
git add src/alert/poller.rs src/alert/mod.rs
git commit -m "feat(alert): 后台轮询任务

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 7: 配置与渠道写方法（含即时重评估）

**Files:**
- Modify: `src/alert/mod.rs`（在 `impl AlertService` 内新增写方法）

**Interfaces:**
- Produces（`impl AlertService`）：
  - `fn update_config(&self, enabled: Option<bool>, threshold: Option<f64>, poll_interval_secs: Option<u64>, subject_prefix: Option<Option<String>>) -> AlertConfig`：部分更新（None=不改；`subject_prefix` 用 `Option<Option<String>>` 区分「不改」与「清空」），持久化 config，返回新 config。
  - `fn add_channel(&self, ch: AlertChannel) -> AlertChannel`：若 `ch.id` 为空则生成 uuid v4，push，持久化，返回存入的渠道。
  - `fn update_channel(&self, id: &str, ch: AlertChannel) -> anyhow::Result<AlertChannel>`：按 id 替换（保留原 id）；若传入的 `bot_token` 等于脱敏占位符 `MASK_PLACEHOLDER` 则保留原 token（不覆盖）。找不到返回 Err。
  - `fn delete_channel(&self, id: &str) -> anyhow::Result<()>`：删除；找不到返回 Err。
  - `const MASK_PLACEHOLDER: &str = "__unchanged__";`
  - `async fn evaluate_now(&self)`：`evaluate_once` 的公开别名，供 config 保存后立即触发（handler spawn 调用）。
  - 注：`update_config` 修改阈值后立即 re-arm 由状态机 `decide` 的「阈值变化」分支覆盖，无需在此显式改 state；保存后由 handler 触发一次 `evaluate_now`。

- [ ] **Step 1: 写失败测试**

在 `src/alert/mod.rs` 的 `mod tests` 中追加（构造需要 token_manager 的 service——用一个空 manager 辅助构造函数；若构造成本高，仅测不依赖 manager 的 mask 合并逻辑，将其抽为关联函数）：

```rust
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
```

- [ ] **Step 2: 运行确认失败**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline alert::tests::test_merge"`
Expected: 编译失败（`merge_channel_secrets` 未定义）。

- [ ] **Step 3: 写实现**

在 `src/alert/mod.rs` 顶部 imports 增加 `use config::{AlertChannel};`（若尚未引入）。在 `impl AlertService` 内新增：

```rust
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
```

- [ ] **Step 4: 运行确认通过**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline alert:: && cargo check --release --no-default-features --offline"`
Expected: PASS + check 通过。

- [ ] **Step 5: 提交**

```bash
git add src/alert/mod.rs
git commit -m "feat(alert): 配置/渠道写方法与密钥合并

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 8: Admin API DTO + handlers + 路由

**Files:**
- Create: `src/alert/types.rs`
- Create: `src/alert/handlers.rs`
- Modify: `src/alert/mod.rs`（加 `pub mod types; pub mod handlers;`）

**Interfaces:**
- Consumes: `axum::{Json, extract::{Path, State}, response::IntoResponse}`；`crate::admin::AdminState`（handler 复用 admin 的 state 与 auth）；`AlertService`（经 `AdminState.service` 无法获取——见下方「状态获取」说明）。
- **状态获取说明：** alert handler 需要 `Arc<AlertService>`。由于 admin 的 `AdminState` 只持有 `AdminService`，本任务让 alert 路由使用**独立的 `AlertState`**（`#[derive(Clone)] struct AlertState { admin_api_key: String, service: Arc<AlertService> }`），复用同一个 admin auth 中间件逻辑。为避免重复 auth 代码，新增 `src/alert/handlers.rs` 内一个轻量 auth 中间件（复用 `crate::common::auth`）。
- Produces（`src/alert/types.rs`，全部 camelCase）：
  - `struct AlertConfigResponse { enabled: bool, threshold_remaining: f64, poll_interval_secs: u64, subject_prefix: Option<String>, channels: Vec<ChannelResponse>, smtp_configured: bool }`
  - `struct ChannelResponse { id: String, kind: ChannelKind, enabled: bool, name: Option<String>, masked_bot_token: Option<String>, chat_id: Option<String>, to: Option<String> }`（telegram 返回 `masked_bot_token`，绝不返回明文）
  - `struct UpdateConfigRequest { enabled: Option<bool>, threshold_remaining: Option<f64>, poll_interval_secs: Option<u64>, subject_prefix: Option<String> }`
  - `struct ChannelRequest { kind: ChannelKind, enabled: Option<bool>, name: Option<String>, bot_token: Option<String>, chat_id: Option<String>, to: Option<String> }`
  - `struct StatusResponse { fired: bool, last_total_remaining: Option<f64>, last_evaluated_at: Option<i64>, last_threshold: Option<f64> }`
  - `struct TestResponse { results: Vec<TestChannelResult> }`、`struct TestChannelResult { label: String, ok: bool, error: Option<String> }`
  - `fn mask_token(token: &str) -> String`：`前6...后2`，长度不足回退 `***`（telegram token 形如 `123456:AA..`，保留 bot id 段前缀便于辨识）。
  - `fn AlertConfig -> AlertConfigResponse` 转换（含 `smtp_configured` 由 handler 传入）。

- [ ] **Step 1: 写失败测试（mask_token）**

`src/alert/types.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_token() {
        assert_eq!(mask_token("1234567890:ABCDEFGH"), "123456...GH");
        assert_eq!(mask_token("short"), "***");
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline alert::types"`
Expected: 编译失败。

- [ ] **Step 3: 写 types.rs**

```rust
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
```

- [ ] **Step 4: 写 handlers.rs（含独立 AlertState + auth 中间件 + 路由）**

```rust
//! Alert Admin API handlers、状态与路由

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};

use crate::common::auth;

use super::config::AlertChannel;
use super::types::{
    AlertConfigResponse, ChannelRequest, StatusResponse, TestChannelResult, TestResponse,
    UpdateConfigRequest,
};
use super::AlertService;

/// Alert 路由共享状态
#[derive(Clone)]
pub struct AlertState {
    pub admin_api_key: String,
    pub service: Arc<AlertService>,
    pub smtp_configured: bool,
}

async fn alert_auth_middleware(
    State(state): State<AlertState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    match auth::extract_api_key(&request) {
        Some(key) if auth::constant_time_eq(&key, &state.admin_api_key) => next.run(request).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": {"type": "authentication_error", "message": "Invalid or missing admin API key"}})),
        )
            .into_response(),
    }
}

/// GET /alerts/config
async fn get_config(State(state): State<AlertState>) -> impl IntoResponse {
    let cfg = state.service.config_snapshot();
    Json(AlertConfigResponse::from_config(&cfg, state.smtp_configured))
}

/// PUT /alerts/config
async fn put_config(
    State(state): State<AlertState>,
    Json(req): Json<UpdateConfigRequest>,
) -> impl IntoResponse {
    let subject_prefix = Some(req.subject_prefix); // Some(None)=清空, Some(Some(x))=设置
    state.service.update_config(
        req.enabled,
        req.threshold_remaining,
        req.poll_interval_secs,
        subject_prefix,
    );
    // 保存后立即重评估（阈值变化即时生效），不阻塞响应
    let svc = state.service.clone();
    tokio::spawn(async move { svc.evaluate_now().await });
    let cfg = state.service.config_snapshot();
    Json(AlertConfigResponse::from_config(&cfg, state.smtp_configured))
}

/// GET /alerts/status
async fn get_status(State(state): State<AlertState>) -> impl IntoResponse {
    let s = state.service.state_snapshot();
    Json(StatusResponse {
        fired: s.fired,
        last_total_remaining: s.last_total_remaining,
        last_evaluated_at: s.last_evaluated_at,
        last_threshold: s.last_threshold,
    })
}

/// POST /alerts/channels
async fn create_channel(
    State(state): State<AlertState>,
    Json(req): Json<ChannelRequest>,
) -> impl IntoResponse {
    let ch = AlertChannel {
        id: String::new(),
        kind: req.kind,
        enabled: req.enabled.unwrap_or(true),
        name: req.name,
        bot_token: req.bot_token,
        chat_id: req.chat_id,
        to: req.to,
    };
    let saved = state.service.add_channel(ch);
    let cfg = state.service.config_snapshot();
    let _ = saved;
    (StatusCode::CREATED, Json(AlertConfigResponse::from_config(&cfg, state.smtp_configured))).into_response()
}

/// PUT /alerts/channels/{id}
async fn update_channel(
    State(state): State<AlertState>,
    Path(id): Path<String>,
    Json(req): Json<ChannelRequest>,
) -> impl IntoResponse {
    let ch = AlertChannel {
        id: id.clone(),
        kind: req.kind,
        enabled: req.enabled.unwrap_or(true),
        name: req.name,
        bot_token: req.bot_token,
        chat_id: req.chat_id,
        to: req.to,
    };
    match state.service.update_channel(&id, ch) {
        Ok(_) => {
            let cfg = state.service.config_snapshot();
            Json(AlertConfigResponse::from_config(&cfg, state.smtp_configured)).into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": {"type": "not_found", "message": e.to_string()}})),
        )
            .into_response(),
    }
}

/// DELETE /alerts/channels/{id}
async fn delete_channel(
    State(state): State<AlertState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.service.delete_channel(&id) {
        Ok(_) => {
            let cfg = state.service.config_snapshot();
            Json(AlertConfigResponse::from_config(&cfg, state.smtp_configured)).into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": {"type": "not_found", "message": e.to_string()}})),
        )
            .into_response(),
    }
}

/// POST /alerts/test
async fn test_alert(State(state): State<AlertState>) -> impl IntoResponse {
    let results = state.service.send_test().await;
    Json(TestResponse {
        results: results
            .into_iter()
            .map(|r| TestChannelResult { label: r.label, ok: r.ok, error: r.error })
            .collect(),
    })
}

/// 创建 alert 路由
pub fn create_alert_router(state: AlertState) -> Router {
    Router::new()
        .route("/config", get(get_config).put(put_config))
        .route("/status", get(get_status))
        .route("/channels", post(create_channel))
        .route(
            "/channels/{id}",
            axum::routing::put(update_channel).delete(delete_channel),
        )
        .route("/test", post(test_alert))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            alert_auth_middleware,
        ))
        .with_state(state)
}
```

在 `src/alert/mod.rs` 加入 `pub mod types;` 和 `pub mod handlers;`，并 `pub use handlers::{create_alert_router, AlertState as AlertRouterState};`。

- [ ] **Step 5: 运行确认通过 + 编译**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline alert:: && cargo check --release --no-default-features --offline"`
Expected: PASS + check 通过。

- [ ] **Step 6: 提交**

```bash
git add src/alert/types.rs src/alert/handlers.rs src/alert/mod.rs
git commit -m "feat(alert): Admin API DTO、handlers 与路由

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 9: main.rs 接线

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `alert::{AlertService, spawn_poller, create_alert_router, AlertRouterState}`；`alert::smtp_settings::SmtpSettings`。
- 在 admin API 启用分支内（`main.rs` 现有 `let admin_service = ...` 附近）：构建 `AlertService`，spawn poller，创建 alert 路由并 `.nest("/api/admin/alerts", alert_app)`。

- [ ] **Step 1: 修改 main.rs**

在 admin 启用分支（`if admin_key.trim().is_empty() { ... } else { ... }` 的 else 块内，`anthropic_app.nest("/api/admin", admin_app)...` 之前）插入：

```rust
            // 构建 Credit 预警服务
            let smtp_settings = crate::alert::smtp_settings::SmtpSettings::from_env();
            let smtp_configured = smtp_settings.is_some();
            let alert_service = std::sync::Arc::new(alert::AlertService::new(
                token_manager.clone(),
                token_manager.cache_dir(),
                proxy_config.clone(),
                config.tls_backend,
                smtp_settings,
            ));
            alert::spawn_poller(alert_service.clone());
            let alert_state = alert::AlertRouterState {
                admin_api_key: admin_key.clone(),
                service: alert_service,
                smtp_configured,
            };
            let alert_app = alert::create_alert_router(alert_state);
            tracing::info!("Credit 预警 API 已启用: /api/admin/alerts");
```

> 注意 `proxy_config` 在 `main.rs` 上文已 `clone()` 过多次；此处再 `.clone()`。若此前所有权已被 move，改用在构建前保留一个副本。确认 `proxy_config` 类型为 `Option<ProxyConfig>`（可 Clone）。

然后把现有：

```rust
            anthropic_app
                .nest("/api/admin", admin_app)
                .nest("/admin", admin_ui_app)
```

改为：

```rust
            anthropic_app
                .nest("/api/admin", admin_app)
                .nest("/api/admin/alerts", alert_app)
                .nest("/admin", admin_ui_app)
```

在启动日志段（`if admin_key_valid { ... }`）追加：

```rust
        tracing::info!("  GET  /api/admin/alerts/config");
        tracing::info!("  PUT  /api/admin/alerts/config");
        tracing::info!("  GET  /api/admin/alerts/status");
```

在 `mod.rs` 需确保 `pub use` 暴露 `AlertService`、`spawn_poller`、`create_alert_router`、`AlertRouterState`。在 `src/alert/mod.rs` 顶部补：

```rust
pub use handlers::{create_alert_router, AlertState as AlertRouterState};
pub use poller::spawn_poller;
// AlertService 已在本文件定义，无需 re-export
```

> **嵌套顺序说明：** axum 中 `/api/admin` 与 `/api/admin/alerts` 是不同前缀，先 nest 谁都可以，router 按最长前缀匹配。若出现冲突，将 `/api/admin/alerts` 放在 `/api/admin` 之前 nest。

- [ ] **Step 2: 编译**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo check --release --no-default-features --offline"`
Expected: 通过。

- [ ] **Step 3: 提交**

```bash
git add src/main.rs src/alert/mod.rs
git commit -m "feat(alert): 接入 main（服务、poller、路由）

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---
### Task 10: 前端类型 + API 层 + hooks

**Files:**
- Modify: `admin-ui/src/types/api.ts`（追加 alert 类型）
- Create: `admin-ui/src/api/alerts.ts`
- Create: `admin-ui/src/hooks/use-alerts.ts`

**Interfaces:**
- Produces（`types/api.ts`）：
  - `type AlertChannelKind = 'telegram' | 'email'`
  - `interface AlertChannelResponse { id: string; kind: AlertChannelKind; enabled: boolean; name?: string; maskedBotToken?: string; chatId?: string; to?: string }`
  - `interface AlertConfigResponse { enabled: boolean; thresholdRemaining: number; pollIntervalSecs: number; subjectPrefix?: string; channels: AlertChannelResponse[]; smtpConfigured: boolean }`
  - `interface UpdateAlertConfigRequest { enabled?: boolean; thresholdRemaining?: number; pollIntervalSecs?: number; subjectPrefix?: string }`
  - `interface AlertChannelRequest { kind: AlertChannelKind; enabled?: boolean; name?: string; botToken?: string; chatId?: string; to?: string }`
  - `interface AlertStatusResponse { fired: boolean; lastTotalRemaining?: number; lastEvaluatedAt?: number; lastThreshold?: number }`
- Produces（`api/alerts.ts`）：`getAlertConfig()`, `updateAlertConfig(req)`, `getAlertStatus()`, `createAlertChannel(req)`, `updateAlertChannel(id, req)`, `deleteAlertChannel(id)`（均经 `/api/admin` axios 实例）。
- Produces（`use-alerts.ts`）：`useAlertConfig()`, `useAlertStatus()`, `useUpdateAlertConfig()`, `useCreateAlertChannel()`, `useUpdateAlertChannel()`, `useDeleteAlertChannel()`（写操作 onSuccess 使 `['alert-config']` 失效）。

- [ ] **Step 1: 追加类型到 `admin-ui/src/types/api.ts`（文件末尾）**

```ts
// ============ Credit 预警 ============

export type AlertChannelKind = 'telegram' | 'email'

export interface AlertChannelResponse {
  id: string
  kind: AlertChannelKind
  enabled: boolean
  name?: string
  maskedBotToken?: string
  chatId?: string
  to?: string
}

export interface AlertConfigResponse {
  enabled: boolean
  thresholdRemaining: number
  pollIntervalSecs: number
  subjectPrefix?: string
  channels: AlertChannelResponse[]
  smtpConfigured: boolean
}

export interface UpdateAlertConfigRequest {
  enabled?: boolean
  thresholdRemaining?: number
  pollIntervalSecs?: number
  subjectPrefix?: string
}

export interface AlertChannelRequest {
  kind: AlertChannelKind
  enabled?: boolean
  name?: string
  botToken?: string
  chatId?: string
  to?: string
}

export interface AlertStatusResponse {
  fired: boolean
  lastTotalRemaining?: number
  lastEvaluatedAt?: number
  lastThreshold?: number
}
```

- [ ] **Step 2: 创建 `admin-ui/src/api/alerts.ts`**

复用 credentials.ts 里同款 axios 实例配置（相同 baseURL/拦截器）。为避免重复实例，import 现有的不方便（未导出），故本文件内新建同款实例：

```ts
import axios from 'axios'
import { storage } from '@/lib/storage'
import type {
  AlertConfigResponse,
  UpdateAlertConfigRequest,
  AlertChannelRequest,
  AlertStatusResponse,
} from '@/types/api'

const api = axios.create({
  baseURL: '/api/admin',
  headers: { 'Content-Type': 'application/json' },
})

api.interceptors.request.use((config) => {
  const apiKey = storage.getApiKey()
  if (apiKey) {
    config.headers['x-api-key'] = apiKey
  }
  return config
})

export async function getAlertConfig(): Promise<AlertConfigResponse> {
  const { data } = await api.get<AlertConfigResponse>('/alerts/config')
  return data
}

export async function updateAlertConfig(
  req: UpdateAlertConfigRequest
): Promise<AlertConfigResponse> {
  const { data } = await api.put<AlertConfigResponse>('/alerts/config', req)
  return data
}

export async function getAlertStatus(): Promise<AlertStatusResponse> {
  const { data } = await api.get<AlertStatusResponse>('/alerts/status')
  return data
}

export async function createAlertChannel(
  req: AlertChannelRequest
): Promise<AlertConfigResponse> {
  const { data } = await api.post<AlertConfigResponse>('/alerts/channels', req)
  return data
}

export async function updateAlertChannel(
  id: string,
  req: AlertChannelRequest
): Promise<AlertConfigResponse> {
  const { data } = await api.put<AlertConfigResponse>(`/alerts/channels/${id}`, req)
  return data
}

export async function deleteAlertChannel(id: string): Promise<AlertConfigResponse> {
  const { data } = await api.delete<AlertConfigResponse>(`/alerts/channels/${id}`)
  return data
}
```

- [ ] **Step 3: 创建 `admin-ui/src/hooks/use-alerts.ts`**

```ts
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  getAlertConfig,
  updateAlertConfig,
  getAlertStatus,
  createAlertChannel,
  updateAlertChannel,
  deleteAlertChannel,
} from '@/api/alerts'
import type { AlertChannelRequest, UpdateAlertConfigRequest } from '@/types/api'

export function useAlertConfig() {
  return useQuery({ queryKey: ['alert-config'], queryFn: getAlertConfig })
}

export function useAlertStatus() {
  return useQuery({
    queryKey: ['alert-status'],
    queryFn: getAlertStatus,
    refetchInterval: 60000,
  })
}

export function useUpdateAlertConfig() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (req: UpdateAlertConfigRequest) => updateAlertConfig(req),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['alert-config'] }),
  })
}

export function useCreateAlertChannel() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (req: AlertChannelRequest) => createAlertChannel(req),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['alert-config'] }),
  })
}

export function useUpdateAlertChannel() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ id, req }: { id: string; req: AlertChannelRequest }) =>
      updateAlertChannel(id, req),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['alert-config'] }),
  })
}

export function useDeleteAlertChannel() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => deleteAlertChannel(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['alert-config'] }),
  })
}
```

- [ ] **Step 4: 构建校验（类型编译）**

Run: 在 `admin-ui/` 下 `pnpm build`（若无本地 node，用 docker：`docker run --rm -v "D:\dev\kiro.rs\admin-ui:/app" -w /app node:22-alpine sh -c "corepack enable && pnpm install --frozen-lockfile && pnpm build"`）。
Expected: TypeScript 编译通过（暂无组件引用新 hook 也可通过）。

- [ ] **Step 5: 提交**

```bash
git add admin-ui/src/types/api.ts admin-ui/src/api/alerts.ts admin-ui/src/hooks/use-alerts.ts
git commit -m "feat(alert-ui): 前端类型、API 层与 hooks

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 11: 前端设置组件 + 渠道对话框 + 接入 dashboard

**Files:**
- Create: `admin-ui/src/components/alert-channel-dialog.tsx`
- Create: `admin-ui/src/components/alert-settings.tsx`
- Modify: `admin-ui/src/components/dashboard.tsx`（在主内容区渲染 `<AlertSettings />`）

**Interfaces:**
- Consumes: Task 10 的 hooks 与类型；现有 ui 组件（`Card`, `Button`, `Input`, `Switch`, `Dialog`, `Badge`）；`toast`（sonner）。
- `AlertChannelDialog` props：`{ open: boolean; onOpenChange: (o: boolean) => void; channel: AlertChannelResponse | null }`（channel=null 表示新增）。表单字段随 `kind` 切换：telegram 显示 name/botToken/chatId，email 显示 name/to。编辑 telegram 时 `botToken` 输入框 placeholder 显示当前 `maskedBotToken`，留空提交时传占位符 `__unchanged__` 以保留原 token。
- `AlertSettings`：渲染设置卡片（enable Switch、阈值 Input、轮询间隔 Input（秒）、subjectPrefix Input、保存按钮）、状态行（`useAlertStatus`：显示总剩余/上次检查时间/`fired?已触发:已就绪` Badge、SMTP 状态）、渠道列表（每项 name/kind/masked、编辑/删除按钮、新增按钮）。**不含「发送测试」按钮。**

- [ ] **Step 1: 创建 `alert-channel-dialog.tsx`**

```tsx
import { useState, useEffect } from 'react'
import {
  Dialog, DialogContent, DialogHeader, DialogTitle, DialogFooter,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { toast } from 'sonner'
import { useCreateAlertChannel, useUpdateAlertChannel } from '@/hooks/use-alerts'
import type { AlertChannelResponse, AlertChannelKind } from '@/types/api'

const MASK_PLACEHOLDER = '__unchanged__'

interface Props {
  open: boolean
  onOpenChange: (o: boolean) => void
  channel: AlertChannelResponse | null
}

export function AlertChannelDialog({ open, onOpenChange, channel }: Props) {
  const isEdit = channel !== null
  const [kind, setKind] = useState<AlertChannelKind>('telegram')
  const [name, setName] = useState('')
  const [enabled, setEnabled] = useState(true)
  const [botToken, setBotToken] = useState('')
  const [chatId, setChatId] = useState('')
  const [to, setTo] = useState('')

  const create = useCreateAlertChannel()
  const update = useUpdateAlertChannel()

  useEffect(() => {
    if (open) {
      setKind(channel?.kind ?? 'telegram')
      setName(channel?.name ?? '')
      setEnabled(channel?.enabled ?? true)
      setBotToken('') // 编辑时留空 = 不改
      setChatId(channel?.chatId ?? '')
      setTo(channel?.to ?? '')
    }
  }, [open, channel])

  const handleSubmit = async () => {
    const req = {
      kind,
      enabled,
      name: name || undefined,
      chatId: kind === 'telegram' ? chatId || undefined : undefined,
      to: kind === 'email' ? to || undefined : undefined,
      botToken:
        kind === 'telegram'
          ? (botToken || (isEdit ? MASK_PLACEHOLDER : undefined))
          : undefined,
    }
    try {
      if (isEdit && channel) {
        await update.mutateAsync({ id: channel.id, req })
      } else {
        await create.mutateAsync(req)
      }
      toast.success(isEdit ? '渠道已更新' : '渠道已添加')
      onOpenChange(false)
    } catch (e) {
      toast.error('保存失败')
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{isEdit ? '编辑渠道' : '添加渠道'}</DialogTitle>
        </DialogHeader>
        <div className="space-y-3">
          <div className="flex gap-2">
            <Button
              variant={kind === 'telegram' ? 'default' : 'outline'}
              size="sm"
              onClick={() => setKind('telegram')}
              disabled={isEdit}
            >
              Telegram
            </Button>
            <Button
              variant={kind === 'email' ? 'default' : 'outline'}
              size="sm"
              onClick={() => setKind('email')}
              disabled={isEdit}
            >
              Email
            </Button>
          </div>
          <Input placeholder="名称（可选）" value={name} onChange={(e) => setName(e.target.value)} />
          {kind === 'telegram' ? (
            <>
              <Input
                placeholder={channel?.maskedBotToken ? `当前: ${channel.maskedBotToken}（留空不改）` : 'Bot Token'}
                value={botToken}
                onChange={(e) => setBotToken(e.target.value)}
              />
              <Input placeholder="Chat ID" value={chatId} onChange={(e) => setChatId(e.target.value)} />
            </>
          ) : (
            <Input placeholder="收件邮箱" value={to} onChange={(e) => setTo(e.target.value)} />
          )}
          <div className="flex items-center gap-2">
            <Switch checked={enabled} onCheckedChange={setEnabled} />
            <span className="text-sm">启用</span>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>取消</Button>
          <Button onClick={handleSubmit} disabled={create.isPending || update.isPending}>
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
```

- [ ] **Step 2: 创建 `alert-settings.tsx`**

```tsx
import { useState, useEffect } from 'react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { Badge } from '@/components/ui/badge'
import { toast } from 'sonner'
import { Plus, Pencil, Trash2 } from 'lucide-react'
import {
  useAlertConfig, useAlertStatus, useUpdateAlertConfig, useDeleteAlertChannel,
} from '@/hooks/use-alerts'
import { AlertChannelDialog } from '@/components/alert-channel-dialog'
import type { AlertChannelResponse } from '@/types/api'

export function AlertSettings() {
  const { data: config } = useAlertConfig()
  const { data: status } = useAlertStatus()
  const updateConfig = useUpdateAlertConfig()
  const deleteChannel = useDeleteAlertChannel()

  const [enabled, setEnabled] = useState(false)
  const [threshold, setThreshold] = useState('1000')
  const [pollSecs, setPollSecs] = useState('1800')
  const [prefix, setPrefix] = useState('')
  const [dialogOpen, setDialogOpen] = useState(false)
  const [editing, setEditing] = useState<AlertChannelResponse | null>(null)

  useEffect(() => {
    if (config) {
      setEnabled(config.enabled)
      setThreshold(String(config.thresholdRemaining))
      setPollSecs(String(config.pollIntervalSecs))
      setPrefix(config.subjectPrefix ?? '')
    }
  }, [config])

  const handleSave = async () => {
    try {
      await updateConfig.mutateAsync({
        enabled,
        thresholdRemaining: Number(threshold),
        pollIntervalSecs: Number(pollSecs),
        subjectPrefix: prefix,
      })
      toast.success('预警设置已保存')
    } catch {
      toast.error('保存失败')
    }
  }

  const handleDelete = async (id: string) => {
    try {
      await deleteChannel.mutateAsync(id)
      toast.success('渠道已删除')
    } catch {
      toast.error('删除失败')
    }
  }

  const fmtTime = (ts?: number) =>
    ts ? new Date(ts * 1000).toLocaleString('zh-CN') : '尚未检查'

  return (
    <Card className="mb-6">
      <CardHeader>
        <CardTitle className="flex items-center justify-between">
          <span>Credit 预警设置</span>
          {status && (
            <Badge variant={status.fired ? 'destructive' : 'secondary'}>
              {status.fired ? '已触发' : '已就绪'}
            </Badge>
          )}
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex items-center gap-2">
          <Switch checked={enabled} onCheckedChange={setEnabled} />
          <span className="text-sm">启用预警</span>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <div>
            <label className="text-sm text-muted-foreground">阈值（总剩余低于）</label>
            <Input value={threshold} onChange={(e) => setThreshold(e.target.value)} />
          </div>
          <div>
            <label className="text-sm text-muted-foreground">轮询间隔（秒）</label>
            <Input value={pollSecs} onChange={(e) => setPollSecs(e.target.value)} />
          </div>
        </div>
        <div>
          <label className="text-sm text-muted-foreground">主题前缀（区分实例，可选）</label>
          <Input value={prefix} onChange={(e) => setPrefix(e.target.value)} />
        </div>
        <div className="text-sm text-muted-foreground">
          上次总剩余：{status?.lastTotalRemaining?.toFixed(2) ?? '—'} ·
          上次检查：{fmtTime(status?.lastEvaluatedAt)} ·
          SMTP：{config?.smtpConfigured ? '已配置' : '未配置（通过环境变量设置）'}
        </div>
        <Button onClick={handleSave} disabled={updateConfig.isPending}>保存设置</Button>

        <div className="border-t pt-4">
          <div className="flex items-center justify-between mb-2">
            <span className="font-medium text-sm">通知渠道</span>
            <Button size="sm" variant="outline" onClick={() => { setEditing(null); setDialogOpen(true) }}>
              <Plus className="h-4 w-4 mr-1" />添加渠道
            </Button>
          </div>
          <div className="space-y-2">
            {config?.channels.map((ch) => (
              <div key={ch.id} className="flex items-center justify-between rounded border px-3 py-2 text-sm">
                <div className="flex items-center gap-2">
                  <Badge variant="outline">{ch.kind}</Badge>
                  <span>{ch.name || (ch.kind === 'telegram' ? ch.maskedBotToken : ch.to)}</span>
                  {!ch.enabled && <Badge variant="secondary">已禁用</Badge>}
                </div>
                <div className="flex gap-1">
                  <Button size="icon" variant="ghost" onClick={() => { setEditing(ch); setDialogOpen(true) }}>
                    <Pencil className="h-4 w-4" />
                  </Button>
                  <Button size="icon" variant="ghost" onClick={() => handleDelete(ch.id)}>
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            ))}
            {(!config?.channels || config.channels.length === 0) && (
              <div className="text-sm text-muted-foreground">暂无渠道</div>
            )}
          </div>
        </div>
      </CardContent>
      <AlertChannelDialog open={dialogOpen} onOpenChange={setDialogOpen} channel={editing} />
    </Card>
  )
}
```

- [ ] **Step 3: 接入 dashboard**

在 `admin-ui/src/components/dashboard.tsx` import 区加：

```tsx
import { AlertSettings } from '@/components/alert-settings'
```

在主内容 `<main ...>` 内、统计卡片区块（`{/* 统计卡片 */}`）之前插入：

```tsx
        <AlertSettings />
```

- [ ] **Step 4: 构建**

Run: 在 `admin-ui/` 下 `pnpm build`（或上文 docker node 命令）。
Expected: 构建成功，`admin-ui/dist` 更新。

- [ ] **Step 5: 提交**

```bash
git add admin-ui/src/components/alert-settings.tsx admin-ui/src/components/alert-channel-dialog.tsx admin-ui/src/components/dashboard.tsx admin-ui/dist
git commit -m "feat(alert-ui): 预警设置组件与渠道对话框

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 12: 集成验证与文档

**Files:**
- Modify: `README.md`（新增 SMTP 环境变量与预警功能说明）
- Modify: `.env.example` 或等价文件（如存在）

**Interfaces:** 无新代码接口，仅端到端验证。

- [ ] **Step 1: 全量编译 + 测试**

Run: `docker run --rm -v "D:\dev\kiro.rs:/app" -v "kiro_cargo_registry:/usr/local/cargo/registry" -v "kiro-target:/app/target" -w /app rust:1.92-alpine sh -c "cargo test --no-default-features --offline && cargo check --release --no-default-features --offline"`
Expected: 全部测试 PASS，check 通过。

- [ ] **Step 2: 前端全量构建**

Run: `admin-ui/` 下 `pnpm build`。
Expected: 成功，`admin-ui/dist` 就绪。

- [ ] **Step 3: 完整镜像构建（走代理拉取 lettre）**

Run: `docker build --build-arg BUILD_PROXY=socks5h://host.docker.internal:10808 -t kiro-rs:alert-test .`
Expected: 镜像构建成功（验证 lettre 在正式构建路径可用）。

- [ ] **Step 4: 手动冒烟（可选，需运行实例）**

启动容器（配置好 `admin_api_key`），用 curl 验证：
```bash
curl -s -H "x-api-key: <KEY>" http://127.0.0.1:8080/api/admin/alerts/config
curl -s -X PUT -H "x-api-key: <KEY>" -H "Content-Type: application/json" \
  -d '{"enabled":true,"thresholdRemaining":1000}' http://127.0.0.1:8080/api/admin/alerts/config
curl -s -X POST -H "x-api-key: <KEY>" -H "Content-Type: application/json" \
  -d '{"kind":"telegram","botToken":"123:XYZ","chatId":"-100","name":"t"}' http://127.0.0.1:8080/api/admin/alerts/channels
# 确认 GET config 返回的 maskedBotToken 为脱敏值，非明文
curl -s -X POST -H "x-api-key: <KEY>" http://127.0.0.1:8080/api/admin/alerts/test
```
Expected: config 读写正常；`maskedBotToken` 脱敏；test 返回逐渠道结果。

- [ ] **Step 5: 更新 README**

在 README 环境变量/配置章节追加：SMTP 环境变量清单（`ALERT_SMTP_HOST` 等）、预警功能简介（阈值、轮询、Telegram/Email 渠道、单次告警语义与 re-arm 条件）。

- [ ] **Step 6: 提交**

```bash
git add README.md
git commit -m "docs(alert): README 补充预警功能与 SMTP 环境变量

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## 附：SMTP 环境变量清单

| 变量 | 必填 | 说明 |
|---|---|---|
| `ALERT_SMTP_HOST` | 是 | SMTP 服务器地址；缺失则 email 渠道禁用 |
| `ALERT_SMTP_FROM` | 是 | 发件人地址；缺失则 email 渠道禁用 |
| `ALERT_SMTP_PORT` | 否 | 端口；缺省按 TLS 推断（implicit=465，其它=587）|
| `ALERT_SMTP_USERNAME` | 否 | SMTP 认证用户名 |
| `ALERT_SMTP_PASSWORD` | 否 | SMTP 认证密码 |
| `ALERT_SMTP_TLS` | 否 | `starttls`（默认）/ `implicit` / `none` |





