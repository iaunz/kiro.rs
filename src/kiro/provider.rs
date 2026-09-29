//! Kiro API Provider
//!
//! 核心组件，负责与 Kiro API 通信
//! 支持流式和非流式请求
//! 支持多凭据故障转移和重试
//! 支持按凭据级 endpoint 切换不同 Kiro API 端点

use reqwest::Client;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use crate::http_client::{ProxyConfig, build_client};
use crate::kiro::endpoint::{KiroEndpoint, RequestContext};
use crate::kiro::image_recovery::{image_dimension_limit, resize_request_images};
use crate::kiro::machine_id;
use crate::kiro::model::available_models::UpstreamModel;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::model_catalog::{ModelCatalogCache, fetch_available_models};
use crate::kiro::token_manager::{CallContext, MultiTokenManager};
use crate::model::config::TlsBackend;
use parking_lot::Mutex;

/// 每个凭据的最大重试次数
const MAX_RETRIES_PER_CREDENTIAL: usize = 3;

/// 总重试次数硬上限（避免无限重试）
const MAX_TOTAL_RETRIES: usize = 9;

/// Kiro API Provider
///
/// 核心组件，负责与 Kiro API 通信
/// 支持多凭据故障转移和重试机制
/// 按凭据 `endpoint` 字段选择 [`KiroEndpoint`] 实现
pub struct KiroProvider {
    token_manager: Arc<MultiTokenManager>,
    /// 全局代理配置（用于凭据无自定义代理时的回退）
    global_proxy: Option<ProxyConfig>,
    /// Client 缓存：key = effective proxy config, value = reqwest::Client
    /// 不同代理配置的凭据使用不同的 Client，共享相同代理的凭据复用 Client
    client_cache: Mutex<HashMap<Option<ProxyConfig>, Client>>,
    /// TLS 后端配置
    tls_backend: TlsBackend,
    /// 端点实现注册表（key: endpoint 名称）
    endpoints: HashMap<String, Arc<dyn KiroEndpoint>>,
    /// 默认端点名称（凭据未指定 endpoint 时使用）
    default_endpoint: String,
    /// 已尝试过 profileArn 解析的凭据 ID（进程内去重）
    ///
    /// 只在拿到**上游确定结果**后才写入：解析成功后凭据已有真实 ARN，
    /// `streaming_profile_arn()` 直接命中不会再进来；确定无 profile 的账号
    /// （纯 BuilderID）靠这个集合避免每次请求都白跑一次往返。
    /// 网络抖动等不确定失败不写入，留待下次请求重试。
    profile_resolution_attempted: Mutex<HashSet<u64>>,
    model_catalog: ModelCatalogCache,
}

impl KiroProvider {
    /// 创建带代理配置和端点注册表的 KiroProvider 实例
    ///
    /// # Arguments
    /// * `token_manager` - 多凭据 Token 管理器
    /// * `proxy` - 全局代理配置
    /// * `endpoints` - 端点名 → 实现的注册表（至少包含 `default_endpoint` 对应条目）
    /// * `default_endpoint` - 凭据未显式指定 endpoint 时使用的名称
    pub fn with_proxy(
        token_manager: Arc<MultiTokenManager>,
        proxy: Option<ProxyConfig>,
        endpoints: HashMap<String, Arc<dyn KiroEndpoint>>,
        default_endpoint: String,
    ) -> Self {
        assert!(
            endpoints.contains_key(&default_endpoint),
            "默认端点 {} 未在 endpoints 注册表中",
            default_endpoint
        );
        let tls_backend = token_manager.config().tls_backend;
        // 预热：构建全局代理对应的 Client
        let initial_client = build_client(proxy.as_ref(), 720, tls_backend)
            .expect("创建 HTTP 客户端失败");
        let mut cache = HashMap::new();
        cache.insert(proxy.clone(), initial_client);

        Self {
            token_manager,
            global_proxy: proxy,
            client_cache: Mutex::new(cache),
            tls_backend,
            endpoints,
            default_endpoint,
            profile_resolution_attempted: Mutex::new(HashSet::new()),
            model_catalog: ModelCatalogCache::default(),
        }
    }

