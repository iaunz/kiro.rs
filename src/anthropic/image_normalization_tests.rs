//! Regression coverage for image limits across the complete converted conversation.

use std::io::Cursor;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, GenericImageView, ImageFormat, Rgb, RgbImage};
use serde_json::{Value, json};

use super::{converter::convert_request, types::MessagesRequest};
use crate::kiro::image_normalization::normalize_images;
use crate::kiro::model::requests::conversation::{ConversationState, KiroImage, Message};
use crate::kiro::model::requests::tool::ToolResult;

fn png_base64(width: u32, height: u32) -> String {
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(width, height, Rgb([37, 82, 143])));
    let mut output = Cursor::new(Vec::new());
    image.write_to(&mut output, ImageFormat::Png).unwrap();
    STANDARD.encode(output.into_inner())
}

fn image_block(data: &str) -> Value {
    json!({
        "type": "image",
        "source": {"type": "base64", "media_type": "image/png", "data": data}
    })
}

fn tool_call(id: &str) -> Value {
    json!({
        "role": "assistant",
        "content": [{"type": "tool_use", "id": id, "name": "screenshot", "input": {}}]
    })
}

fn convert(messages: Vec<Value>) -> ConversationState {
    let request: MessagesRequest = serde_json::from_value(json!({
        "model": "claude-opus-5-thinking",
        "max_tokens": 1024,
        "messages": messages,
        "tools": [{"name": "screenshot", "description": "Take a screenshot", "input_schema": {"type": "object"}}]
    }))
    .unwrap();
    convert_request(&request).unwrap().conversation_state
}

fn all_images(state: &ConversationState) -> Vec<&KiroImage> {
    state
        .history
        .iter()
        .filter_map(|message| match message {
            Message::User(message) => Some(message.user_input_message.images.iter()),
            Message::Assistant(_) => None,
        })
        .flatten()
        .chain(state.current_message.user_input_message.images.iter())
        .collect()
}

fn dimensions(image: &KiroImage) -> (u32, u32) {
    image::load_from_memory(&STANDARD.decode(&image.source.bytes).unwrap())
        .unwrap()
        .dimensions()
}

fn assert_tool_result(result: &ToolResult, id: &str, text: &str, is_error: bool) {
    assert_eq!(result.tool_use_id, id);
    assert_eq!(result.is_error, is_error);
    assert_eq!(
        result.status.as_deref(),
        Some(if is_error { "error" } else { "success" })
    );
    assert_eq!(result.content.len(), 1);
    assert_eq!(result.content[0].get("text"), Some(&json!(text)));
}

fn assert_tool_calls(state: &ConversationState, expected: &[&str]) {
    let ids: Vec<&str> = state
        .history
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(message) => message.assistant_response_message.tool_uses.as_ref(),
            Message::User(_) => None,
        })
        .flatten()
        .map(|tool| tool.tool_use_id.as_str())
        .collect();
    assert_eq!(ids, expected);
}

fn conversation_with_current_screenshot(
    historical_image_count: usize,
    small: &str,
    screenshot: &str,
) -> ConversationState {
    let mut historical_content = vec![json!({"type": "text", "text": "Previous images"})];
    historical_content.extend((0..historical_image_count).map(|_| image_block(small)));
    convert(vec![
        json!({"role": "user", "content": historical_content}),
        tool_call("current-shot"),
        json!({"role": "user", "content": [
            {"type": "text", "text": "Inspect this screenshot"},
            {"type": "tool_result", "tool_use_id": "current-shot", "is_error": true, "content": [
                {"type": "text", "text": "Screenshot captured"},
                image_block(screenshot),
                {"type": "text", "text": "Window lookup failed"}
            ]}
        ]}),
    ])
}

#[test]
fn recovery_resizes_current_tool_screenshot_and_preserves_tool_pairing() {
    let small = png_base64(1, 1);
    let screenshot = png_base64(2400, 12);
    let mut state = conversation_with_current_screenshot(20, &small, &screenshot);
    assert_eq!(
        state.current_message.user_input_message.images[0]
            .source
            .bytes,
        screenshot
    );
    assert!(normalize_images(&mut state, 2000).unwrap());

    let images = all_images(&state);
    assert_eq!(images.len(), 21);
    for image in &images[..20] {
        assert_eq!(image.source.bytes, small);
    }
    assert_ne!(images[20].source.bytes, screenshot);
    assert_eq!(dimensions(images[20]), (2000, 10));
    assert!(images.iter().all(|image| {
        let (width, height) = dimensions(image);
        width <= 2000 && height <= 2000
    }));

    let current = &state.current_message.user_input_message;
    assert_eq!(current.content, "Inspect this screenshot");
    assert_eq!(current.images.len(), 1);
    assert_eq!(current.user_input_message_context.tool_results.len(), 1);
    assert_tool_result(
        &current.user_input_message_context.tool_results[0],
        "current-shot",
        "Screenshot captured\nWindow lookup failed",
        true,
    );
    assert_tool_calls(&state, &["current-shot"]);
}

