//! OpenAI Responses API compatibility layer.

use std::{collections::HashMap, convert::Infallible, sync::Arc, time::Duration};

use anyhow::Error;
use axum::{
    Json as JsonExtractor,
    body::Body,
    extract::{State, rejection::JsonRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use bytes::Bytes;
use futures::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::{Instant, interval_at};
use uuid::Uuid;

use crate::{
    kiro::{
        model::{events::Event, requests::kiro::KiroRequest},
        parser::decoder::EventStreamDecoder,
        provider::KiroProvider,
    },
    token,
};

use super::{
    converter::{ConversionError, convert_request, get_context_window_size},
    middleware::AppState,
    stream::extract_thinking_from_complete_text,
    types::{Message, MessagesRequest, OutputConfig, SystemMessage, Thinking, Tool},
};

const DEFAULT_MAX_OUTPUT_TOKENS: i32 = 4096;
const KEEP_ALIVE_SECS: u64 = 25;

#[derive(Debug, Deserialize)]
pub struct ResponsesRequest {
    model: String,
    input: Value,
    instructions: Option<String>,
    max_output_tokens: Option<i32>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    tools: Vec<ResponseTool>,
    tool_choice: Option<Value>,
    reasoning: Option<ReasoningConfig>,
    #[serde(default)]
    previous_response_id: Value,
    #[serde(default)]
    conversation: Value,
    #[serde(default)]
    background: bool,
    #[serde(default)]
    store: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ReasoningConfig {
    effort: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponseTool {
    #[serde(rename = "type")]
    tool_type: String,
    name: Option<String>,
    description: Option<String>,
    parameters: Option<Value>,
    /// Inner function tools for `type: "namespace"` grouping tools.
    #[serde(default)]
    tools: Vec<ResponseTool>,
}

#[derive(Debug, Serialize)]
struct OpenAiErrorResponse {
    error: OpenAiError,
}

#[derive(Debug, Serialize)]
struct OpenAiError {
    message: String,
    #[serde(rename = "type")]
    error_type: String,
    param: Option<String>,
    code: Option<String>,
}

#[derive(Debug)]
struct RequestError {
    message: String,
    param: Option<String>,
    code: Option<String>,
}

impl RequestError {
    fn new(message: impl Into<String>, param: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            param: Some(param.into()),
            code: Some("invalid_value".to_string()),
        }
    }

    fn unsupported(message: impl Into<String>, param: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            param: Some(param.into()),
            code: Some("unsupported_parameter".to_string()),
        }
    }
}

fn error_response(status: StatusCode, error: RequestError) -> Response {
    (
        status,
        Json(OpenAiErrorResponse {
            error: OpenAiError {
                message: error.message,
                error_type: "invalid_request_error".to_string(),
                param: error.param,
                code: error.code,
            },
        }),
    )
        .into_response()
}

fn api_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(OpenAiErrorResponse {
            error: OpenAiError {
                message: message.into(),
                error_type: "api_error".to_string(),
                param: None,
                code: None,
            },
        }),
    )
        .into_response()
}

pub async fn post_response(
    State(state): State<AppState>,
    payload: Result<JsonExtractor<ResponsesRequest>, JsonRejection>,
) -> Response {
    let payload = match payload {
        Ok(JsonExtractor(payload)) => payload,
        Err(error) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                RequestError::new(format!("Invalid JSON request: {error}"), "body"),
            );
        }
    };

    tracing::info!(
        model = %payload.model,
        stream = payload.stream,
        "Received POST /v1/responses request"
    );

    let provider = match &state.kiro_provider {
        Some(provider) => provider.clone(),
        None => {
            return api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Kiro API provider not configured",
            );
        }
    };

    let mapped = match map_request(payload) {
        Ok(mapped) => mapped,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error),
    };

    let conversion = match convert_request(&mapped.messages) {
        Ok(result) => result,
        Err(error) => {
            let message = match error {
                ConversionError::UnsupportedModel(model) => format!("Unsupported model: {model}"),
                ConversionError::EmptyMessages => {
                    "Input must contain at least one user message".to_string()
                }
            };
            return error_response(StatusCode::BAD_REQUEST, RequestError::new(message, "input"));
        }
    };

    let input_tokens = token::count_all_tokens(
        mapped.messages.model.clone(),
        mapped.messages.system.clone(),
        mapped.messages.messages.clone(),
        mapped.messages.tools.clone(),
    ) as i32;
    let request_body = match serde_json::to_string(&KiroRequest {
        conversation_state: conversion.conversation_state,
        profile_arn: None,
    }) {
        Ok(body) => body,
        Err(error) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to serialize request: {error}"),
            );
        }
    };

    let response_id = format!("resp_{}", Uuid::new_v4().simple());
    if mapped.messages.stream {
        handle_stream(
            provider,
            request_body,
            response_id,
            mapped.messages.model,
            input_tokens,
            mapped.max_output_tokens,
            mapped.thinking_enabled,
            conversion.tool_name_map,
        )
        .await
    } else {
        handle_non_stream(
            provider,
            request_body,
            response_id,
            mapped.messages.model,
            input_tokens,
            mapped.max_output_tokens,
            mapped.thinking_enabled,
            conversion.tool_name_map,
        )
        .await
    }
}

#[derive(Debug)]
struct MappedRequest {
    messages: MessagesRequest,
    max_output_tokens: i32,
    thinking_enabled: bool,
}

fn map_request(request: ResponsesRequest) -> Result<MappedRequest, RequestError> {
    validate_stateless_fields(&request)?;
    if request.model.trim().is_empty() {
        return Err(RequestError::new("model must not be empty", "model"));
    }
    let max_output_tokens = request
        .max_output_tokens
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    if max_output_tokens <= 0 {
        return Err(RequestError::new(
            "max_output_tokens must be greater than 0",
            "max_output_tokens",
        ));
    }

    let mut system = request
        .instructions
        .filter(|text| !text.is_empty())
        .map(|text| vec![SystemMessage { text }]);
    let mut extra_tools = Vec::new();
    let mut messages = map_input(&request.input, &mut system, &mut extra_tools)?;
    normalize_messages(&mut messages)?;
    let mut all_tools = request.tools;
    all_tools.append(&mut extra_tools);
    let tools = map_tools(all_tools)?;
    let tool_choice = map_tool_choice(request.tool_choice, &tools)?;
    let (thinking, output_config) = map_reasoning(&request.model, request.reasoning)?;
    let thinking_enabled = thinking.as_ref().is_some_and(Thinking::is_enabled);

    Ok(MappedRequest {
        messages: MessagesRequest {
            model: request.model,
            max_tokens: max_output_tokens,
            messages,
            stream: request.stream,
            system,
            tools: if tools.is_empty() { None } else { Some(tools) },
            tool_choice,
            thinking,
            output_config,
            metadata: None,
        },
        max_output_tokens,
        thinking_enabled,
    })
}