    /// 在发起请求前，确保 Enterprise / IdC 账号的真实 profileArn 已解析并写入 `ctx`。
    ///
    /// 流式端点强制要求 profileArn：不带会被上游以
    /// `403 {"message":"User is not authorized to make this call."}` 拒绝。
    /// Enterprise / IdC 账号还必须是**真实** ARN —— BuilderID 占位符会因身份不匹配被拒，
    /// 而真实 ARN 既不在 OIDC 刷新响应里，也不在 SSO 导入结果里，只能查
    /// `ListAvailableProfiles`。
    ///
    /// 仅对「OAuth 凭据 + profileArn 缺失或仍是占位符」触发一次查询（进程内去重）：
    /// - 命中真实 ARN → 写回并持久化，之后 `streaming_profile_arn()` 直接命中；
    /// - 上游确定无 profile（纯 BuilderID）→ 标记已尝试，回退占位符；
    /// - 查询失败 → **不标记**，本次按原值继续，下次请求再试。
    async fn ensure_profile_arn(&self, ctx: &mut CallContext) {
        use crate::kiro::model::credentials::is_placeholder_profile_arn;

        if ctx.credentials.is_api_key_credential() {
            return;
        }
        let needs = match ctx.credentials.profile_arn.as_deref() {
            None => true,
            Some(arn) => is_placeholder_profile_arn(arn),
        };
        if !needs {
            return;
        }
        if self.profile_resolution_attempted.lock().contains(&ctx.id) {
            return;
        }

        match self
            .token_manager
            .resolve_profile_arn_for(ctx.id, &ctx.token)
            .await
        {
            Ok(Some(arn)) => {
                ctx.credentials.profile_arn = Some(arn);
                self.profile_resolution_attempted.lock().insert(ctx.id);
            }
            Ok(None) => {
                // 上游确认该账号无 Enterprise profile：标记已尝试，后续回退占位符
                self.profile_resolution_attempted.lock().insert(ctx.id);
            }
            Err(e) => {
                // 网络/瞬态错误：不标记，下次请求再试；本次按原 profileArn 继续
                tracing::warn!(
                    "凭据 #{} 解析真实 profileArn 失败（按原 profileArn 继续）: {}",
                    ctx.id,
                    e
                );
            }
        }
    }

    /// 根据凭据的代理配置获取（或创建并缓存）对应的 reqwest::Client
    fn client_for(&self, credentials: &KiroCredentials) -> anyhow::Result<Client> {
        let effective = credentials.effective_proxy(self.global_proxy.as_ref());
        let mut cache = self.client_cache.lock();
        if let Some(client) = cache.get(&effective) {
            return Ok(client.clone());
        }
        let client = build_client(effective.as_ref(), 720, self.tls_backend)?;
        cache.insert(effective, client.clone());
        Ok(client)
    }

