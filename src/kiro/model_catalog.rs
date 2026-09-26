//! 自动模型发现：按需刷新、合并分页，查询失败时保留上次成功的目录。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use reqwest::{Client, RequestBuilder, StatusCode};
use tokio::sync::Mutex;

use crate::kiro::kiro_version::{USAGE_API_AWS_SDK_VERSION, USAGE_API_KIRO_VERSION};
use crate::kiro::machine_id;
use crate::kiro::model::available_models::{ListAvailableModelsResponse, UpstreamModel};
use crate::kiro::model::credentials::KiroCredentials;
use crate::model::config::Config;

const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const FAILURE_RETRY_DELAY: Duration = Duration::from_secs(30);
const MAX_MODEL_PAGES: usize = 100;

// 转换器是同步代码，只读取最近一次成功发现的目录，不触发网络请求。
static DISCOVERED_MODELS: LazyLock<RwLock<HashMap<String, UpstreamModel>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Claude 家族统一使用 claude- 前缀；版本号、日期及其他模型 ID 保持原样。
pub fn canonical_model_id(model: &str) -> String {
    let normalized = model.trim().to_ascii_lowercase();
    match claude_family_id(&normalized) {
        Some(id) => format!("claude-{id}"),
        None => model.to_string(),
    }
}

// 只识别完整家族名称或以连字符分隔的后缀，避免将 opus-custom 的规则扩展到 opusfoo。
fn claude_family_id(model: &str) -> Option<&str> {
    let bare = model.strip_prefix("claude-").unwrap_or(model);
    let family = bare.split('-').next()?;
    matches!(family, "fable" | "sonnet" | "opus" | "haiku").then_some(bare)
}

fn find_discovered_model<'a>(
    models: &'a HashMap<String, UpstreamModel>,
    model: &str,
) -> Option<&'a UpstreamModel> {
    let normalized = model.trim().to_ascii_lowercase();
    if let Some(model) = models.get(&normalized) {
        return Some(model);
    }
    let bare = claude_family_id(&normalized)?;
    if normalized.starts_with("claude-") {
        models.get(bare)
    } else {
        models.get(&format!("claude-{bare}"))
    }
}

/// 接受上游实际公布的模型 ID 及 Claude 前缀别名，保留上游的拼写。
pub fn discovered_model_id(model: &str) -> Option<String> {
    find_discovered_model(&DISCOVERED_MODELS.read(), model).map(|model| model.model_id.clone())
}

/// 未公布、无效或超出本地计数范围的限额由调用者使用原有默认值。
pub fn discovered_context_window(model: &str) -> Option<i32> {
    let models = DISCOVERED_MODELS.read();
    let limit = find_discovered_model(&models, model)?
        .token_limits
        .as_ref()?
        .max_input_tokens?;
    i32::try_from(limit).ok().filter(|limit| *limit > 0)
}

#[derive(Default)]
struct CachedModels {
    models: Vec<UpstreamModel>,
    refresh_after: Option<Instant>,
    last_error: Option<String>,
}

impl CachedModels {
    fn result(&self) -> anyhow::Result<Vec<UpstreamModel>> {
        if self.models.is_empty() {
            anyhow::bail!(
                "{}",
                self.last_error.as_deref().unwrap_or("尚未获取上游模型目录")
            );
        }
        Ok(self.models.clone())
    }
}

#[derive(Default)]
pub(crate) struct ModelCatalogCache {
    // 刷新期间持有异步锁，合并并发查询，避免每个 /models 请求都发起上游请求。
    state: Mutex<CachedModels>,
}

