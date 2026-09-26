//! Anthropic API Handler 函数

use std::convert::Infallible;

use crate::kiro::model::events::Event;
use crate::kiro::model::requests::kiro::KiroRequest;
use crate::kiro::model_catalog::canonical_model_id;
use crate::kiro::parser::decoder::EventStreamDecoder;
use crate::token;
use anyhow::Error;
use axum::{
    Json as JsonExtractor,
    body::Body,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use bytes::Bytes;
use futures::{Stream, StreamExt, stream};
use serde_json::json;
use std::time::Duration;
use tokio::time::interval;
use uuid::Uuid;

use super::converter::{ConversionError, convert_request, map_model};
use super::middleware::AppState;
use super::stream::{BufferedStreamContext, SseEvent, StreamContext};
use super::types::{
    CountTokensRequest, CountTokensResponse, ErrorResponse, MessagesRequest, Model, ModelsResponse,
    OutputConfig, Thinking,
};
use super::websearch;

/// 将 KiroProvider 错误映射为 HTTP 响应
fn map_provider_error(err: Error) -> Response {
    let err_str = err.to_string();

    // 上下文窗口满了（对话历史累积超出模型上下文窗口限制）
    if err_str.contains("CONTENT_LENGTH_EXCEEDS_THRESHOLD") {
        tracing::warn!(error = %err, "上游拒绝请求：上下文窗口已满（不应重试）");
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "invalid_request_error",
                "Context window is full. Reduce conversation history, system prompt, or tools.",
            )),
        )
            .into_response();
    }

    // 单次输入太长（请求体本身超出上游限制）
    if err_str.contains("Input is too long") {
        tracing::warn!(error = %err, "上游拒绝请求：输入过长（不应重试）");
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "invalid_request_error",
                "Input is too long. Reduce the size of your messages.",
            )),
        )
            .into_response();
    }
    tracing::error!("Kiro API 调用失败: {}", err);
    (
        StatusCode::BAD_GATEWAY,
        Json(ErrorResponse::new(
            "api_error",
            format!("上游 API 调用失败: {}", err),
        )),
    )
        .into_response()
}

/// GET /v1/models
///
/// 返回可用的模型列表
pub async fn get_models(State(state): State<AppState>) -> impl IntoResponse {
    tracing::info!("Received GET /v1/models request");
    let mut models = static_models();
    if let Some(provider) = &state.kiro_provider {
        if let Ok(discovered) = provider.available_models().await {
            merge_discovered_models(&mut models, discovered);
        }
    }
    Json(ModelsResponse {
        object: "list".to_string(),
        data: models,
    })
}

fn static_models() -> Vec<Model> {
    let mut models = vec![
        Model {
            id: "gpt-5.6-sol".to_string(),
            object: "model".to_string(),
            created: 1784332800, // Jul 14, 2026
            owned_by: "openai".to_string(),
            display_name: "GPT-5.6 Sol".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(128_000),
        },
        Model {
            id: "gpt-5.6-terra".to_string(),
            object: "model".to_string(),
            created: 1784332800, // Jul 14, 2026
            owned_by: "openai".to_string(),
            display_name: "GPT-5.6 Terra".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(128_000),
        },
        Model {
            id: "gpt-5.6-luna".to_string(),
            object: "model".to_string(),
            created: 1784332800, // Jul 14, 2026
            owned_by: "openai".to_string(),
            display_name: "GPT-5.6 Luna".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(128_000),
        },
        Model {
            id: "claude-opus-5".to_string(),
            object: "model".to_string(),
            created: 1784937600, // Jul 25, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 5".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(128_000),
        },
        Model {
            id: "claude-opus-5-thinking".to_string(),
            object: "model".to_string(),
            created: 1784937600, // Jul 25, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 5 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(128_000),
        },
        Model {
            id: "claude-opus-4-8".to_string(),
            object: "model".to_string(),
            created: 1779897600, // May 28, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.8".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(128_000),
        },
        Model {
            id: "claude-opus-4-8-thinking".to_string(),
            object: "model".to_string(),
            created: 1779897600, // May 28, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.8 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(128_000),
        },
        Model {
            id: "claude-opus-4-7".to_string(),
            object: "model".to_string(),
            created: 1776276000, // Apr 16, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.7".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(64000),
        },
        Model {
            id: "claude-opus-4-7-thinking".to_string(),
            object: "model".to_string(),
            created: 1776276000, // Apr 16, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.7 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(64000),
        },
        Model {
            id: "claude-opus-4-6".to_string(),
            object: "model".to_string(),
            created: 1770163200, // Feb 4, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.6".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(64000),
        },
        Model {
            id: "claude-opus-4-6-thinking".to_string(),
            object: "model".to_string(),
            created: 1770163200, // Feb 4, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.6 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(64000),
        },
        Model {
            id: "claude-sonnet-4-6".to_string(),
            object: "model".to_string(),
            created: 1771286400, // Feb 17, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Sonnet 4.6".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(64000),
        },
        Model {
            id: "claude-sonnet-4-6-thinking".to_string(),
            object: "model".to_string(),
            created: 1771286400, // Feb 17, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Sonnet 4.6 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(64000),
        },
        Model {
            id: "claude-opus-4-5-20251101".to_string(),
            object: "model".to_string(),
            created: 1763942400, // Nov 24, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.5".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(64000),
        },
        Model {
            id: "claude-opus-4-5-20251101-thinking".to_string(),
            object: "model".to_string(),
            created: 1763942400, // Nov 24, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.5 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(64000),
        },
        Model {
            id: "claude-sonnet-4-5-20250929".to_string(),
            object: "model".to_string(),
            created: 1759104000, // Sep 29, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Sonnet 4.5".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(64000),
        },
        Model {
            id: "claude-sonnet-4-5-20250929-thinking".to_string(),
            object: "model".to_string(),
            created: 1759104000, // Sep 29, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Sonnet 4.5 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(64000),
        },
        Model {
            id: "claude-haiku-4-5-20251001".to_string(),
            object: "model".to_string(),
            created: 1760486400, // Oct 15, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Haiku 4.5".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(64000),
        },
        Model {
            id: "claude-haiku-4-5-20251001-thinking".to_string(),
            object: "model".to_string(),
            created: 1760486400, // Oct 15, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Haiku 4.5 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: Some(64000),
        },
    ];

    for (id, name) in [
        ("claude-sonnet-5", "Claude Sonnet 5"),
        ("claude-fable-5-1", "Claude Fable 5.1"),
    ] {
        for thinking in [false, true] {
            models.push(Model {
                id: if thinking {
                    format!("{id}-thinking")
                } else {
                    id.into()
                },
                object: "model".into(),
                created: 0,
                owned_by: "anthropic".into(),
                display_name: if thinking {
                    format!("{name} (Thinking)")
                } else {
                    name.into()
                },
                model_type: "chat".into(),
                max_tokens: Some(128_000),
            });
        }
    }
    models
}