    /// 根据凭据选择 endpoint 实现
    fn endpoint_for(
        &self,
        credentials: &KiroCredentials,
    ) -> anyhow::Result<Arc<dyn KiroEndpoint>> {
        let name = credentials
            .endpoint
            .as_deref()
            .unwrap_or(&self.default_endpoint);
        self.endpoints
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("未知端点: {}", name))
    }

    /// 发送非流式 API 请求
    ///
    /// 支持多凭据故障转移（见 [`Self::call_api_with_retry`]）
    pub async fn call_api(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        self.call_api_with_retry(request_body, false).await
    }

    /// 发送流式 API 请求
    pub async fn call_api_stream(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        self.call_api_with_retry(request_body, true).await
    }

    /// 发送 MCP API 请求（WebSearch 等工具调用）
    pub async fn call_mcp(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        self.call_mcp_with_retry(request_body).await
    }

    /// 获取当前凭据可见的模型目录。成功缓存五分钟，失败保留旧值并退避三十秒。
    ///
    /// 该查询复用 Token、代理和 profile ARN 解析，不修改推理成功/失败计数。
    /// 首次发现失败由调用者保留静态模型列表；后续失败返回上次成功的列表。
    pub async fn available_models(&self) -> anyhow::Result<Vec<UpstreamModel>> {
        self.model_catalog
            .get_or_refresh(|| async {
                tokio::time::timeout(Duration::from_secs(30), async {
                    let mut ctx = self.token_manager.acquire_context(None).await?;
                    self.ensure_profile_arn(&mut ctx).await;
                    let client = self.client_for(&ctx.credentials)?;
                    fetch_available_models(
                        &client,
                        &ctx.credentials,
                        self.token_manager.config(),
                        &ctx.token,
                    )
                    .await
                })
                .await
                .map_err(|_| anyhow::anyhow!("自动获取 Kiro 模型超时"))?
            })
            .await
    }

    /// 内部方法：带重试逻辑的 MCP API 调用
    async fn call_mcp_with_retry(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        let total_credentials = self.token_manager.total_count();
        let max_retries = (total_credentials * MAX_RETRIES_PER_CREDENTIAL).min(MAX_TOTAL_RETRIES);
        let mut last_error: Option<anyhow::Error> = None;
        let mut force_refreshed: HashSet<u64> = HashSet::new();

        for attempt in 0..max_retries {
            // MCP 调用（WebSearch 等工具）不涉及模型选择，无需按模型过滤凭据
            let mut ctx = match self.token_manager.acquire_context(None).await {
                Ok(c) => c,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };

            // MCP 的 x-amzn-kiro-profile-arn 头同样需要真实 ARN
            self.ensure_profile_arn(&mut ctx).await;

            let config = self.token_manager.config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, config);

            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(e) => e,
                Err(e) => {
                    last_error = Some(e);
                    // endpoint 解析失败：记为失败，换下一张凭据
                    self.token_manager.report_failure(ctx.id);
                    continue;
                }
            };

            let rctx = RequestContext {
                credentials: &ctx.credentials,
                token: &ctx.token,
                machine_id: &machine_id,
                config,
            };

            let url = endpoint.mcp_url(&rctx);
            let body = endpoint.transform_mcp_body(request_body, &rctx);

            let base = self
                .client_for(&ctx.credentials)?
                .post(&url)
                .body(body)
                .header("content-type", "application/json")
                .header("Connection", "close");
            let request = endpoint.decorate_mcp(base, &rctx);

            let response = match request.send().await {
                Ok(resp) => resp,
                Err(e) => {
                    tracing::warn!(
                        "MCP 请求发送失败（尝试 {}/{}）: {}",
                        attempt + 1,
                        max_retries,
                        e
                    );
                    last_error = Some(e.into());
                    if attempt + 1 < max_retries {
                        sleep(Self::retry_delay(attempt)).await;
                    }
                    continue;
                }
            };

            let status = response.status();

            // 成功响应
            if status.is_success() {
                self.token_manager.report_success(ctx.id);
                return Ok(response);
            }

            // 失败响应
            let body = response.text().await.unwrap_or_default();

            // 402 额度用尽
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
                let has_available = self.token_manager.report_quota_exhausted(ctx.id);
                if !has_available {
                    anyhow::bail!("MCP 请求失败（所有凭据已用尽）: {} {}", status, body);
                }
                last_error = Some(anyhow::anyhow!("MCP 请求失败: {} {}", status, body));
                continue;
            }

            // 400 Bad Request
            if status.as_u16() == 400 {
                anyhow::bail!("MCP 请求失败: {} {}", status, body);
            }

            // 401/403 凭据问题
            if matches!(status.as_u16(), 401 | 403) {
                // token 被上游失效：先尝试 force-refresh，每凭据仅一次机会
                if endpoint.is_bearer_token_invalid(&body) && !force_refreshed.contains(&ctx.id) {
                    force_refreshed.insert(ctx.id);
                    tracing::info!("凭据 #{} token 疑似被上游失效，尝试强制刷新", ctx.id);
                    if self.token_manager.force_refresh_token_for(ctx.id).await.is_ok() {
                        tracing::info!("凭据 #{} token 强制刷新成功，重试请求", ctx.id);
                        continue;
                    }
                    tracing::warn!("凭据 #{} token 强制刷新失败，计入失败", ctx.id);
                }

                let has_available = self.token_manager.report_failure(ctx.id);
                if !has_available {
                    anyhow::bail!("MCP 请求失败（所有凭据已用尽）: {} {}", status, body);
                }
                last_error = Some(anyhow::anyhow!("MCP 请求失败: {} {}", status, body));
                continue;
            }

            // 瞬态错误
            if matches!(status.as_u16(), 408 | 429) || status.is_server_error() {
                tracing::warn!(
                    "MCP 请求失败（上游瞬态错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                last_error = Some(anyhow::anyhow!("MCP 请求失败: {} {}", status, body));
                if attempt + 1 < max_retries {
                    sleep(Self::retry_delay(attempt)).await;
                }
                continue;
            }

            // 其他 4xx
            if status.is_client_error() {
                anyhow::bail!("MCP 请求失败: {} {}", status, body);
            }

            // 兜底
            last_error = Some(anyhow::anyhow!("MCP 请求失败: {} {}", status, body));
            if attempt + 1 < max_retries {
                sleep(Self::retry_delay(attempt)).await;
            }
        }

        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!("MCP 请求失败：已达到最大重试次数（{}次）", max_retries)
        }))
    }

    /// 内部方法：带重试逻辑的 API 调用
    ///
    /// 重试策略：
    /// - 每个凭据最多重试 MAX_RETRIES_PER_CREDENTIAL 次
    /// - 总重试次数 = min(凭据数量 × 每凭据重试次数, MAX_TOTAL_RETRIES)
    /// - 硬上限 9 次，避免无限重试
    /// - Kiro 明确报告图片尺寸上限时，仅追加一次同上下文恢复请求，不重置普通重试预算
    async fn call_api_with_retry(
        &self,
        request_body: &str,
        is_stream: bool,
    ) -> anyhow::Result<reqwest::Response> {
        let total_credentials = self.token_manager.total_count();
        let max_retries = (total_credentials * MAX_RETRIES_PER_CREDENTIAL).min(MAX_TOTAL_RETRIES);
        let mut last_error: Option<anyhow::Error> = None;
        let mut force_refreshed: HashSet<u64> = HashSet::new();
        let api_type = if is_stream { "流式" } else { "非流式" };
        // 仅在 Kiro 明确拒绝图片尺寸后尝试一次恢复，首次请求保持原样。
        let mut image_recovery_attempted = false;
        let mut recovered_request_body: Option<String> = None;

        // 尝试从请求体中提取模型信息
        let model = Self::extract_model_from_request(request_body);

        'attempts: for attempt in 0..max_retries {
            // 获取调用上下文（绑定 index、credentials、token）
            let mut ctx = match self.token_manager.acquire_context(model.as_deref()).await {
                Ok(c) => c,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };

            // Enterprise / IdC 账号需要真实 profileArn，流式端点强制要求
            self.ensure_profile_arn(&mut ctx).await;

            let config = self.token_manager.config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, config);

            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(e) => e,
                Err(e) => {
                    last_error = Some(e);
                    self.token_manager.report_failure(ctx.id);
                    continue;
                }
            };

            let rctx = RequestContext {
                credentials: &ctx.credentials,
                token: &ctx.token,
                machine_id: &machine_id,
                config,
            };

            let url = endpoint.api_url(&rctx);
            let client = self.client_for(&ctx.credentials)?;
            let (status, body) = loop {
                let effective_body = recovered_request_body.as_deref().unwrap_or(request_body);
                let body = endpoint.transform_api_body(effective_body, &rctx);
                let base = client
                    .post(&url)
                    .body(body)
                    .header("content-type", "application/json")
                    .header("Connection", "close");
                let request = endpoint.decorate_api(base, &rctx);

                let response = match request.send().await {
                    Ok(resp) => resp,
                    Err(e) => {
                        tracing::warn!(
                            "API 请求发送失败（尝试 {}/{}）: {}",
                            attempt + 1,
                            max_retries,
                            e
                        );
                        // 保留原网络重试预算，不因图片恢复重新调度或重置预算。
                        last_error = Some(e.into());
                        if attempt + 1 < max_retries {
                            sleep(Self::retry_delay(attempt)).await;
                        }
                        continue 'attempts;
                    }
                };

                let status = response.status();
                if status.is_success() {
                    self.token_manager.report_success(ctx.id);
                    return Ok(response);
                }

                let mut body = response.text().await.unwrap_or_default();
                if !image_recovery_attempted {
                    if let Some(limit) = image_dimension_limit(status, &body) {
                        image_recovery_attempted = true;
                        match resize_request_images(effective_body.to_owned(), limit).await {
                            Ok(Some(resized)) => {
                                recovered_request_body = Some(resized);
                                tracing::warn!(
                                    credential_id = ctx.id,
                                    max_dimension = limit,
                                    "Kiro 拒绝图片尺寸，按返回上限缩小图片并使用同一凭据重试一次"
                                );
                                // 此循环保留 ctx、token、endpoint 和 client，不计凭据失败。
                                continue;
                            }
                            Ok(None) => {}
                            Err(error) => {
                                tracing::warn!(
                                    credential_id = ctx.id,
                                    %error,
                                    "图片尺寸恢复失败，保留 Kiro 原始错误"
                                );
                                body.push_str(&format!("；自动图片缩放失败: {error}"));
                            }
                        }
                    }
                }
                break (status, body);
            };

            // 402 Payment Required 且额度用尽：禁用凭据并故障转移
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
                tracing::warn!(
                    "API 请求失败（额度已用尽，禁用凭据并切换，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );

                let has_available = self.token_manager.report_quota_exhausted(ctx.id);
                if !has_available {
                    anyhow::bail!(
                        "{} API 请求失败（所有凭据已用尽）: {} {}",
                        api_type,
                        status,
                        body
                    );
                }

                last_error = Some(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                ));
                continue;
            }

            // 400 Bad Request - 请求问题，重试/切换凭据无意义
            if status.as_u16() == 400 {
                anyhow::bail!("{} API 请求失败: {} {}", api_type, status, body);
            }

            // 401/403 - 更可能是凭据/权限问题：计入失败并允许故障转移
            if matches!(status.as_u16(), 401 | 403) {
                tracing::warn!(
                    "API 请求失败（可能为凭据错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );

                // token 被上游失效：先尝试 force-refresh，每凭据仅一次机会
                if endpoint.is_bearer_token_invalid(&body) && !force_refreshed.contains(&ctx.id) {
                    force_refreshed.insert(ctx.id);
                    tracing::info!("凭据 #{} token 疑似被上游失效，尝试强制刷新", ctx.id);
                    if self.token_manager.force_refresh_token_for(ctx.id).await.is_ok() {
                        tracing::info!("凭据 #{} token 强制刷新成功，重试请求", ctx.id);
                        continue;
                    }
                    tracing::warn!("凭据 #{} token 强制刷新失败，计入失败", ctx.id);
                }

                let has_available = self.token_manager.report_failure(ctx.id);
                if !has_available {
                    anyhow::bail!(
                        "{} API 请求失败（所有凭据已用尽）: {} {}",
                        api_type,
                        status,
                        body
                    );
                }

                last_error = Some(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                ));
                continue;
            }

            // 429/408/5xx - 瞬态上游错误：重试但不禁用或切换凭据
            // （避免 429 high traffic / 502 high load 等瞬态错误把所有凭据锁死）
            if matches!(status.as_u16(), 408 | 429) || status.is_server_error() {
                tracing::warn!(
                    "API 请求失败（上游瞬态错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                last_error = Some(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                ));
                if attempt + 1 < max_retries {
                    sleep(Self::retry_delay(attempt)).await;
                }
                continue;
            }

            // 其他 4xx - 通常为请求/配置问题：直接返回，不计入凭据失败
            if status.is_client_error() {
                anyhow::bail!("{} API 请求失败: {} {}", api_type, status, body);
            }

            // 兜底：当作可重试的瞬态错误处理（不切换凭据）
            tracing::warn!(
                "API 请求失败（未知错误，尝试 {}/{}）: {} {}",
                attempt + 1,
                max_retries,
                status,
                body
            );
            last_error = Some(anyhow::anyhow!(
                "{} API 请求失败: {} {}",
                api_type,
                status,
                body
            ));
            if attempt + 1 < max_retries {
                sleep(Self::retry_delay(attempt)).await;
            }
        }

        // 所有重试都失败
        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!(
                "{} API 请求失败：已达到最大重试次数（{}次）",
                api_type,
                max_retries
            )
        }))
    }

    /// 从请求体中提取模型信息
    ///
    /// 尝试解析 JSON 请求体，提取 conversationState.currentMessage.userInputMessage.modelId
    fn extract_model_from_request(request_body: &str) -> Option<String> {
        use serde_json::Value;

        let json: Value = serde_json::from_str(request_body).ok()?;

        json.get("conversationState")?
            .get("currentMessage")?
            .get("userInputMessage")?
            .get("modelId")?
            .as_str()
            .map(|s| s.to_string())
    }

    fn retry_delay(attempt: usize) -> Duration {
        // 指数退避 + 少量抖动，避免上游抖动时放大故障
        const BASE_MS: u64 = 200;
        const MAX_MS: u64 = 2_000;
        let exp = BASE_MS.saturating_mul(2u64.saturating_pow(attempt.min(6) as u32));
        let backoff = exp.min(MAX_MS);
        let jitter_max = (backoff / 4).max(1);
        let jitter = fastrand::u64(0..=jitter_max);
        Duration::from_millis(backoff.saturating_add(jitter))
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, io::Cursor};

    use axum::{Router, extract::State, http::HeaderMap, routing::post};
    use base64::{Engine, engine::general_purpose::STANDARD};
    use image::{DynamicImage, ImageFormat};
    use reqwest::{RequestBuilder, StatusCode};
    use serde_json::{Value, json};
    use tokio::{net::TcpListener, task::JoinHandle};

    use super::*;
    use crate::model::config::Config;

    const DIMENSION_ERROR: &str = r#"{"message":"messages.72.content.1.image.source.base64.data: At least one of the image dimensions exceed max allowed size for many-image requests: 2000 pixels","reason":"IMAGE_DIMENSION_EXCEEDED"}"#;

    struct MockEndpoint(String);

    impl KiroEndpoint for MockEndpoint {
        fn name(&self) -> &'static str {
            "mock"
        }

        fn api_url(&self, _: &RequestContext<'_>) -> String {
            self.0.clone()
        }

        fn mcp_url(&self, _: &RequestContext<'_>) -> String {
            self.0.clone()
        }

        fn decorate_api(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
            req.bearer_auth(ctx.token)
                .header("x-test-machine-id", ctx.machine_id)
        }

        fn decorate_mcp(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
            self.decorate_api(req, ctx)
        }

        fn transform_api_body(&self, body: &str, _: &RequestContext<'_>) -> String {
            body.to_owned()
        }
    }

    #[derive(Clone)]
    struct MockState {
        requests: Arc<Mutex<Vec<(HeaderMap, String)>>>,
        responses: Arc<Mutex<VecDeque<(StatusCode, String)>>>,
        manager: Arc<MultiTokenManager>,
        change_priority: bool,
    }

    async fn receive_request(
        State(state): State<MockState>,
        headers: HeaderMap,
        body: String,
    ) -> (StatusCode, String) {
        let mut requests = state.requests.lock();
        requests.push((headers, body));
        // 如果图片恢复错误地重新 acquire_context，balanced 模式会改用凭据 2。
        if state.change_priority && requests.len() == 1 {
            state.manager.set_priority(1, 100).unwrap();
        }
        state
            .responses
            .lock()
            .pop_front()
            .unwrap_or((StatusCode::BAD_REQUEST, "unexpected extra request".into()))
    }

    struct MockServer {
        provider: KiroProvider,
        state: MockState,
        task: JoinHandle<()>,
    }

    impl MockServer {
        async fn start(responses: Vec<(StatusCode, String)>, change_priority: bool) -> Self {
            let credentials = (1..=2)
                .map(|id| KiroCredentials {
                    id: Some(id),
                    kiro_api_key: Some(format!("ksk_test_{id}")),
                    machine_id: Some(format!("test-machine-{id}")),
                    subscription_title: Some("KIRO PRO".into()),
                    priority: id as u32 - 1,
                    ..Default::default()
                })
                .collect();
            let mut config = Config::default();
            config.load_balancing_mode = "balanced".into();
            let manager =
                Arc::new(MultiTokenManager::new(config, credentials, None, None, false).unwrap());
            let state = MockState {
                requests: Arc::new(Mutex::new(Vec::new())),
                responses: Arc::new(Mutex::new(responses.into())),
                manager: manager.clone(),
                change_priority,
            };
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let router = Router::new()
                .route("/", post(receive_request))
                .with_state(state.clone());
            let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let endpoint: Arc<dyn KiroEndpoint> =
                Arc::new(MockEndpoint(format!("http://{address}/")));
            let provider = KiroProvider::with_proxy(
                manager,
                None,
                HashMap::from([("mock".into(), endpoint)]),
                "mock".into(),
            );
            provider.client_cache.lock().insert(
                None,
                Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap(),
            );
            Self {
                provider,
                state,
                task,
            }
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn png(width: u32, height: u32) -> Value {
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(width, height)
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        json!({"format":"png", "source":{"bytes":STANDARD.encode(bytes.into_inner())}})
    }

    fn image_request(large: bool) -> String {
        let edge = if large { 2501 } else { 2000 };
        json!({
            "profileArn":"arn:test:original",
            "conversationState":{
                "conversationId":"same-conversation",
                "currentMessage":{"userInputMessage":{
                    "modelId":"opus-5.5", "content":"Continue", "images":[png(edge, 4)],
                    "userInputMessageContext":{
                        "tools":[{"toolSpecification":{"name":"screenshot", "description":"Capture", "inputSchema":{"json":{"type":"object"}}}}],
                        "toolResults":[{"toolUseId":"shot-2","content":[{"text":"Captured"}]}]
                    }
                }},
                "history":[
                    {"userInputMessage":{"content":"First screenshot", "modelId":"opus-5.5", "images":[png(4, edge)]}},
                    {"assistantResponseMessage":{"content":"Inspecting", "toolUses":[{"toolUseId":"shot-2", "name":"screenshot", "input":{}}]}}
                ]
            }
        })
        .to_string()
    }

    fn image_paths() -> [&'static str; 2] {
        [
            "/conversationState/currentMessage/userInputMessage/images/0",
            "/conversationState/history/0/userInputMessage/images/0",
        ]
    }

    #[tokio::test]
    async fn image_recovery_only_after_kiro_rejection_preserves_context_and_credential() {
        for is_stream in [false, true] {
            let server = MockServer::start(
                vec![
                    (StatusCode::BAD_REQUEST, DIMENSION_ERROR.into()),
                    (StatusCode::OK, "ok".into()),
                ],
                true,
            )
            .await;
            let original = image_request(true);
            let response = server
                .provider
                .call_api_with_retry(&original, is_stream)
                .await
                .unwrap();
            assert_eq!(response.text().await.unwrap(), "ok");
            let requests = server.state.requests.lock();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].1, original, "首次请求不能预先修改图片");
            assert_eq!(requests[0].0["authorization"], "Bearer ksk_test_1");
            assert_eq!(
                requests[1].0["authorization"],
                requests[0].0["authorization"]
            );
            assert_eq!(
                requests[1].0["x-test-machine-id"],
                requests[0].0["x-test-machine-id"]
            );
            let mut expected: Value = serde_json::from_str(&original).unwrap();
            let mut recovered: Value = serde_json::from_str(&requests[1].1).unwrap();
            for path in image_paths() {
                let image = recovered.pointer(path).unwrap();
                let bytes = STANDARD
                    .decode(image["source"]["bytes"].as_str().unwrap())
                    .unwrap();
                let decoded = image::load_from_memory(&bytes).unwrap();
                assert!(decoded.width() <= 2000 && decoded.height() <= 2000);
                *expected.pointer_mut(path).unwrap() = Value::Null;
                *recovered.pointer_mut(path).unwrap() = Value::Null;
            }
            assert_eq!(
                recovered, expected,
                "图片以外的历史、工具与请求字段必须保留"
            );
            let entries = server.state.manager.snapshot().entries;
            assert_eq!(entries[0].failure_count, 0);
            assert_eq!(entries[0].success_count, 1);
            assert_eq!(entries[1].success_count, 0);
            assert!(entries.iter().all(|entry| !entry.disabled));
        }
    }

    #[tokio::test]
    async fn accepted_images_are_not_resized() {
        let server = MockServer::start(vec![(StatusCode::OK, "ok".into())], false).await;
        let original = format!(" {} ", image_request(true));
        server.provider.call_api(&original).await.unwrap();
        let requests = server.state.requests.lock();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1, original);
    }

    #[tokio::test]
    async fn repeated_image_rejection_is_not_retried_or_counted_as_credential_failure() {
        let server = MockServer::start(
            vec![(StatusCode::BAD_REQUEST, DIMENSION_ERROR.into()); 2],
            false,
        )
        .await;
        let error = server
            .provider
            .call_api_stream(&image_request(true))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("IMAGE_DIMENSION_EXCEEDED"));
        assert_eq!(server.state.requests.lock().len(), 2);
        assert!(server.state.manager.snapshot().entries.iter().all(|entry| {
            entry.failure_count == 0 && entry.success_count == 0 && !entry.disabled
        }));
    }

    #[tokio::test]
    async fn unrelated_or_unrecoverable_image_errors_are_not_retried() {
        for (body, request) in [
            (
                DIMENSION_ERROR.replace("IMAGE_DIMENSION_EXCEEDED", "OTHER_REASON"),
                image_request(true),
            ),
            (
                json!({"reason":"IMAGE_DIMENSION_EXCEEDED", "message":"image too large"})
                    .to_string(),
                image_request(true),
            ),
            (DIMENSION_ERROR.into(), image_request(false)),
            (DIMENSION_ERROR.into(), r#"{"conversationState":{}}"#.into()),
        ] {
            let server =
                MockServer::start(vec![(StatusCode::BAD_REQUEST, body.clone())], false).await;
            let error = server.provider.call_api_stream(&request).await.unwrap_err();
            assert!(error.to_string().contains(&body));
            assert_eq!(server.state.requests.lock().len(), 1);
        }
    }

    #[tokio::test]
    async fn image_recovery_keeps_normal_transient_retry_policy() {
        let server = MockServer::start(
            vec![
                (StatusCode::BAD_REQUEST, DIMENSION_ERROR.into()),
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "temporarily unavailable".into(),
                ),
                (StatusCode::OK, "ok".into()),
            ],
            false,
        )
        .await;
        server
            .provider
            .call_api(&image_request(true))
            .await
            .unwrap();
        let requests = server.state.requests.lock();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[1].1, requests[2].1, "正常重试应复用已恢复的图片");
    }

    #[tokio::test]
    async fn image_recovery_is_attempted_once_across_normal_retries() {
        let server = MockServer::start(
            vec![
                (StatusCode::BAD_REQUEST, DIMENSION_ERROR.into()),
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "temporarily unavailable".into(),
                ),
                (
                    StatusCode::BAD_REQUEST,
                    DIMENSION_ERROR.replace("2000 pixels", "1000 pixels"),
                ),
            ],
            false,
        )
        .await;
        let error = server
            .provider
            .call_api(&image_request(true))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("1000 pixels"));
        let requests = server.state.requests.lock();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[1].1, requests[2].1);
    }

    #[tokio::test]
    async fn failed_image_decode_preserves_kiro_error_with_safe_context() {
        let server = MockServer::start(
            vec![(StatusCode::BAD_REQUEST, DIMENSION_ERROR.into())],
            false,
        )
        .await;
        let mut request: Value = serde_json::from_str(&image_request(true)).unwrap();
        request.pointer_mut(image_paths()[0]).unwrap()["source"]["bytes"] =
            json!("invalid-base64-private-image-content");
        let error = server
            .provider
            .call_api(&request.to_string())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(DIMENSION_ERROR));
        assert!(error.contains("自动图片缩放失败"));
        assert!(!error.contains("private-image-content"));
        assert_eq!(server.state.requests.lock().len(), 1);
    }
}