impl ModelCatalogCache {
    pub(crate) async fn get_or_refresh<F, Fut>(
        &self,
        fetch: F,
    ) -> anyhow::Result<Vec<UpstreamModel>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<Vec<UpstreamModel>>>,
    {
        let mut state = self.state.lock().await;
        if state
            .refresh_after
            .is_some_and(|after| Instant::now() < after)
        {
            return state.result();
        }

        let result = fetch().await.and_then(|mut models| {
            let mut seen = HashSet::new();
            models.retain(|model| {
                !model.model_id.trim().is_empty()
                    && seen.insert(model.model_id.to_ascii_lowercase())
            });
            anyhow::ensure!(!models.is_empty(), "上游返回了空模型目录");
            Ok(models)
        });

        match result {
            Ok(models) => {
                *DISCOVERED_MODELS.write() = models
                    .iter()
                    .cloned()
                    .map(|model| (model.model_id.to_ascii_lowercase(), model))
                    .collect();
                state.models = models;
                state.last_error = None;
                state.refresh_after = Some(Instant::now() + CACHE_TTL);
            }
            Err(error) => {
                tracing::warn!("自动获取 Kiro 模型失败，保留现有模型目录: {}", error);
                state.last_error = Some(error.to_string());
                state.refresh_after = Some(Instant::now() + FAILURE_RETRY_DELAY);
            }
        }
        state.result()
    }
}

// 首先遵守本地 apiRegion 配置；只有 403 才尝试来源实现的跨区兼容回退。
fn region_candidates(credentials: &KiroCredentials, config: &Config) -> Vec<String> {
    let mut regions = vec![credentials.effective_api_region(config).to_string()];
    let fallback = if credentials.effective_auth_region(config).starts_with("eu-") {
        ["eu-central-1", "us-east-1"]
    } else {
        ["us-east-1", "eu-central-1"]
    };
    for region in fallback {
        if !regions.iter().any(|existing| existing == region) {
            regions.push(region.to_string());
        }
    }
    regions
}

fn model_request(
    client: &Client,
    base_url: &str,
    credentials: &KiroCredentials,
    config: &Config,
    token: &str,
    next_token: Option<&str>,
) -> RequestBuilder {
    let machine_id = machine_id::generate_from_credentials(credentials, config);
    let sdk = USAGE_API_AWS_SDK_VERSION;
    let version = USAGE_API_KIRO_VERSION;
    let user_agent = format!(
        "aws-sdk-js/{sdk} ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererruntime#{sdk} m/N,E KiroIDE-{version}-{machine_id}",
        config.system_version, config.node_version
    );
    let mut request = client
        .get(format!("{base_url}/ListAvailableModels"))
        .query(&[("origin", "AI_EDITOR")])
        .header("user-agent", user_agent)
        .header(
            "x-amz-user-agent",
            format!("aws-sdk-js/{sdk} KiroIDE-{version}-{machine_id}"),
        )
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("amz-sdk-request", "attempt=1; max=1")
        .header("Connection", "close")
        .bearer_auth(token)
        .timeout(Duration::from_secs(20));

    // REST 和推理共用当前项目的 profile ARN 规则，不能照搬来源旧版的省略规则。
    if let Some(arn) = credentials.streaming_profile_arn() {
        request = request.query(&[("profileArn", arn)]);
    }
    if credentials.is_api_key_credential() {
        request = request.header("tokentype", "API_KEY");
    }
    if let Some(next_token) = next_token {
        request = request.query(&[("nextToken", next_token)]);
    }
    request
}

pub(crate) async fn fetch_available_models(
    client: &Client,
    credentials: &KiroCredentials,
    config: &Config,
    token: &str,
) -> anyhow::Result<Vec<UpstreamModel>> {
    let regions = region_candidates(credentials, config);
    for (index, region) in regions.iter().enumerate() {
        let result = fetch_region_models(
            client,
            &format!("https://q.{region}.amazonaws.com"),
            credentials,
            config,
            token,
        )
        .await;
        match result {
            Err(error)
                if error
                    .downcast_ref::<reqwest::Error>()
                    .and_then(|e| e.status())
                    == Some(StatusCode::FORBIDDEN)
                    && index + 1 < regions.len() =>
            {
                tracing::debug!("ListAvailableModels 在 {} 返回 403，尝试下一地区", region);
            }
            other => return other,
        }
    }
    anyhow::bail!("没有可用的模型查询端点")
}

