//! AWS IAM Identity Center (SSO) OIDC 设备授权流程客户端
//!
//! 实现标准 OAuth 设备授权流程（RFC 8628，与 `aws sso login` 一致）：
//! 1. `RegisterClient` — 取得 clientId / clientSecret
//! 2. `StartDeviceAuthorization` — 取得 userCode / deviceCode / verificationUriComplete
//! 3. 用户在浏览器登录并批准
//! 4. `CreateToken`（device_code grant）轮询 — 取得 refreshToken / accessToken
//!
//! 三个交付物（clientId / clientSecret / refreshToken）随后可作为 IdC 凭据添加。

use serde::{Deserialize, Serialize};

use crate::http_client::{ProxyConfig, build_client};
use crate::model::config::TlsBackend;

/// 注册的 OIDC 客户端名
const CLIENT_NAME: &str = "kiro-cli";
/// 设备授权 grant type
const GRANT_DEVICE_CODE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// OIDC HTTP 请求超时（秒）
const OIDC_TIMEOUT_SECS: u64 = 30;

fn oidc_host(region: &str) -> String {
    format!("oidc.{}.amazonaws.com", region)
}

// ============ RegisterClient ============

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegisterClientRequest {
    client_name: String,
    client_type: String,
    scopes: Vec<String>,
    grant_types: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterClientResponse {
    pub client_id: String,
    pub client_secret: String,
}

/// 注册 OIDC 客户端，取得 clientId / clientSecret
///
/// `scopes` 决定取得的 Token 拥有的权限。使用 CodeWhisperer scopes
/// （如 `codewhisperer:completions`）才能访问 Kiro / CodeWhisperer API；
/// 仅 `sso:account:access` 只能访问 AWS 账号，调用 Kiro API 会 403。
pub async fn register_client(
    region: &str,
    scopes: &[String],
    proxy: Option<&ProxyConfig>,
    tls_backend: TlsBackend,
) -> anyhow::Result<RegisterClientResponse> {
    let host = oidc_host(region);
    let url = format!("https://{}/client/register", host);

    let body = RegisterClientRequest {
        client_name: CLIENT_NAME.to_string(),
        client_type: "public".to_string(),
        scopes: scopes.to_vec(),
        grant_types: vec![GRANT_DEVICE_CODE.to_string(), "refresh_token".to_string()],
    };

    let client = build_client(proxy, OIDC_TIMEOUT_SECS, tls_backend)?;
    let response = client
        .post(&url)
        .header("content-type", "application/json")
        .header("host", &host)
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        anyhow::bail!("RegisterClient 失败: {} {}", status, body_text);
    }

    Ok(response.json().await?)
}

// ============ StartDeviceAuthorization ============

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StartDeviceAuthRequest {
    client_id: String,
    client_secret: String,
    start_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartDeviceAuthResponse {
    pub device_code: String,
    pub user_code: String,
    #[serde(default)]
    pub verification_uri: Option<String>,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub interval: Option<i64>,
}

/// 发起设备授权，取得 userCode / deviceCode / verificationUriComplete
pub async fn start_device_authorization(
    region: &str,
    client_id: &str,
    client_secret: &str,
    start_url: &str,
    proxy: Option<&ProxyConfig>,
    tls_backend: TlsBackend,
) -> anyhow::Result<StartDeviceAuthResponse> {
    let host = oidc_host(region);
    let url = format!("https://{}/device_authorization", host);

    let body = StartDeviceAuthRequest {
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
        start_url: start_url.to_string(),
    };

    let client = build_client(proxy, OIDC_TIMEOUT_SECS, tls_backend)?;
    let response = client
        .post(&url)
        .header("content-type", "application/json")
        .header("host", &host)
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        // 区域取错会返回 InvalidRequestException
        anyhow::bail!(
            "StartDeviceAuthorization 失败（请确认 Start URL 与 Auth Region 正确）: {} {}",
            status,
            body_text
        );
    }

    Ok(response.json().await?)
}

// ============ CreateToken（device_code grant，轮询） ============

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateTokenRequest {
    client_id: String,
    client_secret: String,
    grant_type: String,
    device_code: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTokenResponse {
    /// 访问令牌（当前流程只取 refresh_token，保留以备扩展）
    #[allow(dead_code)]
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// 访问令牌有效期（秒），保留以备扩展
    #[allow(dead_code)]
    #[serde(default)]
    pub expires_in: Option<i64>,
}

/// CreateToken 单次轮询结果
#[derive(Debug)]
pub enum CreateTokenPoll {
    /// 用户已批准，取得 token
    Token(Box<CreateTokenResponse>),
    /// 尚未批准，继续等待
    Pending,
    /// 服务端要求降低轮询频率
    SlowDown,
    /// 设备码已过期
    Expired,
    /// 用户拒绝了授权
    Denied,
}

#[derive(Debug, Deserialize)]
struct OidcErrorBody {
    #[serde(default)]
    error: String,
}

/// 轮询一次 CreateToken
///
/// 未批准时返回 `Pending`（不视为错误），由调用方按 interval 重试。
pub async fn create_token_once(
    region: &str,
    client_id: &str,
    client_secret: &str,
    device_code: &str,
    proxy: Option<&ProxyConfig>,
    tls_backend: TlsBackend,
) -> anyhow::Result<CreateTokenPoll> {
    let host = oidc_host(region);
    let url = format!("https://{}/token", host);

    let body = CreateTokenRequest {
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
        grant_type: GRANT_DEVICE_CODE.to_string(),
        device_code: device_code.to_string(),
    };

    let client = build_client(proxy, OIDC_TIMEOUT_SECS, tls_backend)?;
    let response = client
        .post(&url)
        .header("content-type", "application/json")
        .header("host", &host)
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if status.is_success() {
        let token: CreateTokenResponse = response.json().await?;
        return Ok(CreateTokenPoll::Token(Box::new(token)));
    }

    // 非 2xx：解析 OAuth 错误码
    let body_text = response.text().await.unwrap_or_default();
    let error_code = serde_json::from_str::<OidcErrorBody>(&body_text)
        .map(|e| e.error)
        .unwrap_or_default();

    match error_code.as_str() {
        "authorization_pending" => Ok(CreateTokenPoll::Pending),
        "slow_down" => Ok(CreateTokenPoll::SlowDown),
        "expired_token" => Ok(CreateTokenPoll::Expired),
        "access_denied" => Ok(CreateTokenPoll::Denied),
        _ => anyhow::bail!("CreateToken 失败: {} {}", status, body_text),
    }
}