fn merge_discovered_models(
    models: &mut Vec<Model>,
    mut discovered: Vec<crate::kiro::model::available_models::UpstreamModel>,
) {
    // If both spellings are advertised upstream, use the prefixed entry's metadata.
    discovered.sort_by_key(|model| {
        canonical_model_id(&model.model_id) != model.model_id.trim().to_ascii_lowercase()
    });
    let mut discovered_ids = std::collections::HashSet::new();
    for model in discovered {
        let id = canonical_model_id(&model.model_id);
        if !discovered_ids.insert(id.to_ascii_lowercase()) {
            continue;
        }
        let matches_model = |existing: &Model| {
            let base_id = existing
                .id
                .strip_suffix("-thinking")
                .unwrap_or(&existing.id);
            canonical_model_id(base_id).eq_ignore_ascii_case(&id)
                || map_model(&existing.id)
                    .is_some_and(|mapped| canonical_model_id(&mapped).eq_ignore_ascii_case(&id))
        };
        let output_limit = model
            .token_limits
            .as_ref()
            .and_then(|limits| limits.max_output_tokens)
            .and_then(|limit| i32::try_from(limit).ok())
            .filter(|limit| *limit > 0)
            .or_else(|| {
                models.iter().find_map(|existing| {
                    matches_model(existing)
                        .then_some(existing.max_tokens)
                        .flatten()
                })
            });
        // Update the limits of existing compatibility aliases as well as the exact ID.
        for existing in models.iter_mut() {
            if matches_model(existing) {
                if let Some(limit) = output_limit {
                    existing.max_tokens = Some(limit);
                }
            }
        }
        if models
            .iter()
            .any(|existing| existing.id.eq_ignore_ascii_case(&id))
        {
            continue;
        }
        let owner = if id.starts_with("claude-") {
            "anthropic"
        } else if id.starts_with("gpt-") {
            "openai"
        } else {
            "kiro"
        };
        models.push(Model {
            display_name: model
                .model_name
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| id.clone()),
            id,
            object: "model".into(),
            created: 0,
            owned_by: owner.into(),
            model_type: "chat".into(),
            max_tokens: output_limit,
        });
    }

    // Only advertise aliases here; request conversion and thinking behavior stay unchanged.
    let mut ids: std::collections::HashSet<_> = models
        .iter()
        .map(|model| model.id.to_ascii_lowercase())
        .collect();
    let mut thinking_models = Vec::new();
    for model in models.iter() {
        let model_lower = model.id.to_ascii_lowercase();
        if model_lower.ends_with("-thinking")
            || !model_lower.strip_prefix("claude-").is_some_and(|name| {
                name.split('-')
                    .next()
                    .is_some_and(|family| matches!(family, "fable" | "sonnet" | "opus" | "haiku"))
            })
        {
            continue;
        }
        let id = format!("{}-thinking", model.id);
        if ids.insert(id.to_ascii_lowercase()) {
            thinking_models.push(Model {
                id,
                object: model.object.clone(),
                created: model.created,
                owned_by: model.owned_by.clone(),
                display_name: format!("{} (Thinking)", model.display_name),
                model_type: model.model_type.clone(),
                max_tokens: model.max_tokens,
            });
        }
    }
    models.extend(thinking_models);
}