fn validate_stateless_fields(request: &ResponsesRequest) -> Result<(), RequestError> {
    if !request.previous_response_id.is_null() {
        return Err(RequestError::unsupported(
            "previous_response_id is not supported because this service does not persist response state",
            "previous_response_id",
        ));
    }
    if !request.conversation.is_null() {
        return Err(RequestError::unsupported(
            "conversation is not supported because this service does not persist conversation state",
            "conversation",
        ));
    }
    if request.background {
        return Err(RequestError::unsupported(
            "background responses are not supported",
            "background",
        ));
    }
    if request.store == Some(true) {
        return Err(RequestError::unsupported(
            "stored responses are not supported",
            "store",
        ));
    }
    // `include` is an optional response-enrichment hint (e.g.
    // "reasoning.encrypted_content"), not a stateful field. Clients such as
    // Codex send it on every request when response storage is disabled. This
    // service simply does not emit those extra fields, so we accept and ignore
    // the parameter rather than rejecting the whole request.
    Ok(())
}

fn map_input(
    input: &Value,
    system: &mut Option<Vec<SystemMessage>>,
    extra_tools: &mut Vec<ResponseTool>,
) -> Result<Vec<Message>, RequestError> {
    match input {
        Value::String(text) => {
            if text.is_empty() {
                return Err(RequestError::new("input must not be empty", "input"));
            }
            Ok(vec![Message {
                role: "user".to_string(),
                content: Value::String(text.clone()),
            }])
        }
        Value::Array(items) => {
            if items.is_empty() {
                return Err(RequestError::new("input must not be empty", "input"));
            }
            let mut messages = Vec::new();
            for (index, item) in items.iter().enumerate() {
                map_input_item(item, index, system, &mut messages, extra_tools)?;
            }
            if messages.is_empty() {
                return Err(RequestError::new(
                    "input must contain a user or assistant item",
                    "input",
                ));
            }
            Ok(messages)
        }
        _ => Err(RequestError::new(
            "input must be a string or an array",
            "input",
        )),
    }
}

fn map_input_item(
    item: &Value,
    index: usize,
    system: &mut Option<Vec<SystemMessage>>,
    messages: &mut Vec<Message>,
    extra_tools: &mut Vec<ResponseTool>,
) -> Result<(), RequestError> {
    let object = item.as_object().ok_or_else(|| {
        RequestError::new("input items must be objects", format!("input[{index}]"))
    })?;
    let item_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    match item_type {
        "message" => {
            let role = required_string(object, "role", index)?;
            let content = object.get("content").ok_or_else(|| {
                RequestError::new(
                    "message content is required",
                    format!("input[{index}].content"),
                )
            })?;
            map_message(role, content, index, system, messages)
        }
        // `function_call` carries JSON-string arguments; `custom_tool_call`
        // (freeform tools) carries a raw string under `input`. Both map to an
        // assistant tool_use block.
        "function_call" | "custom_tool_call" => map_function_call(object, index, messages),
        "function_call_output" | "custom_tool_call_output" => {
            map_function_output(object, index, messages)
        }
        // `additional_tools` is a developer-role item that carries tool
        // definitions inside the input stream (Codex places tools here instead
        // of the top-level `tools` field in some turns). Collect them so they
        // are exposed to the backend alongside any top-level tools.
        "additional_tools" => {
            if let Some(tools) = object.get("tools").and_then(Value::as_array) {
                for tool in tools {
                    if let Ok(tool) = serde_json::from_value::<ResponseTool>(tool.clone()) {
                        extra_tools.push(tool);
                    }
                }
            }
            Ok(())
        }
        // Other replayed item types (reasoning, web_search_call, image
        // generation, tool_search, etc.) have no Anthropic equivalent here.
        // Codex replays full history each turn when response storage is
        // disabled, so skip unknown items rather than failing the request.
        _ => Ok(()),
    }
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<&'a str, RequestError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            RequestError::new(
                format!("{field} is required and must be a non-empty string"),
                format!("input[{index}].{field}"),
            )
        })
}

fn map_message(
    role: &str,
    content: &Value,
    index: usize,
    system: &mut Option<Vec<SystemMessage>>,
    messages: &mut Vec<Message>,
) -> Result<(), RequestError> {
    if matches!(role, "system" | "developer") {
        let text = content_to_system_text(content, index)?;
        system
            .get_or_insert_with(Vec::new)
            .push(SystemMessage { text });
        return Ok(());
    }
    if !matches!(role, "user" | "assistant") {
        return Err(RequestError::new(
            format!("Unsupported message role: {role}"),
            format!("input[{index}].role"),
        ));
    }
    let blocks = map_message_content(content, role, index)?;
    messages.push(Message {
        role: role.to_string(),
        content: blocks,
    });
    Ok(())
}

fn content_to_system_text(content: &Value, index: usize) -> Result<String, RequestError> {
    match content {
        Value::String(text) if !text.is_empty() => Ok(text.clone()),
        Value::Array(parts) => {
            let mut text = Vec::new();
            for (part_index, part) in parts.iter().enumerate() {
                let object = part.as_object().ok_or_else(|| {
                    RequestError::new(
                        "system content parts must be objects",
                        format!("input[{index}].content[{part_index}]"),
                    )
                })?;
                let part_type = object.get("type").and_then(Value::as_str).unwrap_or("text");
                if !matches!(part_type, "input_text" | "output_text" | "text") {
                    return Err(RequestError::new(
                        "system and developer messages only support text content",
                        format!("input[{index}].content[{part_index}].type"),
                    ));
                }
                let value = object.get("text").and_then(Value::as_str).ok_or_else(|| {
                    RequestError::new(
                        "text is required",
                        format!("input[{index}].content[{part_index}].text"),
                    )
                })?;
                text.push(value.to_string());
            }
            if text.is_empty() {
                Err(RequestError::new(
                    "message content must not be empty",
                    format!("input[{index}].content"),
                ))
            } else {
                Ok(text.join("\n"))
            }
        }
        _ => Err(RequestError::new(
            "message content must be a string or content array",
            format!("input[{index}].content"),
        )),
    }
}