#[test]
fn recovery_resizes_historical_regular_and_tool_images() {
    let small = png_base64(1, 1);
    let landscape = png_base64(2400, 12);
    let portrait = png_base64(12, 2400);
    let mut historical_result = vec![
        json!({"type": "text", "text": "Historical screenshot"}),
        image_block(&portrait),
    ];
    historical_result.extend((0..18).map(|_| image_block(&small)));
    let mut state = convert(vec![
        json!({"role": "user", "content": [
            {"type": "text", "text": "Original image"}, image_block(&landscape)
        ]}),
        tool_call("historical-shot"),
        json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "historical-shot", "content": historical_result}
        ]}),
        tool_call("current-shot"),
        json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "current-shot", "content": [
                {"type": "text", "text": "Current screenshot"}, image_block(&small)
            ]}
        ]}),
    ]);

    let original_images = all_images(&state);
    assert_eq!(original_images[0].source.bytes, landscape);
    assert_eq!(original_images[1].source.bytes, portrait);
    assert!(normalize_images(&mut state, 2000).unwrap());
    let images = all_images(&state);
    assert_eq!(images.len(), 21);
    assert_ne!(images[0].source.bytes, landscape);
    assert_ne!(images[1].source.bytes, portrait);
    assert_eq!(dimensions(images[0]), (2000, 10));
    assert_eq!(dimensions(images[1]), (10, 2000));
    for image in &images[2..] {
        assert_eq!(image.source.bytes, small);
    }
    assert!(images.iter().all(|image| {
        let (width, height) = dimensions(image);
        width <= 2000 && height <= 2000
    }));

    let historical_results: Vec<&ToolResult> = state
        .history
        .iter()
        .filter_map(|message| match message {
            Message::User(message) => Some(
                message
                    .user_input_message
                    .user_input_message_context
                    .tool_results
                    .iter(),
            ),
            Message::Assistant(_) => None,
        })
        .flatten()
        .collect();
    assert_eq!(historical_results.len(), 1);
    assert_tool_result(
        historical_results[0],
        "historical-shot",
        "Historical screenshot",
        false,
    );
    let current = &state.current_message.user_input_message;
    assert_eq!(current.images.len(), 1);
    assert_eq!(current.user_input_message_context.tool_results.len(), 1);
    assert_tool_result(
        &current.user_input_message_context.tool_results[0],
        "current-shot",
        "Current screenshot",
        false,
    );
    assert_tool_calls(&state, &["historical-shot", "current-shot"]);
}

#[test]
fn normal_conversion_preserves_oversized_screenshot_bytes_regardless_of_image_count() {
    let small = png_base64(1, 1);
    let screenshot = png_base64(2400, 12);
    for historical_count in [0, 19, 20, 24] {
        let state = conversation_with_current_screenshot(historical_count, &small, &screenshot);
        let images = all_images(&state);
        assert_eq!(images.len(), historical_count + 1);
        for image in &images[..historical_count] {
            assert_eq!(image.source.bytes, small);
        }
        assert_eq!(images[historical_count].source.bytes, screenshot);
        assert_eq!(dimensions(images[historical_count]), (2400, 12));
        assert_tool_calls(&state, &["current-shot"]);
    }
}

#[test]
fn recovery_uses_the_upstream_dimension_limit_even_for_one_image() {
    let screenshot = png_base64(2400, 12);
    let mut state = conversation_with_current_screenshot(0, &png_base64(1, 1), &screenshot);
    assert_eq!(all_images(&state)[0].source.bytes, screenshot);

    assert!(normalize_images(&mut state, 1000).unwrap());
    assert_eq!(dimensions(all_images(&state)[0]), (1000, 5));
    let recovered_bytes = all_images(&state)[0].source.bytes.clone();
    assert!(!normalize_images(&mut state, 1000).unwrap());
    assert_eq!(all_images(&state)[0].source.bytes, recovered_bytes);
}