/// POST /v1/messages
///
/// 创建消息（对话）
pub async fn post_messages(
    State(state): State<AppState>,
    JsonExtractor(mut payload): JsonExtractor<MessagesRequest>,
) -> Response {
    tracing::info!(
        model = %payload.model,
        max_tokens = %payload.max_tokens,
        stream = %payload.stream,
        message_count = %payload.messages.len(),
        "Received POST /v1/messages request"
    );
    // 检查 KiroProvider 是否可用
    let provider = match &state.kiro_provider {
        Some(p) => p.clone(),
        None => {
            tracing::error!("KiroProvider 未配置");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse::new(
                    "service_unavailable",
                    "Kiro API provider not configured",
                )),
            )
                .into_response();
        }
    };

    // 检测模型名是否包含 "thinking" 后缀，若包含则覆写 thinking 配置
    override_thinking_from_model_name(&mut payload);

    // 检查是否为 WebSearch 请求
    if websearch::has_web_search_tool(&payload) {
        tracing::info!("检测到 WebSearch 工具，路由到 WebSearch 处理");

        // 估算输入 tokens
        let input_tokens = token::count_all_tokens(
            payload.model.clone(),
            payload.system.clone(),
            payload.messages.clone(),
            payload.tools.clone(),
        ) as i32;

        return websearch::handle_websearch_request(provider, &payload, input_tokens).await;
    }

    // 转换请求
    let conversion_result = match convert_request(&payload) {
        Ok(result) => result,
        Err(e) => {
            let (error_type, message) = match &e {
                ConversionError::UnsupportedModel(model) => {
                    ("invalid_request_error", format!("模型不支持: {}", model))
                }
                ConversionError::EmptyMessages => {
                    ("invalid_request_error", "消息列表为空".to_string())
                }
            };
            tracing::warn!("请求转换失败: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(error_type, message)),
            )
                .into_response();
        }
    };

    // 构建 Kiro 请求（profile_arn 由 provider 层根据实际凭据注入）
    let kiro_request = KiroRequest {
        conversation_state: conversion_result.conversation_state,
        profile_arn: None,
    };

    let request_body = match serde_json::to_string(&kiro_request) {
        Ok(body) => body,
        Err(e) => {
            tracing::error!("序列化请求失败: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    "internal_error",
                    format!("序列化请求失败: {}", e),
                )),
            )
                .into_response();
        }
    };

    tracing::debug!("Kiro request body: {}", request_body);

    // 估算输入 tokens
    let input_tokens = token::count_all_tokens(
        payload.model.clone(),
        payload.system,
        payload.messages,
        payload.tools,
    ) as i32;

    // 检查是否启用了thinking
    let thinking_enabled = should_extract_thinking(&payload.model, &payload.thinking);

    let tool_name_map = conversion_result.tool_name_map;

    if payload.stream {
        // 流式响应
        handle_stream_request(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            thinking_enabled,
            tool_name_map,
        )
        .await
    } else {
        // 非流式响应：仅在配置开启时提取 thinking 块
        let extract_thinking = state.extract_thinking && thinking_enabled;
        handle_non_stream_request(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            extract_thinking,
            tool_name_map,
        )
        .await
    }
}