fn map_message_content(content: &Value, role: &str, index: usize) -> Result<Value, RequestError> {
    if let Value::String(text) = content {
        if text.is_empty() {
            return Err(RequestError::new(
                "message content must not be empty",
                format!("input[{index}].content"),
            ));
        }
        return Ok(Value::String(text.clone()));
    }
    let parts = content.as_array().ok_or_else(|| {
        RequestError::new(
            "message content must be a string or content array",
            format!("input[{index}].content"),
        )
    })?;
    if parts.is_empty() {
        return Err(RequestError::new(
            "message content must not be empty",
            format!("input[{index}].content"),
        ));
    }
    let mut blocks = Vec::new();
    for (part_index, part) in parts.iter().enumerate() {
        let object = part.as_object().ok_or_else(|| {
            RequestError::new(
                "content parts must be objects",
                format!("input[{index}].content[{part_index}]"),
            )
        })?;
        let part_type = object.get("type").and_then(Value::as_str).ok_or_else(|| {
            RequestError::new(
                "content part type is required",
                format!("input[{index}].content[{part_index}].type"),
            )
        })?;
        match part_type {
            "input_text" | "output_text" | "text" => {
                let text = object.get("text").and_then(Value::as_str).ok_or_else(|| {
                    RequestError::new(
                        "text is required",
                        format!("input[{index}].content[{part_index}].text"),
                    )
                })?;
                blocks.push(json!({"type": "text", "text": text}));
            }
            "input_image" => {
                if role != "user" {
                    return Err(RequestError::new(
                        "input_image is only valid in user messages",
                        format!("input[{index}].content[{part_index}]"),
                    ));
                }
                let image_url =
                    object
                        .get("image_url")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            RequestError::new(
                                "image_url is required",
                                format!("input[{index}].content[{part_index}].image_url"),
                            )
                        })?;
                let (media_type, data) = parse_data_url(image_url).ok_or_else(|| {
                    RequestError::new(
                        "input_image.image_url must be a supported base64 data URL",
                        format!("input[{index}].content[{part_index}].image_url"),
                    )
                })?;
                blocks.push(json!({
                    "type": "image",
                    "source": {"type": "base64", "media_type": media_type, "data": data}
                }));
            }
            other => {
                return Err(RequestError::new(
                    format!("Unsupported content part type: {other}"),
                    format!("input[{index}].content[{part_index}].type"),
                ));
            }
        }
    }
    Ok(Value::Array(blocks))
}

fn parse_data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (metadata, data) = rest.split_once(',')?;
    let media_type = metadata.strip_suffix(";base64")?;
    if data.is_empty()
        || !matches!(
            media_type,
            "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        )
    {
        return None;
    }
    Some((media_type, data))
}

fn map_function_call(
    object: &serde_json::Map<String, Value>,
    index: usize,
    messages: &mut Vec<Message>,
) -> Result<(), RequestError> {
    let call_id = required_string(object, "call_id", index)?;
    let name = required_string(object, "name", index)?;
    // `function_call` uses `arguments` (a JSON string); `custom_tool_call`
    // (freeform tools) uses `input` (a raw string that may not be JSON).
    let (field, raw) = match object.get("arguments").and_then(Value::as_str) {
        Some(arguments) => ("arguments", arguments),
        None => ("input", required_string(object, "input", index)?),
    };
    // Freeform tool input can be arbitrary text; fall back to a wrapper object
    // when it is not valid JSON so the backend always receives an object.
    let input = match serde_json::from_str::<Value>(raw) {
        Ok(value) if value.is_object() => value,
        _ if field == "input" => json!({ "input": raw }),
        Ok(_) => {
            return Err(RequestError::new(
                "function_call arguments must encode a JSON object",
                format!("input[{index}].arguments"),
            ));
        }
        Err(error) => {
            return Err(RequestError::new(
                format!("function_call arguments must be valid JSON: {error}"),
                format!("input[{index}].arguments"),
            ));
        }
    };
    messages.push(Message {
        role: "assistant".to_string(),
        content: json!([{"type": "tool_use", "id": call_id, "name": name, "input": input}]),
    });
    Ok(())
}

fn map_function_output(
    object: &serde_json::Map<String, Value>,
    index: usize,
    messages: &mut Vec<Message>,
) -> Result<(), RequestError> {
    let call_id = required_string(object, "call_id", index)?;
    let output = object.get("output").ok_or_else(|| {
        RequestError::new(
            "function_call_output output is required",
            format!("input[{index}].output"),
        )
    })?;
    let content = match output {
        Value::String(value) => value.clone(),
        value => serde_json::to_string(value).map_err(|error| {
            RequestError::new(
                format!("Invalid function output: {error}"),
                format!("input[{index}].output"),
            )
        })?,
    };
    messages.push(Message {
        role: "user".to_string(),
        content: json!([{"type": "tool_result", "tool_use_id": call_id, "content": content}]),
    });
    Ok(())
}

fn normalize_messages(messages: &mut [Message]) -> Result<(), RequestError> {
    if !messages.iter().any(|message| message.role == "user") {
        return Err(RequestError::new(
            "input must contain a user message or function_call_output",
            "input",
        ));
    }
    if messages.last().is_none_or(|message| message.role != "user") {
        return Err(RequestError::new(
            "The final input item must be a user message or function_call_output",
            "input",
        ));
    }
    Ok(())
}

fn map_tools(tools: Vec<ResponseTool>) -> Result<Vec<Tool>, RequestError> {
    let mut mapped = Vec::new();
    for (index, tool) in tools.into_iter().enumerate() {
        match tool.tool_type.as_str() {
            "function" => mapped.push(map_function_tool(tool, format!("tools[{index}]"))?),
            // `namespace` is a grouping container: its `tools` array holds
            // ordinary function tools with globally unique leaf names. Flatten
            // them into standalone functions so the backend sees every callable.
            "namespace" => {
                for (inner_index, inner) in tool.tools.into_iter().enumerate() {
                    if inner.tool_type != "function" {
                        continue;
                    }
                    mapped.push(map_function_tool(
                        inner,
                        format!("tools[{index}].tools[{inner_index}]"),
                    )?);
                }
            }
            // Other built-in tool types (e.g. `web_search`) have no function
            // schema and are not backed by this service. Skip them rather than
            // rejecting the whole request so the turn can still proceed.
            _ => continue,
        }
    }
    Ok(mapped)
}

fn map_function_tool(tool: ResponseTool, param: String) -> Result<Tool, RequestError> {
    let name = tool.name.filter(|name| !name.is_empty()).ok_or_else(|| {
        RequestError::new("function tool name is required", format!("{param}.name"))
    })?;
    let parameters = tool
        .parameters
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    let input_schema = parameters.as_object().cloned().ok_or_else(|| {
        RequestError::new(
            "function tool parameters must be a JSON object",
            format!("{param}.parameters"),
        )
    })?;
    Ok(Tool {
        tool_type: None,
        name,
        description: tool.description.unwrap_or_default(),
        input_schema: input_schema.into_iter().collect(),
        max_uses: None,
    })
}

