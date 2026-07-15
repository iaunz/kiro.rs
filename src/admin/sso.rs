//! AWS SSO OIDC 自动凭据导入 —— 会话管理
//!
//! 流程：
//! 1. 用户提交 Start URL / Auth Region / API Region，后端注册 OIDC 客户端并发起
//!    设备授权，返回带 user code 的验证 URL 给用户。
//! 2. 用户在浏览器完成 SSO 登录并批准，后台任务轮询 CreateToken。
//! 3. 批准后取得 Refresh Token / Client ID / Client Secret，结合锁定的 Region，
//!    通过已有的添加凭据流程（IdC 认证）添加一个新凭据。
//!
//! 从提交到完成，所有请求数据（Start URL / Auth Region / API Region）均在后端锁定，
//! 客户端后续仅凭 session id 轮询状态，无法改变任何参数。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::http_client::ProxyConfig;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::sso_oidc::{self, CreateTokenPoll};
use crate::kiro::token_manager::MultiTokenManager;
use crate::model::config::Config;

/// 支持的 API Region（选择框可选值）
pub const ALLOWED_API_REGIONS: [&str; 2] = ["us-east-1", "eu-central-1"];

/// 设备授权默认有效期（秒），服务端未返回时使用
const DEFAULT_DEVICE_EXPIRES_IN: i64 = 600;
/// 会话硬超时上限（秒），防止后台任务无限轮询
const MAX_SESSION_TTL_SECS: i64 = 900;
/// 默认轮询间隔（秒），服务端未返回 interval 时使用
const DEFAULT_POLL_INTERVAL_SECS: i64 = 5;
/// slow_down 时的退避增量（秒）
const SLOW_DOWN_BACKOFF_SECS: i64 = 5;
/// 已结束会话保留时长（秒），超过则在下次创建时清理
const FINISHED_SESSION_RETENTION_SECS: i64 = 3600;

/// 会话状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SsoStatus {
    /// 等待用户在浏览器完成登录并批准
    Pending,
    /// 已批准并成功添加凭据
    Completed,
    /// 失败（网络错误、添加凭据失败等）
    Failed,
    /// 设备码已过期（用户未在时限内批准）
    Expired,
    /// 用户拒绝了授权
    Denied,
    /// 会话被取消
    Cancelled,
}

impl SsoStatus {
    fn is_finished(self) -> bool {
        !matches!(self, SsoStatus::Pending)
    }
}

/// 单个 SSO 会话的完整状态（后端锁定，客户端不可修改）
#[derive(Debug, Clone)]
struct SsoSessionState {
    status: SsoStatus,
    // ==== 锁定参数（创建后不可变） ====
    start_url: String,
    auth_region: String,
    api_region: String,
    // ==== 展示给用户的设备授权信息 ====
    user_code: String,
    verification_uri: Option<String>,
    verification_uri_complete: Option<String>,
    // ==== 时间 ====
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    // ==== 结果 ====
    credential_id: Option<u64>,
    email: Option<String>,
    error: Option<String>,
}

/// 会话条目（状态 + 取消标志）
struct SsoSession {
    state: SsoSessionState,
    cancel: Arc<AtomicBool>,
}

