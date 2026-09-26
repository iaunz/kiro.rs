//! Anthropic API 路由配置

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};

use crate::kiro::provider::KiroProvider;

use super::{
    handlers::{count_tokens, get_models, post_messages, post_messages_cc},
    middleware::{AppState, auth_middleware, cors_layer},
    responses::post_response,
};

/// 请求体最大大小限制 (50MB)
const MAX_BODY_SIZE: usize = 50 * 1024 * 1024;

/// 创建 Anthropic API 路由
///
/// # 端点
/// - `GET /v1/models` - 获取可用模型列表
/// - `POST /v1/messages` - 创建消息（对话）
/// - `POST /v1/messages/count_tokens` - 计算 token 数量
/// - `POST /v1/responses` - OpenAI Responses API 兼容端点
/// - `GET /cc/v1/models` - 获取可用模型列表（与 /v1/models 相同）
///
/// # 认证
/// 所有 `/v1` 路径需要 API Key 认证，支持：
/// - `x-api-key` header
/// - `Authorization: Bearer <token>` header
///
/// # 参数
/// - `api_key`: API 密钥，用于验证客户端请求
/// - `kiro_provider`: 可选的 KiroProvider，用于调用上游 API

/// 创建带有 KiroProvider 的 Anthropic API 路由
pub fn create_router_with_provider(
    api_key: impl Into<String>,
    kiro_provider: Option<KiroProvider>,
    extract_thinking: bool,
) -> Router {
    let mut state = AppState::new(api_key, extract_thinking);
    if let Some(provider) = kiro_provider {
        state = state.with_kiro_provider(provider);
        // Warm the catalog without delaying startup; /models refreshes the same cache.
        let provider = state.kiro_provider.as_ref().unwrap().clone();
        tokio::spawn(async move {
            if let Ok(models) = provider.available_models().await {
                tracing::info!(count = models.len(), "已自动获取 Kiro 模型目录");
            }
        });
    }

    // 需要认证的 /v1 路由
    let v1_routes = Router::new()
        .route("/models", get(get_models))
        .route("/messages", post(post_messages))
        .route("/messages/count_tokens", post(count_tokens))
        .route("/responses", post(post_response))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    // 需要认证的 /cc/v1 路由（Claude Code 兼容端点）
    // 与 /v1 的区别：流式响应会等待 contextUsageEvent 后再发送 message_start
    let cc_v1_routes = Router::new()
        .route("/models", get(get_models))
        .route("/messages", post(post_messages_cc))
        .route("/messages/count_tokens", post(count_tokens))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    Router::new()
        .nest("/v1", v1_routes)
        .nest("/cc/v1", cc_v1_routes)
        .layer(cors_layer())
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, time::Duration};

    use reqwest::{Client, StatusCode};
    use serde_json::Value;
    use tokio::{net::TcpListener, task::JoinHandle};

    use super::*;

    const TEST_API_KEY: &str = "router-regression-test-key";

    struct TestServer {
        address: SocketAddr,
        task: JoinHandle<()>,
        client: Client,
    }

    impl TestServer {
        async fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = create_router_with_provider(TEST_API_KEY, None, true);
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let client = Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap();
            Self {
                address,
                task,
                client,
            }
        }

        fn url(&self, path: &str) -> String {
            format!("http://{}{path}", self.address)
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn message_body(path: &str, text_length: usize) -> String {
        let text = "x".repeat(text_length);
        if path == "/v1/responses" {
            format!(r#"{{"model":"claude-sonnet-4-6","input":"{text}"}}"#)
        } else {
            format!(
                r#"{{"model":"claude-sonnet-4-6","max_tokens":1,"messages":[{{"role":"user","content":"{text}"}}]}}"#
            )
        }
    }

    #[tokio::test]
    async fn message_routes_accept_json_above_axum_default_body_limit() {
        let server = TestServer::start().await;
        for path in ["/v1/responses", "/v1/messages", "/cc/v1/messages"] {
            let response = server
                .client
                .post(server.url(path))
                .bearer_auth(TEST_API_KEY)
                .header("content-type", "application/json")
                .body(message_body(path, 2 * 1024 * 1024 + 1))
                .send()
                .await
                .unwrap();
            // Reaching the handler without a provider proves the JSON extractor
            // accepted a real request larger than Axum's default 2 MiB limit.
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
            let body: Value = response.json().await.unwrap();
            assert_eq!(
                body["error"]["message"], "Kiro API provider not configured",
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn message_routes_reject_json_above_fifty_mebibytes() {
        let server = TestServer::start().await;
        for path in ["/v1/responses", "/v1/messages", "/cc/v1/messages"] {
            let response = server
                .client
                .post(server.url(path))
                .header("x-api-key", TEST_API_KEY)
                .header("content-type", "application/json")
                .body(message_body(path, MAX_BODY_SIZE))
                .send()
                .await
                .unwrap();
            // The surrounding JSON makes this valid request exceed the limit.
            assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE, "{path}");
        }
    }

    #[tokio::test]
    async fn message_and_model_routes_still_require_authentication() {
        let server = TestServer::start().await;
        for path in ["/v1/responses", "/v1/messages", "/cc/v1/messages"] {
            for key in [None, Some("incorrect-api-key")] {
                let mut request = server
                    .client
                    .post(server.url(path))
                    .header("content-type", "application/json")
                    .body(message_body(path, 1));
                if let Some(key) = key {
                    request = request.bearer_auth(key);
                }
                assert_eq!(
                    request.send().await.unwrap().status(),
                    StatusCode::UNAUTHORIZED,
                    "{path}"
                );
            }
        }
        for path in ["/v1/models", "/cc/v1/models"] {
            assert_eq!(
                server
                    .client
                    .get(server.url(path))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn model_routes_keep_static_catalog_without_provider() {
        let server = TestServer::start().await;
        for path in ["/v1/models", "/cc/v1/models"] {
            let response = server
                .client
                .get(server.url(path))
                .header("x-api-key", TEST_API_KEY)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let body: Value = response.json().await.unwrap();
            assert_eq!(body["object"], "list", "{path}");
            let models = body["data"].as_array().unwrap();
            assert!(!models.is_empty(), "{path}");
            assert!(
                models
                    .iter()
                    .any(|model| model["id"] == "claude-sonnet-4-6"),
                "{path}"
            );
        }
    }
}