fn map_tool_choice(choice: Option<Value>, tools: &[Tool]) -> Result<Option<Value>, RequestError> {
    let Some(choice) = choice else {
        return Ok(None);
    };
    match choice {
        Value::String(value) if value == "auto" => Ok(Some(json!({"type": "auto"}))),
        Value::String(value) if value == "none" => Ok(Some(json!({"type": "none"}))),
        Value::String(value) if value == "required" => Ok(Some(json!({"type": "any"}))),
        Value::Object(object) if object.get("type").and_then(Value::as_str) == Some("function") => {
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    RequestError::new("tool_choice function name is required", "tool_choice.name")
                })?;
            if !tools.iter().any(|tool| tool.name == name) {
                return Err(RequestError::new(
                    "tool_choice names an undefined function",
                    "tool_choice.name",
                ));
            }
            Ok(Some(json!({"type": "tool", "name": name})))
        }
        _ => Err(RequestError::new("Invalid tool_choice", "tool_choice")),
    }
}

fn map_reasoning(
    model: &str,
    reasoning: Option<ReasoningConfig>,
) -> Result<(Option<Thinking>, Option<OutputConfig>), RequestError> {
    let model_lower = model.to_lowercase();
    let suffix_enabled = model_lower.contains("thinking");
    let effort = reasoning.and_then(|reasoning| reasoning.effort);
    if let Some(value) = effort.as_deref() {
        if !matches!(
            value,
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
        ) {
            return Err(RequestError::new(
                "Invalid reasoning.effort",
                "reasoning.effort",
            ));
        }
    }
    if !suffix_enabled && effort.as_deref().is_none_or(|value| value == "none") {
        return Ok((None, None));
    }

    let adaptive = !suffix_enabled
        || (model_lower.contains("opus")
            && (model_lower.contains("4-6") || model_lower.contains("4.6")));
    let thinking = Thinking {
        thinking_type: if adaptive { "adaptive" } else { "enabled" }.to_string(),
        budget_tokens: 20_000,
    };
    let output_config = adaptive.then(|| OutputConfig {
        effort: effort.unwrap_or_else(|| "high".to_string()),
    });
    Ok((Some(thinking), output_config))
}

#[derive(Debug)]
struct CollectedResponse {
    text: String,
    text_item_id: Option<String>,
    calls: Vec<CollectedCall>,
    input_tokens: i32,
    output_tokens: i32,
    max_output_tokens: i32,
    incomplete_reason: Option<String>,
    created_at: i64,
}

#[derive(Debug, Clone)]
struct CollectedCall {
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
}

async fn handle_non_stream(
    provider: Arc<KiroProvider>,
    request_body: String,
    response_id: String,
    model: String,
    estimated_input_tokens: i32,
    max_output_tokens: i32,
    thinking_enabled: bool,
    tool_name_map: HashMap<String, String>,
) -> Response {
    let response = match provider.call_api(&request_body).await {
        Ok(response) => response,
        Err(error) => return map_provider_error(error),
    };
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            return api_error(
                StatusCode::BAD_GATEWAY,
                format!("Failed to read upstream response: {error}"),
            );
        }
    };
    let collected = match collect_response(
        &bytes,
        &model,
        estimated_input_tokens,
        max_output_tokens,
        thinking_enabled,
        &tool_name_map,
    ) {
        Ok(collected) => collected,
        Err(error) => return api_error(StatusCode::BAD_GATEWAY, error),
    };
    let body = build_response_body(&response_id, &model, &collected);
    (StatusCode::OK, Json(body)).into_response()
}

fn collect_response(
    bytes: &[u8],
    model: &str,
    estimated_input_tokens: i32,
    max_output_tokens: i32,
    thinking_enabled: bool,
    tool_name_map: &HashMap<String, String>,
) -> Result<CollectedResponse, String> {
    let mut decoder = EventStreamDecoder::new();
    decoder
        .feed(bytes)
        .map_err(|error| format!("Failed to decode upstream response: {error}"))?;
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut call_buffers: HashMap<String, (String, String)> = HashMap::new();
    let mut input_tokens = estimated_input_tokens;
    let mut incomplete_reason = None;

    for frame in decoder.decode_iter() {
        let frame = frame.map_err(|error| format!("Failed to decode upstream event: {error}"))?;
        let event =
            Event::from_frame(frame).map_err(|error| format!("Invalid upstream event: {error}"))?;
        match event {
            Event::AssistantResponse(response) => text.push_str(&response.content),
            Event::ToolUse(tool) => {
                let entry = call_buffers
                    .entry(tool.tool_use_id.clone())
                    .or_insert_with(|| (tool.name.clone(), String::new()));
                entry.1.push_str(&tool.input);
                if tool.stop {
                    let (name, arguments) = call_buffers.remove(&tool.tool_use_id).unwrap();
                    let name = tool_name_map.get(&name).cloned().unwrap_or(name);
                    calls.push(CollectedCall {
                        item_id: format!("fc_{}", Uuid::new_v4().simple()),
                        call_id: tool.tool_use_id,
                        name,
                        arguments,
                    });
                }
            }
            Event::ContextUsage(usage) => {
                input_tokens = (usage.context_usage_percentage
                    * get_context_window_size(model) as f64
                    / 100.0) as i32;
                if usage.context_usage_percentage >= 100.0 {
                    incomplete_reason = Some("context_window_exceeded".to_string());
                }
            }
            Event::Exception {
                exception_type,
                message,
            } => {
                if exception_type == "ContentLengthExceededException" {
                    incomplete_reason = Some("max_output_tokens".to_string());
                } else {
                    return Err(format!("Upstream exception {exception_type}: {message}"));
                }
            }
            Event::Error {
                error_code,
                error_message,
            } => {
                return Err(format!("Upstream error {error_code}: {error_message}"));
            }
            _ => {}
        }
    }

    if !call_buffers.is_empty() {
        let mut ids: Vec<_> = call_buffers.keys().cloned().collect();
        ids.sort();
        return Err(format!(
            "Upstream stream ended before tool call(s) completed: {}",
            ids.join(", ")
        ));
    }

    if thinking_enabled {
        text = remove_complete_thinking_blocks(text);
    }
    let mut token_blocks = Vec::new();
    if !text.is_empty() {
        token_blocks.push(json!({"type": "text", "text": text}));
    }
    for call in &calls {
        let input = serde_json::from_str::<Value>(&call.arguments)
            .unwrap_or_else(|_| Value::String(call.arguments.clone()));
        token_blocks.push(json!({"type": "tool_use", "input": input}));
    }
    let output_tokens = token::estimate_output_tokens(&token_blocks);
    if incomplete_reason.is_none() && output_tokens >= max_output_tokens {
        incomplete_reason = Some("max_output_tokens".to_string());
    }

    Ok(CollectedResponse {
        text_item_id: (!text.is_empty()).then(|| format!("msg_{}", Uuid::new_v4().simple())),
        text,
        calls,
        input_tokens,
        output_tokens,
        max_output_tokens,
        incomplete_reason,
        created_at: chrono::Utc::now().timestamp(),
    })
}