/// 会话状态响应（对外，仅暴露非敏感字段；不含 clientSecret / refreshToken）
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SsoSessionResponse {
    pub session_id: String,
    pub status: SsoStatus,
    pub start_url: String,
    pub auth_region: String,
    pub api_region: String,
    pub user_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_uri_complete: Option<String>,
    pub expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 创建会话请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartSsoSessionRequest {
    /// 门户 Start URL，例如 https://<alias>.awsapps.com/start
    pub start_url: String,
    /// 门户 / SSO 所在区域（用于 OIDC token 刷新）
    pub auth_region: String,
    /// API Region（用于 API 请求），仅允许 us-east-1 / eu-central-1
    pub api_region: String,
    /// 优先级（可选，默认 0）
    #[serde(default)]
    pub priority: u32,
    /// 端点名称（可选）
    #[serde(default)]
    pub endpoint: Option<String>,
}

/// SSO 会话管理器
pub struct SsoSessionManager {
    token_manager: Arc<MultiTokenManager>,
    sessions: Arc<Mutex<HashMap<String, SsoSession>>>,
}

/// 会话操作错误
#[derive(Debug)]
pub enum SsoError {
    /// 请求参数无效
    InvalidRequest(String),
    /// 会话不存在
    NotFound(String),
    /// 上游 OIDC 调用失败
    Upstream(String),
}

impl SsoSessionManager {
    pub fn new(token_manager: Arc<MultiTokenManager>) -> Self {
        Self {
            token_manager,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 从全局配置重建代理配置（与 main.rs 的全局代理一致）
    fn global_proxy(config: &Config) -> Option<ProxyConfig> {
        config.proxy_url.as_ref().map(|url| {
            let mut proxy = ProxyConfig::new(url);
            if let (Some(username), Some(password)) =
                (&config.proxy_username, &config.proxy_password)
            {
                proxy = proxy.with_auth(username, password);
            }
            proxy
        })
    }

    /// 清理已结束且超过保留时长的会话
    fn purge_finished(&self) {
        let now = Utc::now();
        let mut sessions = self.sessions.lock();
        sessions.retain(|_, s| {
            if !s.state.status.is_finished() {
                return true;
            }
            (now - s.state.created_at).num_seconds() < FINISHED_SESSION_RETENTION_SECS
        });
    }

    /// 生成会话状态响应
    fn to_response(session_id: &str, state: &SsoSessionState) -> SsoSessionResponse {
        SsoSessionResponse {
            session_id: session_id.to_string(),
            status: state.status,
            start_url: state.start_url.clone(),
            auth_region: state.auth_region.clone(),
            api_region: state.api_region.clone(),
            user_code: state.user_code.clone(),
            verification_uri: state.verification_uri.clone(),
            verification_uri_complete: state.verification_uri_complete.clone(),
            expires_at: state.expires_at.to_rfc3339(),
            credential_id: state.credential_id,
            email: state.email.clone(),
            error: state.error.clone(),
        }
    }

    /// 发起 SSO 会话：注册客户端 + 设备授权，返回带 user code 的验证 URL
    pub async fn start_session(
        &self,
        req: StartSsoSessionRequest,
    ) -> Result<SsoSessionResponse, SsoError> {
        // 清理陈旧会话
        self.purge_finished();

        // 校验并规范化参数
        let start_url = req.start_url.trim().trim_end_matches('/').to_string();
        let auth_region = req.auth_region.trim().to_string();
        let api_region = req.api_region.trim().to_string();

        if start_url.is_empty() {
            return Err(SsoError::InvalidRequest("Start URL 不能为空".to_string()));
        }
        if !(start_url.starts_with("https://") || start_url.starts_with("http://")) {
            return Err(SsoError::InvalidRequest(
                "Start URL 必须以 http(s):// 开头".to_string(),
            ));
        }
        if auth_region.is_empty() {
            return Err(SsoError::InvalidRequest("Auth Region 不能为空".to_string()));
        }
        if !ALLOWED_API_REGIONS.contains(&api_region.as_str()) {
            return Err(SsoError::InvalidRequest(format!(
                "API Region 仅支持 {:?}",
                ALLOWED_API_REGIONS
            )));
        }

        let config = self.token_manager.config();
        let tls_backend = config.tls_backend;
        let proxy = Self::global_proxy(config);

        // 1. RegisterClient
        let registered =
            sso_oidc::register_client(&auth_region, proxy.as_ref(), tls_backend)
                .await
                .map_err(|e| SsoError::Upstream(e.to_string()))?;

        // 2. StartDeviceAuthorization
        let dev = sso_oidc::start_device_authorization(
            &auth_region,
            &registered.client_id,
            &registered.client_secret,
            &start_url,
            proxy.as_ref(),
            tls_backend,
        )
        .await
        .map_err(|e| SsoError::Upstream(e.to_string()))?;

        let now = Utc::now();
        let device_expires_in = dev.expires_in.unwrap_or(DEFAULT_DEVICE_EXPIRES_IN);
        // 会话截止时间取设备码有效期与硬上限的较小值
        let ttl = device_expires_in.min(MAX_SESSION_TTL_SECS).max(1);
        let expires_at = now + Duration::seconds(ttl);
        let interval = dev.interval.unwrap_or(DEFAULT_POLL_INTERVAL_SECS).max(1);

        let session_id = uuid::Uuid::new_v4().to_string();
        let cancel = Arc::new(AtomicBool::new(false));

        let state = SsoSessionState {
            status: SsoStatus::Pending,
            start_url: start_url.clone(),
            auth_region: auth_region.clone(),
            api_region: api_region.clone(),
            user_code: dev.user_code.clone(),
            verification_uri: dev.verification_uri.clone(),
            verification_uri_complete: dev.verification_uri_complete.clone(),
            created_at: now,
            expires_at,
            credential_id: None,
            email: None,
            error: None,
        };

        let response = Self::to_response(&session_id, &state);

        {
            let mut sessions = self.sessions.lock();
            sessions.insert(
                session_id.clone(),
                SsoSession {
                    state,
                    cancel: cancel.clone(),
                },
            );
        }

        // 3. 后台轮询 CreateToken 并在批准后添加凭据
        let poll_ctx = PollContext {
            session_id,
            token_manager: self.token_manager.clone(),
            sessions: self.sessions.clone(),
            cancel,
            auth_region,
            api_region,
            start_url,
            client_id: registered.client_id,
            client_secret: registered.client_secret,
            device_code: dev.device_code,
            interval,
            deadline: expires_at,
            priority: req.priority,
            endpoint: req.endpoint,
            proxy,
            tls_backend,
        };
        tokio::spawn(poll_and_import(poll_ctx));

        Ok(response)
    }

    /// 查询会话状态
    pub fn get_session(&self, session_id: &str) -> Result<SsoSessionResponse, SsoError> {
        let sessions = self.sessions.lock();
        let session = sessions
            .get(session_id)
            .ok_or_else(|| SsoError::NotFound(format!("会话不存在: {}", session_id)))?;
        Ok(Self::to_response(session_id, &session.state))
    }

    /// 取消会话（若仍在等待中）
    pub fn cancel_session(&self, session_id: &str) -> Result<SsoSessionResponse, SsoError> {
        let mut sessions = self.sessions.lock();
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| SsoError::NotFound(format!("会话不存在: {}", session_id)))?;

        if !session.state.status.is_finished() {
            session.cancel.store(true, Ordering::Relaxed);
            session.state.status = SsoStatus::Cancelled;
        }
        Ok(Self::to_response(session_id, &session.state))
    }
}

/// 后台轮询任务上下文（所有参数在此锁定，任务内不接受外部修改）
struct PollContext {
    session_id: String,
    token_manager: Arc<MultiTokenManager>,
    sessions: Arc<Mutex<HashMap<String, SsoSession>>>,
    cancel: Arc<AtomicBool>,
    auth_region: String,
    api_region: String,
    start_url: String,
    client_id: String,
    client_secret: String,
    device_code: String,
    interval: i64,
    deadline: DateTime<Utc>,
    priority: u32,
    endpoint: Option<String>,
    proxy: Option<ProxyConfig>,
    tls_backend: crate::model::config::TlsBackend,
}

/// 更新会话状态的辅助闭包
fn set_session_status(
    sessions: &Arc<Mutex<HashMap<String, SsoSession>>>,
    session_id: &str,
    update: impl FnOnce(&mut SsoSessionState),
) {
    let mut map = sessions.lock();
    if let Some(session) = map.get_mut(session_id) {
        // 已被取消的会话不再覆盖状态
        if session.state.status == SsoStatus::Cancelled {
            return;
        }
        update(&mut session.state);
    }
}

/// 后台轮询 CreateToken，批准后添加凭据
async fn poll_and_import(ctx: PollContext) {
    let mut interval = ctx.interval;

    let token = loop {
        // 取消检查
        if ctx.cancel.load(Ordering::Relaxed) {
            tracing::info!("SSO 会话 {} 已取消，停止轮询", ctx.session_id);
            return;
        }

        // 超时检查
        if Utc::now() >= ctx.deadline {
            tracing::warn!("SSO 会话 {} 超时", ctx.session_id);
            set_session_status(&ctx.sessions, &ctx.session_id, |s| {
                s.status = SsoStatus::Expired;
                s.error = Some("等待用户授权超时".to_string());
            });
            return;
        }

        // 等待一个轮询间隔（分片睡眠以便及时响应取消/超时）
        let mut slept = 0i64;
        while slept < interval {
            if ctx.cancel.load(Ordering::Relaxed) || Utc::now() >= ctx.deadline {
                break;
            }
            tokio::time::sleep(StdDuration::from_secs(1)).await;
            slept += 1;
        }

        if ctx.cancel.load(Ordering::Relaxed) {
            return;
        }

        match sso_oidc::create_token_once(
            &ctx.auth_region,
            &ctx.client_id,
            &ctx.client_secret,
            &ctx.device_code,
            ctx.proxy.as_ref(),
            ctx.tls_backend,
        )
        .await
        {
            Ok(CreateTokenPoll::Token(token)) => break *token,
            Ok(CreateTokenPoll::Pending) => continue,
            Ok(CreateTokenPoll::SlowDown) => {
                interval += SLOW_DOWN_BACKOFF_SECS;
                continue;
            }
            Ok(CreateTokenPoll::Expired) => {
                set_session_status(&ctx.sessions, &ctx.session_id, |s| {
                    s.status = SsoStatus::Expired;
                    s.error = Some("设备码已过期，请重新发起".to_string());
                });
                return;
            }
            Ok(CreateTokenPoll::Denied) => {
                set_session_status(&ctx.sessions, &ctx.session_id, |s| {
                    s.status = SsoStatus::Denied;
                    s.error = Some("用户拒绝了授权".to_string());
                });
                return;
            }
            Err(e) => {
                tracing::error!("SSO 会话 {} 轮询失败: {}", ctx.session_id, e);
                set_session_status(&ctx.sessions, &ctx.session_id, |s| {
                    s.status = SsoStatus::Failed;
                    s.error = Some(format!("轮询失败: {}", e));
                });
                return;
            }
        }
    };

    // 已批准，取得 refresh token；再次检查取消
    if ctx.cancel.load(Ordering::Relaxed) {
        return;
    }

    let refresh_token = match token.refresh_token {
        Some(rt) if !rt.is_empty() => rt,
        _ => {
            set_session_status(&ctx.sessions, &ctx.session_id, |s| {
                s.status = SsoStatus::Failed;
                s.error = Some("授权成功但未返回 Refresh Token".to_string());
            });
            return;
        }
    };

    // 通过已有添加凭据流程添加 IdC 凭据（会重新刷新 token 以验证有效性）
    let new_cred = KiroCredentials {
        auth_method: Some("idc".to_string()),
        refresh_token: Some(refresh_token),
        client_id: Some(ctx.client_id.clone()),
        client_secret: Some(ctx.client_secret.clone()),
        auth_region: Some(ctx.auth_region.clone()),
        api_region: Some(ctx.api_region.clone()),
        priority: ctx.priority,
        endpoint: ctx.endpoint.clone(),
        ..Default::default()
    };

    match ctx.token_manager.add_credential(new_cred).await {
        Ok(credential_id) => {
            // 主动获取订阅等级（失败不影响导入）
            if let Err(e) = ctx.token_manager.get_usage_limits_for(credential_id).await {
                tracing::warn!("SSO 导入后获取订阅等级失败（不影响导入）: {}", e);
            }
            tracing::info!(
                "SSO 会话 {} 完成，已添加凭据 #{}（start_url={}）",
                ctx.session_id,
                credential_id,
                ctx.start_url
            );
            set_session_status(&ctx.sessions, &ctx.session_id, |s| {
                s.status = SsoStatus::Completed;
                s.credential_id = Some(credential_id);
            });
        }
        Err(e) => {
            tracing::error!("SSO 会话 {} 添加凭据失败: {}", ctx.session_id, e);
            set_session_status(&ctx.sessions, &ctx.session_id, |s| {
                s.status = SsoStatus::Failed;
                s.error = Some(format!("添加凭据失败: {}", e));
            });
        }
    }
}