/// 处理流式请求
async fn handle_stream_request(
    provider: std::sync::Arc<crate::kiro::provider::KiroProvider>,
    request_body: &str,
    model: &str,
    input_tokens: i32,
    thinking_enabled: bool,
    tool_name_map: std::collections::HashMap<String, String>,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let response = match provider.call_api_stream(request_body).await {
        Ok(resp) => resp,
        Err(e) => return map_provider_error(e),
    };

    // 创建流处理上下文
    let mut ctx =
        StreamContext::new_with_thinking(model, input_tokens, thinking_enabled, tool_name_map);

    // 生成初始事件
    let initial_events = ctx.generate_initial_events();

    // 创建 SSE 流
    let stream = create_sse_stream(response, ctx, initial_events);

    // 返回 SSE 响应
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// Ping 事件间隔（25秒）
const PING_INTERVAL_SECS: u64 = 25;

/// 创建 ping 事件的 SSE 字符串
fn create_ping_sse() -> Bytes {
    Bytes::from("event: ping\ndata: {\"type\": \"ping\"}\n\n")
}

/// 创建 SSE 事件流
fn create_sse_stream(
    response: reqwest::Response,
    ctx: StreamContext,
    initial_events: Vec<SseEvent>,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
    // 先发送初始事件
    let initial_stream = stream::iter(
        initial_events
            .into_iter()
            .map(|e| Ok(Bytes::from(e.to_sse_string()))),
    );

    // 然后处理 Kiro 响应流，同时每25秒发送 ping 保活
    let body_stream = response.bytes_stream();

    let processing_stream = stream::unfold(
        (body_stream, ctx, EventStreamDecoder::new(), false, interval(Duration::from_secs(PING_INTERVAL_SECS))),
        |(mut body_stream, mut ctx, mut decoder, finished, mut ping_interval)| async move {
            if finished {
                return None;
            }

            // 使用 select! 同时等待数据和 ping 定时器
            tokio::select! {
                // 处理数据流
                chunk_result = body_stream.next() => {
                    match chunk_result {
                        Some(Ok(chunk)) => {
                            // 解码事件
                            if let Err(e) = decoder.feed(&chunk) {
                                tracing::warn!("缓冲区溢出: {}", e);
                            }

                            let mut events = Vec::new();
                            for result in decoder.decode_iter() {
                                match result {
                                    Ok(frame) => {
                                        if let Ok(event) = Event::from_frame(frame) {
                                            let sse_events = ctx.process_kiro_event(&event);
                                            events.extend(sse_events);
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("解码事件失败: {}", e);
                                    }
                                }
                            }

                            // 转换为 SSE 字节流
                            let bytes: Vec<Result<Bytes, Infallible>> = events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();

                            Some((stream::iter(bytes), (body_stream, ctx, decoder, false, ping_interval)))
                        }
                        Some(Err(e)) => {
                            tracing::error!("读取响应流失败: {}", e);
                            // 发送最终事件并结束
                            let final_events = ctx.generate_final_events();
                            let bytes: Vec<Result<Bytes, Infallible>> = final_events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();
                            Some((stream::iter(bytes), (body_stream, ctx, decoder, true, ping_interval)))
                        }
                        None => {
                            // 流结束，发送最终事件
                            let final_events = ctx.generate_final_events();
                            let bytes: Vec<Result<Bytes, Infallible>> = final_events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();
                            Some((stream::iter(bytes), (body_stream, ctx, decoder, true, ping_interval)))
                        }
                    }
                }
                // 发送 ping 保活
                _ = ping_interval.tick() => {
                    tracing::trace!("发送 ping 保活事件");
                    let bytes: Vec<Result<Bytes, Infallible>> = vec![Ok(create_ping_sse())];
                    Some((stream::iter(bytes), (body_stream, ctx, decoder, false, ping_interval)))
                }
            }
        },
    )
    .flatten();

    initial_stream.chain(processing_stream)
}

use super::converter::get_context_window_size;

/// 处理非流式请求
async fn handle_non_stream_request(
    provider: std::sync::Arc<crate::kiro::provider::KiroProvider>,
    request_body: &str,
    model: &str,
    input_tokens: i32,
    thinking_enabled: bool,
    tool_name_map: std::collections::HashMap<String, String>,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let response = match provider.call_api(request_body).await {
        Ok(resp) => resp,
        Err(e) => return map_provider_error(e),
    };

    // 读取响应体
    let body_bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::error!("读取响应体失败: {}", e);
            return (
                StatusCode::BAD_GATEWAY,
                Json(ErrorResponse::new(
                    "api_error",
                    format!("读取响应失败: {}", e),
                )),
            )
                .into_response();
        }
    };

    // 解析事件流
    let mut decoder = EventStreamDecoder::new();
    if let Err(e) = decoder.feed(&body_bytes) {
        tracing::warn!("缓冲区溢出: {}", e);
    }

    let mut text_content = String::new();
    let mut tool_uses: Vec<serde_json::Value> = Vec::new();
    let mut has_tool_use = false;
    let mut stop_reason = "end_turn".to_string();
    // 从 contextUsageEvent 计算的实际输入 tokens
    let mut context_input_tokens: Option<i32> = None;

    // 收集工具调用的增量 JSON
    let mut tool_json_buffers: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for result in decoder.decode_iter() {
        match result {
            Ok(frame) => {
                if let Ok(event) = Event::from_frame(frame) {
                    match event {
                        Event::AssistantResponse(resp) => {
                            text_content.push_str(&resp.content);
                        }
                        Event::ToolUse(tool_use) => {
                            has_tool_use = true;

                            // 累积工具的 JSON 输入
                            let buffer = tool_json_buffers
                                .entry(tool_use.tool_use_id.clone())
                                .or_insert_with(String::new);
                            buffer.push_str(&tool_use.input);

                            // 如果是完整的工具调用，添加到列表
                            if tool_use.stop {
                                let input: serde_json::Value = if buffer.is_empty() {
                                    serde_json::json!({})
                                } else {
                                    serde_json::from_str(buffer).unwrap_or_else(|e| {
                                        tracing::warn!(
                                            "工具输入 JSON 解析失败: {}, tool_use_id: {}",
                                            e,
                                            tool_use.tool_use_id
                                        );
                                        serde_json::json!({})
                                    })
                                };

                                let original_name = tool_name_map
                                    .get(&tool_use.name)
                                    .cloned()
                                    .unwrap_or_else(|| tool_use.name.clone());

                                tool_uses.push(json!({
                                    "type": "tool_use",
                                    "id": tool_use.tool_use_id,
                                    "name": original_name,
                                    "input": input
                                }));
                            }
                        }
                        Event::ContextUsage(context_usage) => {
                            // 从上下文使用百分比计算实际的 input_tokens
                            let window_size = get_context_window_size(model);
                            let actual_input_tokens =
                                (context_usage.context_usage_percentage * (window_size as f64)
                                    / 100.0) as i32;
                            context_input_tokens = Some(actual_input_tokens);
                            // 上下文使用量达到 100% 时，设置 stop_reason 为 model_context_window_exceeded
                            if context_usage.context_usage_percentage >= 100.0 {
                                stop_reason = "model_context_window_exceeded".to_string();
                            }
                            tracing::debug!(
                                "收到 contextUsageEvent: {}%, 计算 input_tokens: {}",
                                context_usage.context_usage_percentage,
                                actual_input_tokens
                            );
                        }
                        Event::Exception { exception_type, .. } => {
                            if exception_type == "ContentLengthExceededException" {
                                stop_reason = "max_tokens".to_string();
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                tracing::warn!("解码事件失败: {}", e);
            }
        }
    }

    // 确定 stop_reason
    if has_tool_use && stop_reason == "end_turn" {
        stop_reason = "tool_use".to_string();
    }

    // 构建响应内容
    let mut content: Vec<serde_json::Value> = Vec::new();

    if thinking_enabled {
        // 从完整文本中提取 thinking 块
        let (thinking, remaining_text) =
            super::stream::extract_thinking_from_complete_text(&text_content);

        if let Some(thinking_text) = thinking {
            content.push(json!({
                "type": "thinking",
                "thinking": thinking_text
            }));
        }

        if !remaining_text.is_empty() {
            content.push(json!({
                "type": "text",
                "text": remaining_text
            }));
        }
    } else if !text_content.is_empty() {
        content.push(json!({
            "type": "text",
            "text": text_content
        }));
    }

    content.extend(tool_uses);

    // 估算输出 tokens
    let output_tokens = token::estimate_output_tokens(&content);

    // 使用从 contextUsageEvent 计算的 input_tokens，如果没有则使用估算值
    let final_input_tokens = context_input_tokens.unwrap_or(input_tokens);

    // 构建 Anthropic 响应
    let response_body = json!({
        "id": format!("msg_{}", Uuid::new_v4().to_string().replace('-', "")),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": final_input_tokens,
            "output_tokens": output_tokens
        }
    });

    (StatusCode::OK, Json(response_body)).into_response()
}

/// Sonnet 5 / Opus 5（含裸别名）默认拆分上游的 thinking 标签，除非显式禁用。
/// GPT-5.6 使用隐藏推理，其他模型仅在客户端显式开启时拆分。
pub(crate) fn should_extract_thinking(model: &str, thinking: &Option<Thinking>) -> bool {
    let model_lower = model.to_lowercase();
    if model_lower.contains("gpt-5.6") || model_lower.contains("gpt-5-6") {
        return false;
    }

    if matches!(
        map_model(model)
            .map(|model| canonical_model_id(&model))
            .as_deref(),
        Some("claude-sonnet-5") | Some("claude-opus-5")
    ) {
        thinking
            .as_ref()
            .map(|t| t.thinking_type != "disabled")
            .unwrap_or(true)
    } else {
        thinking.as_ref().map(Thinking::is_enabled).unwrap_or(false)
    }
}

/// 检测模型名是否包含 "thinking" 后缀，若包含则覆写 thinking 配置
///
/// - Opus 4.6 / Opus 5 / Sonnet 5 / Fable 5.1：覆写为 adaptive 类型
/// - GPT-5.6：保留隐藏推理，不注入 Claude thinking 配置
/// - 其他模型：覆写为 enabled 类型
/// - budget_tokens 固定为 20000
fn override_thinking_from_model_name(payload: &mut MessagesRequest) {
    let model_lower = payload.model.to_lowercase();
    if !model_lower.contains("thinking") {
        return;
    }

    if model_lower.contains("gpt-5.6") || model_lower.contains("gpt-5-6") {
        return;
    }

    let is_adaptive_thinking = matches!(
        map_model(&payload.model)
            .map(|model| canonical_model_id(&model))
            .as_deref(),
        Some("claude-opus-4.6")
            | Some("claude-opus-5")
            | Some("claude-sonnet-5")
            | Some("claude-fable-5.1")
    );

    let thinking_type = if is_adaptive_thinking {
        "adaptive"
    } else {
        "enabled"
    };

    tracing::info!(
        model = %payload.model,
        thinking_type = thinking_type,
        "模型名包含 thinking 后缀，覆写 thinking 配置"
    );

    payload.thinking = Some(Thinking {
        thinking_type: thinking_type.to_string(),
        budget_tokens: 20000,
    });

    if is_adaptive_thinking {
        payload.output_config = Some(OutputConfig {
            effort: "high".to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovered_claude_families_advertise_matching_thinking_models() {
        let mut models = static_models();
        let discovered = serde_json::from_value(json!([
            {"modelId":"claude-opus-5.5", "modelName":"Claude Opus 5.5", "tokenLimits":{"maxOutputTokens":128000}},
            {"modelId":"opus-5.5"},
            {"modelId":"claude-fable-5.1"},
            {"modelId":"claude-sonnet-4.6"},
            {"modelId":"claude-haiku-4.5"},
            {"modelId":"CLAUDE-SONNET-6"},
            {"modelId":"auto"},
            {"modelId":"gpt-5.6-sol"},
            {"modelId":"minimax-m2.5"},
            {"modelId":"qwen3-coder-next"},
            {"modelId":"notfable-1"}
        ])).unwrap();
        merge_discovered_models(&mut models, discovered);
        assert!(!models.iter().any(|model| model.id == "opus-5.5"));
        assert_eq!(
            models
                .iter()
                .filter(|model| model.id == "claude-opus-5.5")
                .count(),
            1
        );

        for id in [
            "claude-opus-5.5",
            "claude-fable-5.1",
            "claude-sonnet-4.6",
            "claude-haiku-4.5",
            "claude-sonnet-6",
        ] {
            let base = models.iter().find(|model| model.id == id).unwrap();
            let thinking_id = format!("{id}-thinking");
            let thinking = models.iter().find(|model| model.id == thinking_id).unwrap();
            let mut expected = serde_json::to_value(base).unwrap();
            expected["id"] = json!(thinking_id);
            expected["display_name"] = json!(format!("{} (Thinking)", base.display_name));
            assert_eq!(serde_json::to_value(thinking).unwrap(), expected);
        }
        for id in [
            "auto",
            "gpt-5.6-sol",
            "minimax-m2.5",
            "qwen3-coder-next",
            "notfable-1",
        ] {
            assert!(models.iter().any(|model| model.id == id));
            assert!(
                !models
                    .iter()
                    .any(|model| model.id == format!("{id}-thinking"))
            );
        }
    }

    #[test]
    fn unprefixed_families_are_listed_with_claude_prefix_and_thinking_variant() {
        let mut models = static_models();
        let discovered = serde_json::from_value(json!([
            {"modelId":"opus-5.5", "tokenLimits":{"maxOutputTokens":128000}},
            {"modelId":"fable-6.1"},
            {"modelId":"sonnet-7"},
            {"modelId":"HAIKU-6"},
            {"modelId":"custom-opus-1"}
        ]))
        .unwrap();
        merge_discovered_models(&mut models, discovered);
        for bare in ["opus-5.5", "fable-6.1", "sonnet-7", "haiku-6"] {
            for suffix in ["", "-thinking"] {
                let id = format!("claude-{bare}{suffix}");
                let model = models.iter().find(|model| model.id == id).unwrap();
                assert_eq!(model.owned_by, "anthropic");
                assert!(
                    !models
                        .iter()
                        .any(|model| model.id.eq_ignore_ascii_case(&format!("{bare}{suffix}")))
                );
            }
        }
        assert!(models.iter().any(|model| model.id == "custom-opus-1"));
        assert!(
            !models
                .iter()
                .any(|model| model.id == "custom-opus-1-thinking")
        );
    }

    #[test]
    fn canonical_discovered_models_merge_both_prefix_forms_independent_of_order() {
        for reverse in [false, true] {
            let mut models = static_models();
            let mut discovered: Vec<crate::kiro::model::available_models::UpstreamModel> = serde_json::from_value(json!([
                {"modelId":"opus-5.5", "modelName":"Bare Opus", "tokenLimits":{"maxOutputTokens":64000}},
                {"modelId":"claude-opus-5.5", "modelName":"Canonical Opus", "tokenLimits":{"maxOutputTokens":128000}},
                {"modelId":"sonnet-5", "tokenLimits":{"maxOutputTokens":32000}},
                {"modelId":"claude-sonnet-5", "tokenLimits":{"maxOutputTokens":96000}}
            ])).unwrap();
            if reverse {
                discovered.reverse();
            }
            merge_discovered_models(&mut models, discovered);
            for (base, expected_limit) in [("claude-opus-5.5", 128000), ("claude-sonnet-5", 96000)]
            {
                for suffix in ["", "-thinking"] {
                    let id = format!("{base}{suffix}");
                    assert_eq!(models.iter().filter(|model| model.id == id).count(), 1);
                    let model = models.iter().find(|model| model.id == id).unwrap();
                    assert_eq!(model.max_tokens, Some(expected_limit));
                }
            }
            assert_eq!(
                models
                    .iter()
                    .find(|model| model.id == "claude-opus-5.5")
                    .unwrap()
                    .display_name,
                "Canonical Opus"
            );
            assert!(
                !models
                    .iter()
                    .any(|model| model.id.starts_with("opus-") || model.id.starts_with("sonnet-"))
            );
        }
    }

    #[test]
    fn thinking_model_expansion_preserves_existing_entries_without_duplicate_suffixes() {
        for reverse in [false, true] {
            let mut models = static_models();
            let mut discovered: Vec<crate::kiro::model::available_models::UpstreamModel> =
                serde_json::from_value(json!([
                    {"modelId":"claude-opus-5.5"},
                    {"modelId":"CLAUDE-OPUS-5.5-THINKING", "modelName":"Upstream Thinking"},
                    {"modelId":"claude-sonnet-5"},
                    {"modelId":"claude-sonnet-5"}
                ]))
                .unwrap();
            if reverse {
                discovered.reverse();
            }
            merge_discovered_models(&mut models, discovered.clone());
            let first = serde_json::to_value(&models).unwrap();
            merge_discovered_models(&mut models, discovered);
            assert_eq!(serde_json::to_value(&models).unwrap(), first);
            let ids: std::collections::HashSet<_> = models
                .iter()
                .map(|model| model.id.to_ascii_lowercase())
                .collect();
            assert_eq!(ids.len(), models.len());
            assert!(!ids.iter().any(|id| id.ends_with("-thinking-thinking")));
            assert_eq!(
                models
                    .iter()
                    .find(|model| model.id == "claude-opus-5.5-thinking")
                    .unwrap()
                    .display_name,
                "Upstream Thinking"
            );
        }
    }

    #[test]
    fn discovered_models_keep_aliases_and_inherit_known_output_limits() {
        let mut models = static_models();
        let discovered = serde_json::from_value(json!([
            {"modelId":"claude-opus-4.8", "modelName":"Claude Opus 4.8", "tokenLimits":{"maxInputTokens":1000000}},
            {"modelId":"new-upstream-model", "modelName":"New model", "tokenLimits":{"maxInputTokens":250000}},
            {"modelId":"new-upstream-model"}
        ])).unwrap();
        merge_discovered_models(&mut models, discovered);
        assert!(models.iter().any(|m| m.id == "claude-opus-4-8-thinking"));
        assert_eq!(
            models
                .iter()
                .find(|m| m.id == "claude-opus-4.8")
                .unwrap()
                .max_tokens,
            Some(128_000)
        );
        assert_eq!(
            models
                .iter()
                .filter(|m| m.id == "new-upstream-model")
                .count(),
            1
        );
        let new_model = models
            .iter()
            .find(|m| m.id == "new-upstream-model")
            .unwrap();
        assert_eq!(new_model.display_name, "New model");
        assert!(
            serde_json::to_value(new_model)
                .unwrap()
                .get("max_tokens")
                .is_none()
        );
    }

    #[test]
    fn discovered_output_limits_update_existing_static_ids_and_aliases() {
        let mut models = static_models();
        let discovered = serde_json::from_value(json!([
            {"modelId":"claude-sonnet-5", "tokenLimits":{"maxOutputTokens":96000}},
            {"modelId":"claude-opus-4.8", "tokenLimits":{"maxOutputTokens":32000}}
        ]))
        .unwrap();
        merge_discovered_models(&mut models, discovered);
        for id in ["claude-sonnet-5", "claude-sonnet-5-thinking"] {
            assert_eq!(
                models.iter().find(|m| m.id == id).unwrap().max_tokens,
                Some(96_000)
            );
        }
        for id in [
            "claude-opus-4.8",
            "claude-opus-4-8",
            "claude-opus-4-8-thinking",
        ] {
            assert_eq!(
                models.iter().find(|m| m.id == id).unwrap().max_tokens,
                Some(32_000)
            );
        }
        assert_eq!(
            models.iter().filter(|m| m.id == "claude-sonnet-5").count(),
            1
        );
    }

    fn request(model: &str) -> MessagesRequest {
        serde_json::from_value(json!({
            "model": model,
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .unwrap()
    }

    #[test]
    fn adaptive_families_extract_thinking_by_default_and_honor_disabled() {
        for model in [
            "sonnet",
            " OPUS ",
            "SONNET",
            "claude-sonnet-5",
            "claude-opus-5-thinking",
        ] {
            assert!(should_extract_thinking(model, &None), "{model}");
            assert!(
                !should_extract_thinking(
                    model,
                    &Some(Thinking {
                        thinking_type: "disabled".to_string(),
                        budget_tokens: 0,
                    })
                ),
                "{model}"
            );

            // 裸别名的默认响应拆分不应向请求注入 thinking 控制参数。
            if !model.contains("thinking") {
                let mut payload = request(model);
                override_thinking_from_model_name(&mut payload);
                assert!(payload.thinking.is_none());
                assert!(payload.output_config.is_none());
            }
        }
    }

    #[test]
    fn gpt_hidden_cot_ignores_thinking_suffix_and_explicit_configuration() {
        for model in [
            "gpt-5.6-sol-thinking",
            "openai.gpt-5-6-terra-thinking",
            "GPT-5.6-LUNA",
        ] {
            let mut payload = request(model);
            override_thinking_from_model_name(&mut payload);
            assert!(payload.thinking.is_none());
            assert!(payload.output_config.is_none());

            for thinking_type in ["enabled", "adaptive"] {
                payload.thinking = Some(Thinking {
                    thinking_type: thinking_type.to_string(),
                    budget_tokens: 1234,
                });
                override_thinking_from_model_name(&mut payload);
                assert!(
                    !should_extract_thinking(model, &payload.thinking),
                    "{model}"
                );
                assert_eq!(payload.thinking.as_ref().unwrap().budget_tokens, 1234);
            }
        }
    }

    #[test]
    fn thinking_suffix_selects_supported_claude_mode() {
        for (model, expected_type) in [
            ("claude-opus-4-6-thinking", "adaptive"),
            ("claude-opus-5-thinking", "adaptive"),
            ("claude-sonnet-5-thinking", "adaptive"),
            ("claude-fable-5-1-thinking", "adaptive"),
            ("claude-fable-5.1-thinking", "adaptive"),
            ("claude-sonnet-4-5-thinking", "enabled"),
            ("claude-haiku-4-5-thinking", "enabled"),
        ] {
            let mut payload = request(model);
            override_thinking_from_model_name(&mut payload);
            let thinking = payload.thinking.as_ref().unwrap();
            assert_eq!(thinking.thinking_type, expected_type, "{model}");
            assert_eq!(thinking.budget_tokens, 20000);
            assert_eq!(
                payload.output_config.as_ref().map(|o| o.effort.as_str()),
                if expected_type == "adaptive" {
                    Some("high")
                } else {
                    None
                }
            );
            assert!(should_extract_thinking(model, &payload.thinking));
        }
    }

    #[test]
    fn other_claude_models_require_explicit_thinking() {
        for model in ["haiku", "claude-sonnet-4-6", "claude-fable-5-1"] {
            assert!(!should_extract_thinking(model, &None));
            assert!(should_extract_thinking(
                model,
                &Some(Thinking {
                    thinking_type: "enabled".to_string(),
                    budget_tokens: 20000,
                })
            ));
        }
    }
}

/// POST /v1/messages/count_tokens
///
/// 计算消息的 token 数量
pub async fn count_tokens(
    JsonExtractor(payload): JsonExtractor<CountTokensRequest>,
) -> impl IntoResponse {
    tracing::info!(
        model = %payload.model,
        message_count = %payload.messages.len(),
        "Received POST /v1/messages/count_tokens request"
    );

    let total_tokens = token::count_all_tokens(
        payload.model,
        payload.system,
        payload.messages,
        payload.tools,
    ) as i32;

    Json(CountTokensResponse {
        input_tokens: total_tokens.max(1) as i32,
    })
}

/// POST /cc/v1/messages
///
/// Claude Code 兼容端点，与 /v1/messages 的区别在于：
/// - 流式响应会等待 kiro 端返回 contextUsageEvent 后再发送 message_start
/// - message_start 中的 input_tokens 是从 contextUsageEvent 计算的准确值
pub async fn post_messages_cc(
    State(state): State<AppState>,
    JsonExtractor(mut payload): JsonExtractor<MessagesRequest>,
) -> Response {
    tracing::info!(
        model = %payload.model,
        max_tokens = %payload.max_tokens,
        stream = %payload.stream,
        message_count = %payload.messages.len(),
        "Received POST /cc/v1/messages request"
    );

    // 检查 KiroProvider 是否可用
    let provider = match &state.kiro_provider {
        Some(p) => p.clone(),
        None => {
            tracing::error!("KiroProvider 未配置");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse::new(
                    "service_unavailable",
                    "Kiro API provider not configured",
                )),
            )
                .into_response();
        }
    };

    // 检测模型名是否包含 "thinking" 后缀，若包含则覆写 thinking 配置
    override_thinking_from_model_name(&mut payload);

    // 检查是否为 WebSearch 请求
    if websearch::has_web_search_tool(&payload) {
        tracing::info!("检测到 WebSearch 工具，路由到 WebSearch 处理");

        // 估算输入 tokens
        let input_tokens = token::count_all_tokens(
            payload.model.clone(),
            payload.system.clone(),
            payload.messages.clone(),
            payload.tools.clone(),
        ) as i32;

        return websearch::handle_websearch_request(provider, &payload, input_tokens).await;
    }

    // 转换请求
    let conversion_result = match convert_request(&payload) {
        Ok(result) => result,
        Err(e) => {
            let (error_type, message) = match &e {
                ConversionError::UnsupportedModel(model) => {
                    ("invalid_request_error", format!("模型不支持: {}", model))
                }
                ConversionError::EmptyMessages => {
                    ("invalid_request_error", "消息列表为空".to_string())
                }
            };
            tracing::warn!("请求转换失败: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(error_type, message)),
            )
                .into_response();
        }
    };

    // 构建 Kiro 请求（profile_arn 由 provider 层根据实际凭据注入）
    let kiro_request = KiroRequest {
        conversation_state: conversion_result.conversation_state,
        profile_arn: None,
    };

    let request_body = match serde_json::to_string(&kiro_request) {
        Ok(body) => body,
        Err(e) => {
            tracing::error!("序列化请求失败: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    "internal_error",
                    format!("序列化请求失败: {}", e),
                )),
            )
                .into_response();
        }
    };

    tracing::debug!("Kiro request body: {}", request_body);

    // 估算输入 tokens
    let input_tokens = token::count_all_tokens(
        payload.model.clone(),
        payload.system,
        payload.messages,
        payload.tools,
    ) as i32;

    // 检查是否启用了thinking
    let thinking_enabled = should_extract_thinking(&payload.model, &payload.thinking);

    let tool_name_map = conversion_result.tool_name_map;

    if payload.stream {
        // 流式响应（缓冲模式）
        handle_stream_request_buffered(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            thinking_enabled,
            tool_name_map,
        )
        .await
    } else {
        // 非流式响应：仅在配置开启时提取 thinking 块
        let extract_thinking = state.extract_thinking && thinking_enabled;
        handle_non_stream_request(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            extract_thinking,
            tool_name_map,
        )
        .await
    }
}

/// 处理流式请求（缓冲版本）
///
/// 与 `handle_stream_request` 不同，此函数会缓冲所有事件直到流结束，
/// 然后用从 contextUsageEvent 计算的正确 input_tokens 生成 message_start 事件。
async fn handle_stream_request_buffered(
    provider: std::sync::Arc<crate::kiro::provider::KiroProvider>,
    request_body: &str,
    model: &str,
    estimated_input_tokens: i32,
    thinking_enabled: bool,
    tool_name_map: std::collections::HashMap<String, String>,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let response = match provider.call_api_stream(request_body).await {
        Ok(resp) => resp,
        Err(e) => return map_provider_error(e),
    };

    // 创建缓冲流处理上下文
    let ctx = BufferedStreamContext::new(
        model,
        estimated_input_tokens,
        thinking_enabled,
        tool_name_map,
    );

    // 创建缓冲 SSE 流
    let stream = create_buffered_sse_stream(response, ctx);

    // 返回 SSE 响应
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// 创建缓冲 SSE 事件流
///
/// 工作流程：
/// 1. 等待上游流完成，期间只发送 ping 保活信号
/// 2. 使用 StreamContext 的事件处理逻辑处理所有 Kiro 事件，结果缓存
/// 3. 流结束后，用正确的 input_tokens 更正 message_start 事件
/// 4. 一次性发送所有事件
fn create_buffered_sse_stream(
    response: reqwest::Response,
    ctx: BufferedStreamContext,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
    let body_stream = response.bytes_stream();

    stream::unfold(
        (
            body_stream,
            ctx,
            EventStreamDecoder::new(),
            false,
            interval(Duration::from_secs(PING_INTERVAL_SECS)),
        ),
        |(mut body_stream, mut ctx, mut decoder, finished, mut ping_interval)| async move {
            if finished {
                return None;
            }

            loop {
                tokio::select! {
                    // 使用 biased 模式，优先检查 ping 定时器
                    // 避免在上游 chunk 密集时 ping 被"饿死"
                    biased;

                    // 优先检查 ping 保活（等待期间唯一发送的数据）
                    _ = ping_interval.tick() => {
                        tracing::trace!("发送 ping 保活事件（缓冲模式）");
                        let bytes: Vec<Result<Bytes, Infallible>> = vec![Ok(create_ping_sse())];
                        return Some((stream::iter(bytes), (body_stream, ctx, decoder, false, ping_interval)));
                    }

                    // 然后处理数据流
                    chunk_result = body_stream.next() => {
                        match chunk_result {
                            Some(Ok(chunk)) => {
                                // 解码事件
                                if let Err(e) = decoder.feed(&chunk) {
                                    tracing::warn!("缓冲区溢出: {}", e);
                                }

                                for result in decoder.decode_iter() {
                                    match result {
                                        Ok(frame) => {
                                            if let Ok(event) = Event::from_frame(frame) {
                                                // 缓冲事件（复用 StreamContext 的处理逻辑）
                                                ctx.process_and_buffer(&event);
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!("解码事件失败: {}", e);
                                        }
                                    }
                                }
                                // 继续读取下一个 chunk，不发送任何数据
                            }
                            Some(Err(e)) => {
                                tracing::error!("读取响应流失败: {}", e);
                                // 发生错误，完成处理并返回所有事件
                                let all_events = ctx.finish_and_get_all_events();
                                let bytes: Vec<Result<Bytes, Infallible>> = all_events
                                    .into_iter()
                                    .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                    .collect();
                                return Some((stream::iter(bytes), (body_stream, ctx, decoder, true, ping_interval)));
                            }
                            None => {
                                // 流结束，完成处理并返回所有事件（已更正 input_tokens）
                                let all_events = ctx.finish_and_get_all_events();
                                let bytes: Vec<Result<Bytes, Infallible>> = all_events
                                    .into_iter()
                                    .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                    .collect();
                                return Some((stream::iter(bytes), (body_stream, ctx, decoder, true, ping_interval)));
                            }
                        }
                    }
                }
            }
        },
    )
    .flatten()
}