fn remove_complete_thinking_blocks(mut text: String) -> String {
    loop {
        let (_, remaining) = extract_thinking_from_complete_text(&text);
        if remaining == text {
            return text;
        }
        text = remaining;
    }
}

fn build_response_body(response_id: &str, model: &str, collected: &CollectedResponse) -> Value {
    let mut output = Vec::new();
    if let Some(item_id) = &collected.text_item_id {
        output.push(json!({
            "id": item_id,
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": collected.text,
                "annotations": [],
                "logprobs": []
            }]
        }));
    }
    for call in &collected.calls {
        output.push(json!({
            "id": call.item_id,
            "type": "function_call",
            "status": "completed",
            "call_id": call.call_id,
            "name": call.name,
            "arguments": call.arguments
        }));
    }

    response_snapshot(response_id, model, collected, output, None)
}

fn response_snapshot(
    response_id: &str,
    model: &str,
    collected: &CollectedResponse,
    output: Vec<Value>,
    error: Option<Value>,
) -> Value {
    let status = if error.is_some() {
        "failed"
    } else if collected.incomplete_reason.is_some() {
        "incomplete"
    } else {
        "completed"
    };
    json!({
        "id": response_id,
        "object": "response",
        "created_at": collected.created_at,
        "status": status,
        "background": false,
        "error": error,
        "incomplete_details": collected.incomplete_reason.as_ref().map(|reason| json!({"reason": reason})),
        "instructions": null,
        "max_output_tokens": collected.max_output_tokens,
        "model": model,
        "output": output,
        "parallel_tool_calls": true,
        "previous_response_id": null,
        "store": false,
        "usage": {
            "input_tokens": collected.input_tokens,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens": collected.output_tokens,
            "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": collected.input_tokens + collected.output_tokens
        }
    })
}

fn map_provider_error(error: Error) -> Response {
    let message = error.to_string();
    if message.contains("CONTENT_LENGTH_EXCEEDS_THRESHOLD") {
        return error_response(
            StatusCode::BAD_REQUEST,
            RequestError::new("Context window is full. Reduce the input size.", "input"),
        );
    }
    if message.contains("Input is too long") {
        return error_response(
            StatusCode::BAD_REQUEST,
            RequestError::new("Input is too long. Reduce the input size.", "input"),
        );
    }
    tracing::error!(error = %error, "OpenAI Responses upstream request failed");
    api_error(
        StatusCode::BAD_GATEWAY,
        format!("Upstream API request failed: {error}"),
    )
}

struct ResponseStreamContext {
    response_id: String,
    model: String,
    max_output_tokens: i32,
    thinking_enabled: bool,
    tool_name_map: HashMap<String, String>,
    text: String,
    calls: Vec<CollectedCall>,
    call_buffers: HashMap<String, (String, String)>,
    input_tokens: i32,
    incomplete_reason: Option<String>,
    created_at: i64,
    sequence_number: u64,
}

impl ResponseStreamContext {
    fn new(
        response_id: String,
        model: String,
        max_output_tokens: i32,
        thinking_enabled: bool,
        tool_name_map: HashMap<String, String>,
        input_tokens: i32,
    ) -> Self {
        Self {
            response_id,
            model,
            max_output_tokens,
            thinking_enabled,
            tool_name_map,
            text: String::new(),
            calls: Vec::new(),
            call_buffers: HashMap::new(),
            input_tokens,
            incomplete_reason: None,
            created_at: chrono::Utc::now().timestamp(),
            sequence_number: 0,
        }
    }

    fn event(&mut self, event_type: &str, mut data: Value) -> Bytes {
        self.sequence_number += 1;
        data["type"] = json!(event_type);
        data["sequence_number"] = json!(self.sequence_number);
        Bytes::from(format!(
            "event: {event_type}\ndata: {}\n\n",
            serde_json::to_string(&data).unwrap_or_else(|_| "{}".to_string())
        ))
    }

    fn initial_events(&mut self) -> Vec<Bytes> {
        let response = self.snapshot(Vec::new(), None);
        vec![
            self.event("response.created", json!({"response": response})),
            self.event(
                "response.in_progress",
                json!({"response": self.snapshot(Vec::new(), None)}),
            ),
        ]
    }

    fn process_event(&mut self, event: Event) -> Result<(), String> {
        match event {
            Event::AssistantResponse(response) => self.text.push_str(&response.content),
            Event::ToolUse(tool) => self.process_tool(tool)?,
            Event::ContextUsage(usage) => {
                self.input_tokens = (usage.context_usage_percentage
                    * get_context_window_size(&self.model) as f64
                    / 100.0) as i32;
                if usage.context_usage_percentage >= 100.0 {
                    self.incomplete_reason = Some("context_window_exceeded".to_string());
                }
            }
            Event::Exception {
                exception_type,
                message,
            } => {
                if exception_type == "ContentLengthExceededException" {
                    self.incomplete_reason = Some("max_output_tokens".to_string());
                } else {
                    return Err(format!("Upstream exception {exception_type}: {message}"));
                }
            }
            Event::Error {
                error_code,
                error_message,
            } => return Err(format!("Upstream error {error_code}: {error_message}")),
            _ => {}
        }
        Ok(())
    }

    fn process_tool(
        &mut self,
        tool: crate::kiro::model::events::ToolUseEvent,
    ) -> Result<(), String> {
        let entry = self
            .call_buffers
            .entry(tool.tool_use_id.clone())
            .or_insert_with(|| (tool.name.clone(), String::new()));
        entry.1.push_str(&tool.input);
        if tool.stop {
            let (name, arguments) = self
                .call_buffers
                .remove(&tool.tool_use_id)
                .ok_or_else(|| "Tool call state was lost".to_string())?;
            let name = self.tool_name_map.get(&name).cloned().unwrap_or(name);
            self.calls.push(CollectedCall {
                item_id: format!("fc_{}", Uuid::new_v4().simple()),
                call_id: tool.tool_use_id,
                name,
                arguments,
            });
        }
        Ok(())
    }

