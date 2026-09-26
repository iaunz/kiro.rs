//! `ListAvailableModels` 返回的模型及 token 限额。

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAvailableModelsResponse {
    #[serde(default)]
    pub models: Vec<UpstreamModel>,
    #[serde(default)]
    pub next_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamModel {
    pub model_id: String,
    #[serde(default)]
    pub model_name: Option<String>,
    #[serde(default)]
    pub token_limits: Option<TokenLimits>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenLimits {
    #[serde(default)]
    pub max_input_tokens: Option<i64>,
    #[serde(default)]
    pub max_output_tokens: Option<i64>,
}
