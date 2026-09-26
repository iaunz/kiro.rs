//! Admin API 路由配置

use axum::{
    Router, middleware,
    routing::{delete, get, post},
};

use super::{
    handlers::{
        add_credential, cancel_sso_session, delete_credential, force_refresh_token,
        get_all_credentials, get_credential_balance, get_credential_models,
        get_load_balancing_mode, get_sso_session, reset_failure_count, set_credential_disabled,
        set_credential_priority, set_load_balancing_mode, start_sso_session,
    },
    middleware::{AdminState, admin_auth_middleware},
};

/// 创建 Admin API 路由
///
/// # 端点
/// - `GET /credentials` - 获取所有凭据状态
/// - `POST /credentials` - 添加新凭据
/// - `DELETE /credentials/:id` - 删除凭据
/// - `POST /credentials/:id/disabled` - 设置凭据禁用状态
/// - `POST /credentials/:id/priority` - 设置凭据优先级
/// - `POST /credentials/:id/reset` - 重置失败计数
/// - `POST /credentials/:id/refresh` - 强制刷新 Token
/// - `GET /credentials/:id/balance` - 获取凭据余额
/// - `GET /credentials/:id/models` - 实时获取凭据的模型 ID
/// - `GET /config/load-balancing` - 获取负载均衡模式
/// - `PUT /config/load-balancing` - 设置负载均衡模式
/// - `POST /sso/sessions` - 发起 AWS SSO OIDC 自动导入会话
/// - `GET /sso/sessions/:id` - 查询 SSO 会话状态
/// - `DELETE /sso/sessions/:id` - 取消 SSO 会话
///
/// # 认证
/// 需要 Admin API Key 认证，支持：
/// - `x-api-key` header
/// - `Authorization: Bearer <token>` header
pub fn create_admin_router(state: AdminState) -> Router {
    Router::new()
        .route(
            "/credentials",
            get(get_all_credentials).post(add_credential),
        )
        .route("/credentials/{id}", delete(delete_credential))
        .route("/credentials/{id}/disabled", post(set_credential_disabled))
        .route("/credentials/{id}/priority", post(set_credential_priority))
        .route("/credentials/{id}/reset", post(reset_failure_count))
        .route("/credentials/{id}/refresh", post(force_refresh_token))
        .route("/credentials/{id}/balance", get(get_credential_balance))
        .route("/credentials/{id}/models", get(get_credential_models))
        .route(
            "/config/load-balancing",
            get(get_load_balancing_mode).put(set_load_balancing_mode),
        )
        .route("/sso/sessions", post(start_sso_session))
        .route(
            "/sso/sessions/{id}",
            get(get_sso_session).delete(cancel_sso_session),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::service::AdminService;
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::kiro::token_manager::MultiTokenManager;
    use crate::model::config::Config;
    use std::sync::Arc;

    #[tokio::test]
    async fn credential_models_require_admin_auth_and_report_lookup_errors() {
        let manager = Arc::new(
            MultiTokenManager::new(
                Config::default(),
                vec![KiroCredentials {
                    id: Some(7),
                    disabled: true,
                    ..Default::default()
                }],
                None,
                None,
                false,
            )
            .unwrap(),
        );
        let before = serde_json::to_value(manager.snapshot()).unwrap();
        let state = AdminState::new(
            "test-admin-key",
            AdminService::new(manager.clone(), Vec::new()),
        );
        let app = Router::new().nest("/api/admin", create_admin_router(state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let missing_url = format!("http://{address}/api/admin/credentials/99/models");

        for key in [None, Some("wrong-key")] {
            let mut request = client.get(&missing_url);
            if let Some(key) = key {
                request = request.header("x-api-key", key);
            }
            assert_eq!(
                request.send().await.unwrap().status(),
                reqwest::StatusCode::UNAUTHORIZED
            );
        }
        for request in [
            client
                .get(&missing_url)
                .header("x-api-key", "test-admin-key"),
            client.get(&missing_url).bearer_auth("test-admin-key"),
        ] {
            let response = request.send().await.unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
            assert_eq!(
                response.json::<serde_json::Value>().await.unwrap()["error"]["type"],
                "not_found"
            );
        }
        let response = client
            .get(format!("http://{address}/api/admin/credentials/7/models"))
            .bearer_auth("test-admin-key")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
        let body = response.json::<serde_json::Value>().await.unwrap();
        assert_eq!(body["error"]["type"], "api_error");
        assert!(body.get("models").is_none());
        assert_eq!(serde_json::to_value(manager.snapshot()).unwrap(), before);
        server.abort();
    }
}