    fn snapshot(&self, output: Vec<Value>, error: Option<Value>) -> Value {
        json!({
            "id": self.response_id,
            "object": "response",
            "created_at": self.created_at,
            "status": if error.is_some() { "failed" } else { "in_progress" },
            "background": false,
            "error": error,
            "incomplete_details": null,
            "max_output_tokens": self.max_output_tokens,
            "model": self.model,
            "output": output,
            "parallel_tool_calls": true,
            "previous_response_id": null,
            "store": false
        })
    }

    fn collected(&self, text: String, text_item_id: Option<String>) -> CollectedResponse {
        let mut blocks = Vec::new();
        if !text.is_empty() {
            blocks.push(json!({"type": "text", "text": text}));
        }
        for call in &self.calls {
            let input = serde_json::from_str::<Value>(&call.arguments)
                .unwrap_or_else(|_| Value::String(call.arguments.clone()));
            blocks.push(json!({"type": "tool_use", "input": input}));
        }
        let output_tokens = token::estimate_output_tokens(&blocks);
        let incomplete_reason = self.incomplete_reason.clone().or_else(|| {
            (output_tokens >= self.max_output_tokens).then(|| "max_output_tokens".to_string())
        });
        CollectedResponse {
            text,
            text_item_id,
            calls: self.calls.clone(),
            input_tokens: self.input_tokens,
            output_tokens,
            max_output_tokens: self.max_output_tokens,
            incomplete_reason,
            created_at: self.created_at,
        }
    }

    /// Emit the terminal event sequence once the upstream stream is done.
    fn finish_events(&mut self) -> Vec<Bytes> {
        if !self.call_buffers.is_empty() {
            let mut ids: Vec<_> = self.call_buffers.keys().cloned().collect();
            ids.sort();
            return self.failed_events(format!(
                "Upstream stream ended before tool call(s) completed: {}",
                ids.join(", ")
            ));
        }

        let text = if self.thinking_enabled {
            remove_complete_thinking_blocks(std::mem::take(&mut self.text))
        } else {
            std::mem::take(&mut self.text)
        };
        let text_item_id = (!text.is_empty()).then(|| format!("msg_{}", Uuid::new_v4().simple()));

        let mut events = Vec::new();
        let mut output_index: i64 = 0;

        if let Some(item_id) = &text_item_id {
            let item_id = item_id.clone();
            let base_item = json!({
                "id": item_id,
                "type": "message",
                "status": "in_progress",
                "role": "assistant",
                "content": []
            });
            events.push(self.event(
                "response.output_item.added",
                json!({"output_index": output_index, "item": base_item}),
            ));
            events.push(self.event(
                "response.content_part.added",
                json!({
                    "item_id": item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": "", "annotations": [], "logprobs": []}
                }),
            ));
            if !text.is_empty() {
                events.push(self.event(
                    "response.output_text.delta",
                    json!({
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "delta": text
                    }),
                ));
            }
            events.push(self.event(
                "response.output_text.done",
                json!({
                    "item_id": item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "text": text
                }),
            ));
            events.push(self.event(
                "response.content_part.done",
                json!({
                    "item_id": item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": text, "annotations": [], "logprobs": []}
                }),
            ));
            events.push(self.event(
                "response.output_item.done",
                json!({
                    "output_index": output_index,
                    "item": {
                        "id": item_id,
                        "type": "message",
                        "status": "completed",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text, "annotations": [], "logprobs": []}]
                    }
                }),
            ));
            output_index += 1;
        }

        let calls = self.calls.clone();
        for call in &calls {
            events.extend(self.call_events(call, output_index));
            output_index += 1;
        }

        let collected = self.collected(text, text_item_id);
        let response = build_response_body(&self.response_id, &self.model.clone(), &collected);
        let terminal = if collected.incomplete_reason.is_some() {
            "response.incomplete"
        } else {
            "response.completed"
        };
        events.push(self.event(terminal, json!({"response": response})));
        events
    }

    fn call_events(&mut self, call: &CollectedCall, output_index: i64) -> Vec<Bytes> {
        let base_item = json!({
            "id": call.item_id,
            "type": "function_call",
            "status": "in_progress",
            "call_id": call.call_id,
            "name": call.name,
            "arguments": ""
        });
        vec![
            self.event(
                "response.output_item.added",
                json!({"output_index": output_index, "item": base_item}),
            ),
            self.event(
                "response.function_call_arguments.delta",
                json!({
                    "item_id": call.item_id,
                    "output_index": output_index,
                    "delta": call.arguments
                }),
            ),
            self.event(
                "response.function_call_arguments.done",
                json!({
                    "item_id": call.item_id,
                    "output_index": output_index,
                    "arguments": call.arguments
                }),
            ),
            self.event(
                "response.output_item.done",
                json!({
                    "output_index": output_index,
                    "item": {
                        "id": call.item_id,
                        "type": "function_call",
                        "status": "completed",
                        "call_id": call.call_id,
                        "name": call.name,
                        "arguments": call.arguments
                    }
                }),
            ),
        ]
    }

    fn failed_events(&mut self, message: impl Into<String>) -> Vec<Bytes> {
        let message = message.into();
        tracing::error!(response_id = %self.response_id, error = %message, "OpenAI Responses stream failed");
        let error = json!({"code": "upstream_error", "message": message});
        let response = self.snapshot(Vec::new(), Some(error));
        vec![self.event("response.failed", json!({"response": response}))]
    }
}

fn keep_alive_comment() -> Bytes {
    Bytes::from_static(b": keep-alive\n\n")
}

async fn handle_stream(
    provider: Arc<KiroProvider>,
    request_body: String,
    response_id: String,
    model: String,
    input_tokens: i32,
    max_output_tokens: i32,
    thinking_enabled: bool,
    tool_name_map: HashMap<String, String>,
) -> Response {
    let upstream = match provider.call_api_stream(&request_body).await {
        Ok(response) => response,
        Err(error) => return map_provider_error(error),
    };

    let mut ctx = ResponseStreamContext::new(
        response_id,
        model,
        max_output_tokens,
        thinking_enabled,
        tool_name_map,
        input_tokens,
    );
    let initial = ctx.initial_events();
    let stream = build_stream(upstream, ctx, initial);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(stream))
        .unwrap()
}

