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