async fn fetch_region_models(
    client: &Client,
    base_url: &str,
    credentials: &KiroCredentials,
    config: &Config,
    token: &str,
) -> anyhow::Result<Vec<UpstreamModel>> {
    let mut models = Vec::new();
    let mut next_token = None;
    let mut seen_tokens = HashSet::new();
    for _ in 0..MAX_MODEL_PAGES {
        let page = model_request(
            client,
            base_url,
            credentials,
            config,
            token,
            next_token.as_deref(),
        )
        .send()
        .await?
        .error_for_status()?
        .json::<ListAvailableModelsResponse>()
        .await?;
        models.extend(page.models);
        next_token = page.next_token.filter(|token| !token.is_empty());
        match next_token.as_ref() {
            None => return Ok(models),
            Some(token) => anyhow::ensure!(
                seen_tokens.insert(token.clone()),
                "模型目录分页重复返回同一 nextToken"
            ),
        }
    }
    anyhow::bail!("模型目录分页超过安全上限 ({MAX_MODEL_PAGES})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::model::credentials::{BUILDER_ID_PROFILE_ARN, SOCIAL_PROFILE_ARN};

    fn model(id: &str) -> UpstreamModel {
        serde_json::from_value(serde_json::json!({ "modelId": id })).unwrap()
    }

    #[test]
    fn canonical_model_ids_only_normalize_claude_families() {
        for (input, expected) in [
            ("fable-5.1", "claude-fable-5.1"),
            ("Sonnet-5", "claude-sonnet-5"),
            ("opus-5.5-thinking", "claude-opus-5.5-thinking"),
            ("haiku-4-5-20251001", "claude-haiku-4-5-20251001"),
            (" CLAUDE-OPUS-5.5 ", "claude-opus-5.5"),
            ("claude-fable-5.1", "claude-fable-5.1"),
            ("haiku", "claude-haiku"),
            ("GPT-5.6-Sol", "GPT-5.6-Sol"),
            ("opusfoo-5.5", "opusfoo-5.5"),
            ("claude-sonnetish", "claude-sonnetish"),
            ("amazon-sonnet-5", "amazon-sonnet-5"),
        ] {
            assert_eq!(canonical_model_id(input), expected, "input: {input}");
        }
    }

    #[test]
    fn discovered_lookup_only_aliases_claude_family_prefixes() {
        let models = [
            model("Opus-99.1"),
            model("Claude-Sonnet-99.2"),
            model("Haiku-99.3"),
            model("Claude-Haiku-99.3"),
            model("claude-gpt-99.4"),
            model("opusfoo-99.5"),
        ]
        .into_iter()
        .map(|model| (model.model_id.to_ascii_lowercase(), model))
        .collect();
        for (input, expected) in [
            ("CLAUDE-OPUS-99.1", Some("Opus-99.1")),
            ("opus-99.1", Some("Opus-99.1")),
            ("sonnet-99.2", Some("Claude-Sonnet-99.2")),
            ("claude-sonnet-99.2", Some("Claude-Sonnet-99.2")),
            ("haiku-99.3", Some("Haiku-99.3")),
            ("claude-haiku-99.3", Some("Claude-Haiku-99.3")),
            ("gpt-99.4", None),
            ("claude-opusfoo-99.5", None),
            ("claude-opus-99.1-thinking", None),
        ] {
            assert_eq!(
                find_discovered_model(&models, input).map(|model| model.model_id.as_str()),
                expected,
                "input: {input}"
            );
        }
    }

    #[tokio::test]
    async fn cache_reuses_results_and_preserves_them_on_failure() {
        let cache = ModelCatalogCache::default();
        let models = cache
            .get_or_refresh(|| async {
                let mut discovered = model("test-cache-model");
                discovered.token_limits = Some(crate::kiro::model::available_models::TokenLimits {
                    max_input_tokens: Some(321_000),
                    max_output_tokens: Some(8_000),
                });
                Ok(vec![discovered, model("test-cache-model")])
            })
            .await
            .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(
            discovered_model_id("TEST-CACHE-MODEL").as_deref(),
            Some("test-cache-model")
        );
        assert_eq!(discovered_context_window("test-cache-model"), Some(321_000));
        assert_eq!(discovered_model_id("test-not-discovered"), None);
        assert_eq!(discovered_context_window("test-not-discovered"), None);
        assert_eq!(
            cache
                .get_or_refresh(|| async { panic!("fresh cache must not fetch") })
                .await
                .unwrap()
                .len(),
            1
        );
        cache.state.lock().await.refresh_after = None;
        let stale = cache
            .get_or_refresh(|| async { anyhow::bail!("offline") })
            .await
            .unwrap();
        assert_eq!(stale[0].model_id, "test-cache-model");
        assert_eq!(
            cache
                .get_or_refresh(|| async { panic!("failure backoff must not fetch") })
                .await
                .unwrap()
                .len(),
            1
        );
        cache.state.lock().await.refresh_after = None;
        assert_eq!(
            cache
                .get_or_refresh(|| async { Ok(vec![]) })
                .await
                .unwrap()
                .len(),
            1
        );
        // 无限额、非正数和超出 i32 范围的值均交给转换器的静态默认值处理。
        for limit in [None, Some(0), Some(-1), Some(i64::MAX)] {
            cache.state.lock().await.refresh_after = None;
            cache
                .get_or_refresh(|| async {
                    let mut discovered = model("test-cache-model");
                    discovered.token_limits =
                        Some(crate::kiro::model::available_models::TokenLimits {
                            max_input_tokens: limit,
                            max_output_tokens: None,
                        });
                    Ok(vec![discovered])
                })
                .await
                .unwrap();
            assert_eq!(discovered_context_window("test-cache-model"), None);
        }

        // 与唯一写入全局目录的缓存测试共用生命周期，避免并行测试相互覆盖目录。
        cache.state.lock().await.refresh_after = None;
        cache
            .get_or_refresh(|| async {
                Ok([
                    ("Opus-99.1", Some(123_000)),
                    ("Claude-Sonnet-99.2", Some(456_000)),
                    ("Haiku-99.3", None),
                    ("Claude-Haiku-99.3", Some(789_000)),
                ]
                .into_iter()
                .map(|(id, limit)| {
                    let mut discovered = model(id);
                    discovered.token_limits =
                        Some(crate::kiro::model::available_models::TokenLimits {
                            max_input_tokens: limit,
                            max_output_tokens: None,
                        });
                    discovered
                })
                .collect())
            })
            .await
            .unwrap();
        for (input, expected_id, expected_limit) in [
            ("opus-99.1", "Opus-99.1", Some(123_000)),
            ("claude-opus-99.1", "Opus-99.1", Some(123_000)),
            ("sonnet-99.2", "Claude-Sonnet-99.2", Some(456_000)),
            ("claude-sonnet-99.2", "Claude-Sonnet-99.2", Some(456_000)),
            ("haiku-99.3", "Haiku-99.3", None),
            ("claude-haiku-99.3", "Claude-Haiku-99.3", Some(789_000)),
        ] {
            assert_eq!(discovered_model_id(input).as_deref(), Some(expected_id));
            assert_eq!(discovered_context_window(input), expected_limit);
            for requested in [input.to_string(), format!("{input}-thinking")] {
                assert_eq!(
                    crate::anthropic::map_model(&requested).as_deref(),
                    Some(expected_id)
                );
                assert_eq!(
                    crate::anthropic::get_context_window_size(&requested),
                    expected_limit.unwrap_or(200_000)
                );
            }
        }
    }

    #[tokio::test]
    async fn empty_initial_catalog_is_an_error_and_uses_backoff() {
        let cache = ModelCatalogCache::default();
        assert!(
            cache
                .get_or_refresh(|| async { Ok(vec![model("")]) })
                .await
                .is_err()
        );
        assert!(
            cache
                .get_or_refresh(|| async { panic!("empty catalog must use backoff") })
                .await
                .is_err()
        );
    }

    #[test]
    fn requests_keep_local_profile_arn_and_api_key_rules() {
        let client = Client::new();
        let config = Config::default();
        for (method, expected) in [
            ("idc", BUILDER_ID_PROFILE_ARN),
            ("social", SOCIAL_PROFILE_ARN),
        ] {
            let credentials = KiroCredentials {
                auth_method: Some(method.into()),
                ..Default::default()
            };
            let request = model_request(
                &client,
                "https://q.us-east-1.amazonaws.com",
                &credentials,
                &config,
                "test-token",
                Some("page+/=&?"),
            )
            .build()
            .unwrap();
            let query: HashMap<_, _> = request.url().query_pairs().into_owned().collect();
            assert_eq!(query["profileArn"], expected);
            assert_eq!(query["nextToken"], "page+/=&?");
            assert!(
                request.headers()["user-agent"]
                    .to_str()
                    .unwrap()
                    .contains(USAGE_API_AWS_SDK_VERSION)
            );
            assert!(
                request.headers()["user-agent"]
                    .to_str()
                    .unwrap()
                    .contains(USAGE_API_KIRO_VERSION)
            );
            assert!(request.headers().get("tokentype").is_none());
        }
        let credentials = KiroCredentials {
            kiro_api_key: Some("test-key".into()),
            profile_arn: Some("ignored".into()),
            ..Default::default()
        };
        let request = model_request(
            &client,
            "https://q.us-east-1.amazonaws.com",
            &credentials,
            &config,
            "test-key",
            None,
        )
        .build()
        .unwrap();
        assert_eq!(request.headers()["tokentype"], "API_KEY");
        assert!(
            !request
                .url()
                .query_pairs()
                .any(|(key, _)| key == "profileArn")
        );
    }

    #[test]
    fn region_candidates_honor_configured_region_before_auth_fallbacks() {
        let credentials = KiroCredentials {
            api_region: Some("ap-southeast-1".into()),
            auth_region: Some("eu-west-1".into()),
            ..Default::default()
        };
        assert_eq!(
            region_candidates(&credentials, &Config::default()),
            ["ap-southeast-1", "eu-central-1", "us-east-1"]
        );
        let credentials = KiroCredentials {
            api_region: Some("eu-central-1".into()),
            ..Default::default()
        };
        assert_eq!(
            region_candidates(&credentials, &Config::default()),
            ["eu-central-1", "us-east-1"]
        );
    }

    #[tokio::test]
    async fn paginated_catalog_collects_every_page_and_rejects_loops() {
        use axum::{Json, Router, extract::Query, routing::get};
        let app = Router::new().route("/ListAvailableModels", get(|Query(query): Query<HashMap<String, String>>| async move {
            if query.contains_key("nextToken") {
                Json(serde_json::json!({"models": [{"modelId":"second"}]}))
            } else {
                Json(serde_json::json!({"models": [{"modelId":"first"}], "nextToken": "page+/=&?"}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = Client::builder().no_proxy().build().unwrap();
        let models = fetch_region_models(
            &client,
            &format!("http://{address}"),
            &KiroCredentials::default(),
            &Config::default(),
            "test",
        )
        .await
        .unwrap();
        assert_eq!(
            models
                .iter()
                .map(|m| m.model_id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        server.abort();

        let app = Router::new().route(
            "/ListAvailableModels",
            get(|| async {
                Json(serde_json::json!({"models": [{"modelId":"first"}], "nextToken":"repeated"}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        assert!(
            fetch_region_models(
                &client,
                &format!("http://{address}"),
                &KiroCredentials::default(),
                &Config::default(),
                "test"
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("nextToken")
        );
        server.abort();
    }
}