fn build_stream(
    upstream: reqwest::Response,
    ctx: ResponseStreamContext,
    initial: Vec<Bytes>,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
    let initial_stream = stream::iter(initial.into_iter().map(Ok));
    let body_stream = upstream.bytes_stream();
    let keep_alive = interval_at(
        Instant::now() + Duration::from_secs(KEEP_ALIVE_SECS),
        Duration::from_secs(KEEP_ALIVE_SECS),
    );

    let processing = stream::unfold(
        (body_stream, ctx, EventStreamDecoder::new(), false, keep_alive),
        |(mut body_stream, mut ctx, mut decoder, finished, mut keep_alive)| async move {
            if finished {
                return None;
            }
            loop {
                tokio::select! {
                    biased;
                    _ = keep_alive.tick() => {
                        let bytes: Vec<Result<Bytes, Infallible>> = vec![Ok(keep_alive_comment())];
                        return Some((stream::iter(bytes), (body_stream, ctx, decoder, false, keep_alive)));
                    }
                    chunk = body_stream.next() => {
                        match chunk {
                            Some(Ok(chunk)) => {
                                if let Err(error) = decoder.feed(&chunk) {
                                    tracing::warn!("缓冲区溢出: {}", error);
                                }
                                for frame in decoder.decode_iter() {
                                    let processed = frame
                                        .map_err(|error| format!("Failed to decode upstream event: {error}"))
                                        .and_then(|frame| {
                                            Event::from_frame(frame)
                                                .map_err(|error| format!("Invalid upstream event: {error}"))
                                                .and_then(|event| ctx.process_event(event))
                                        });
                                    if let Err(error) = processed {
                                        let bytes: Vec<Result<Bytes, Infallible>> =
                                            ctx.failed_events(error).into_iter().map(Ok).collect();
                                        return Some((stream::iter(bytes), (body_stream, ctx, decoder, true, keep_alive)));
                                    }
                                }
                                // keep reading without emitting until the stream completes
                            }
                            Some(Err(error)) => {
                                let bytes: Vec<Result<Bytes, Infallible>> = ctx
                                    .failed_events(format!("Failed to read upstream response: {error}"))
                                    .into_iter()
                                    .map(Ok)
                                    .collect();
                                return Some((stream::iter(bytes), (body_stream, ctx, decoder, true, keep_alive)));
                            }
                            None => {
                                let bytes: Vec<Result<Bytes, Infallible>> =
                                    ctx.finish_events().into_iter().map(Ok).collect();
                                return Some((stream::iter(bytes), (body_stream, ctx, decoder, true, keep_alive)));
                            }
                        }
                    }
                }
            }
        },
    )
    .flatten();

    initial_stream.chain(processing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(input: Value) -> ResponsesRequest {
        serde_json::from_value(json!({
            "model": "claude-opus-4-8",
            "input": input
        }))
        .unwrap()
    }

    #[test]
    fn maps_string_input_to_user_message() {
        let mapped = map_request(request(json!("hello"))).unwrap();
        assert_eq!(mapped.messages.messages.len(), 1);
        assert_eq!(mapped.messages.messages[0].role, "user");
        assert_eq!(mapped.messages.messages[0].content, json!("hello"));
        assert_eq!(mapped.max_output_tokens, DEFAULT_MAX_OUTPUT_TOKENS);
    }

    #[test]
    fn maps_message_items_and_instructions() {
        let mut req = request(json!([
            {"type": "message", "role": "system", "content": "be terse"},
            {"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "hi"}
            ]}
        ]));
        req.instructions = Some("top instructions".to_string());
        let mapped = map_request(req).unwrap();
        let system = mapped.messages.system.unwrap();
        assert_eq!(system.len(), 2);
        assert_eq!(system[0].text, "top instructions");
        assert_eq!(system[1].text, "be terse");
        assert_eq!(
            mapped.messages.messages[0].content,
            json!([{"type": "text", "text": "hi"}])
        );
    }

    #[test]
    fn maps_input_image_data_url() {
        let mapped = map_request(request(json!([
            {"type": "message", "role": "user", "content": [
                {"type": "input_image", "image_url": "data:image/png;base64,AAAA"}
            ]}
        ])))
        .unwrap();
        assert_eq!(
            mapped.messages.messages[0].content,
            json!([{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}])
        );
    }

    #[test]
    fn rejects_unsupported_image_url() {
        let error = map_request(request(json!([
            {"type": "message", "role": "user", "content": [
                {"type": "input_image", "image_url": "https://example.com/a.png"}
            ]}
        ])))
        .unwrap_err();
        assert_eq!(
            error.param.as_deref(),
            Some("input[0].content[0].image_url")
        );
    }

    #[test]
    fn maps_function_tool_and_call_roundtrip() {
        let mut req = request(json!([
            {"type": "function_call", "call_id": "call_1", "name": "lookup", "arguments": "{\"q\":\"x\"}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "done"}
        ]));
        req.tools = serde_json::from_value(json!([
            {"type": "function", "name": "lookup", "parameters": {"type": "object", "properties": {}}}
        ]))
        .unwrap();
        let mapped = map_request(req).unwrap();
        let tools = mapped.messages.tools.unwrap();
        assert_eq!(tools[0].name, "lookup");
        assert_eq!(mapped.messages.messages[0].role, "assistant");
        assert_eq!(
            mapped.messages.messages[0].content,
            json!([{"type": "tool_use", "id": "call_1", "name": "lookup", "input": {"q": "x"}}])
        );
        assert_eq!(mapped.messages.messages[1].role, "user");
        assert_eq!(
            mapped.messages.messages[1].content,
            json!([{"type": "tool_result", "tool_use_id": "call_1", "content": "done"}])
        );
    }

    #[test]
    fn flattens_namespace_tools_and_skips_builtins() {
        // Codex sends `namespace` grouping tools (with a nested `tools` array)
        // and built-in types like `web_search` alongside plain functions.
        let mut req = request(json!("hi"));
        req.tools = serde_json::from_value(json!([
            {"type": "function", "name": "shell_command", "parameters": {"type": "object", "properties": {}}},
            {"type": "namespace", "name": "mcp__node_repl", "description": "node repl", "tools": [
                {"type": "function", "name": "js", "parameters": {"type": "object", "properties": {}}},
                {"type": "function", "name": "js_reset", "parameters": {"type": "object", "properties": {}}}
            ]},
            {"type": "web_search", "external_web_access": true}
        ]))
        .unwrap();
        let mapped = map_request(req).unwrap();
        let names: Vec<_> = mapped
            .messages
            .tools
            .unwrap()
            .iter()
            .map(|t| t.name.clone())
            .collect();
        assert_eq!(names, vec!["shell_command", "js", "js_reset"]);
    }

    #[test]
    fn collects_tools_from_additional_tools_item() {
        // Codex may deliver tool definitions via an `additional_tools` input
        // item instead of the top-level `tools` field.
        let req = request(json!([
            {"type": "additional_tools", "role": "developer", "tools": [
                {"type": "custom", "name": "exec", "description": "run js"},
                {"type": "function", "name": "wait", "parameters": {"type": "object", "properties": {}}},
                {"type": "namespace", "name": "collab", "description": "c", "tools": [
                    {"type": "function", "name": "spawn_agent", "parameters": {"type": "object", "properties": {}}}
                ]}
            ]},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
        ]));
        let mapped = map_request(req).unwrap();
        let names: Vec<_> = mapped
            .messages
            .tools
            .unwrap()
            .iter()
            .map(|t| t.name.clone())
            .collect();
        // `custom` (freeform) has no function schema and is skipped; the
        // function and flattened namespace tool are exposed.
        assert_eq!(names, vec!["wait", "spawn_agent"]);
    }

    #[test]
    fn skips_unknown_replayed_input_items() {
        // With response storage disabled Codex replays full history, including
        // item types with no Anthropic equivalent. These must be ignored.
        let req = request(json!([
            {"type": "reasoning", "summary": [], "encrypted_content": "abc"},
            {"type": "web_search_call", "status": "completed"},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
        ]));
        let mapped = map_request(req).unwrap();
        assert_eq!(mapped.messages.messages.len(), 1);
        assert_eq!(mapped.messages.messages[0].role, "user");
    }

    #[test]
    fn maps_custom_tool_call_roundtrip() {
        // Freeform (`custom`) tool calls use `input` (raw string) instead of
        // `arguments`, and the paired output uses `custom_tool_call_output`.
        let req = request(json!([
            {"type": "custom_tool_call", "call_id": "c1", "name": "exec", "input": "console.log(1)"},
            {"type": "custom_tool_call_output", "call_id": "c1", "output": "1"}
        ]));
        let mapped = map_request(req).unwrap();
        assert_eq!(
            mapped.messages.messages[0].content,
            json!([{"type": "tool_use", "id": "c1", "name": "exec", "input": {"input": "console.log(1)"}}])
        );
        assert_eq!(
            mapped.messages.messages[1].content,
            json!([{"type": "tool_result", "tool_use_id": "c1", "content": "1"}])
        );
    }

    #[test]
    fn rejects_stateful_fields() {
        let mut req = request(json!("hi"));
        req.previous_response_id = json!("resp_1");
        let error = map_request(req).unwrap_err();
        assert_eq!(error.param.as_deref(), Some("previous_response_id"));
        assert_eq!(error.code.as_deref(), Some("unsupported_parameter"));
    }

    #[test]
    fn accepts_include_field() {
        // Codex sends include=["reasoning.encrypted_content"] whenever response
        // storage is disabled. The endpoint must accept and ignore it.
        let req: ResponsesRequest = serde_json::from_value(json!({
            "model": "claude-opus-4-8",
            "input": "hi",
            "store": false,
            "include": ["reasoning.encrypted_content"]
        }))
        .unwrap();
        assert!(map_request(req).is_ok());
    }

    #[test]
    fn rejects_when_no_user_message() {
        let error = map_request(request(json!([
            {"type": "message", "role": "assistant", "content": "hi"}
        ])))
        .unwrap_err();
        assert_eq!(error.param.as_deref(), Some("input"));
    }

    #[test]
    fn thinking_suffix_enables_reasoning() {
        let mapped = map_request(request_with_model("claude-opus-4-8-thinking")).unwrap();
        assert!(mapped.thinking_enabled);
        assert_eq!(mapped.messages.thinking.unwrap().thinking_type, "enabled");
    }

    fn request_with_model(model: &str) -> ResponsesRequest {
        serde_json::from_value(json!({"model": model, "input": "hi"})).unwrap()
    }

    fn sample_collected() -> CollectedResponse {
        CollectedResponse {
            text: "answer".to_string(),
            text_item_id: Some("msg_1".to_string()),
            calls: vec![CollectedCall {
                item_id: "fc_1".to_string(),
                call_id: "call_1".to_string(),
                name: "lookup".to_string(),
                arguments: "{\"q\":\"x\"}".to_string(),
            }],
            input_tokens: 12,
            output_tokens: 7,
            max_output_tokens: 4096,
            incomplete_reason: None,
            created_at: 1_700_000_000,
        }
    }

    #[test]
    fn builds_non_stream_response_body() {
        let body = build_response_body("resp_1", "claude-opus-4-8", &sample_collected());
        assert_eq!(body["object"], "response");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["output"][0]["type"], "message");
        assert_eq!(body["output"][0]["content"][0]["text"], "answer");
        assert_eq!(body["output"][1]["type"], "function_call");
        assert_eq!(body["output"][1]["call_id"], "call_1");
        assert_eq!(body["usage"]["total_tokens"], 19);
    }

    fn context() -> ResponseStreamContext {
        ResponseStreamContext::new(
            "resp_1".to_string(),
            "claude-opus-4-8".to_string(),
            4096,
            false,
            HashMap::new(),
            10,
        )
    }

    fn event_types(chunks: &[Bytes]) -> Vec<String> {
        chunks
            .iter()
            .map(|bytes| {
                let text = String::from_utf8_lossy(bytes);
                text.lines()
                    .find_map(|line| line.strip_prefix("event: "))
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn stream_emits_ordered_events_with_sequence() {
        let mut ctx = context();
        let mut chunks = ctx.initial_events();
        ctx.text.push_str("hi there");
        chunks.extend(ctx.finish_events());

        assert_eq!(
            event_types(&chunks),
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ]
        );

        let mut previous = 0;
        for bytes in &chunks {
            let data = String::from_utf8_lossy(bytes);
            let json_line = data
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .unwrap();
            let value: Value = serde_json::from_str(json_line).unwrap();
            let seq = value["sequence_number"].as_u64().unwrap();
            assert_eq!(seq, previous + 1);
            previous = seq;
        }
    }

    #[test]
    fn stream_emits_function_call_events() {
        let mut ctx = context();
        let _ = ctx.initial_events();
        ctx.calls.push(CollectedCall {
            item_id: "fc_1".to_string(),
            call_id: "call_1".to_string(),
            name: "lookup".to_string(),
            arguments: "{}".to_string(),
        });
        let chunks = ctx.finish_events();
        let types = event_types(&chunks);
        assert!(types.contains(&"response.function_call_arguments.delta".to_string()));
        assert!(types.contains(&"response.function_call_arguments.done".to_string()));
        assert_eq!(types.last().unwrap(), "response.completed");
    }

    #[test]
    fn stream_failure_emits_response_failed() {
        let mut ctx = context();
        let chunks = ctx.failed_events("boom");
        assert_eq!(event_types(&chunks), vec!["response.failed"]);
    }

    #[test]
    fn incomplete_reason_switches_terminal_event() {
        let mut ctx = context();
        let _ = ctx.initial_events();
        ctx.incomplete_reason = Some("context_window_exceeded".to_string());
        ctx.text.push_str("partial");
        let chunks = ctx.finish_events();
        assert_eq!(event_types(&chunks).last().unwrap(), "response.incomplete");
    }
}
