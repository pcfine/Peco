// ============================================================================
// TavilyClient — Tavily Search API（面向 LLM/Agent，需 API key）
// ============================================================================
//
// 关闭 answer / raw_content / images（只搜不读，见设计文档 §3.4）。

use serde::{Deserialize, Serialize};

use crate::config::resolve_key;

use super::{SearchError, SearchQuery, SearchResult, make_result, request_error, status_error};

const DEFAULT_BASE_URL: &str = "https://api.tavily.com";

#[derive(Debug, Clone)]
pub struct TavilyClient {
    api_key: String,
    base_url: String,
    http: reqwest::Client,
}

impl TavilyClient {
    /// api_key 支持 `${ENV_VAR}` 语法（与 providers.toml 的 api_key 一致）。
    pub fn new(api_key: &str, base_url: Option<&str>) -> Result<Self, SearchError> {
        let key = resolve_key(api_key).map_err(|_| {
            SearchError::NotConfigured(format!(
                "cannot resolve [web_search.tavily] api_key '{api_key}' (referenced env var is not set)"
            ))
        })?;
        Ok(Self {
            api_key: key,
            base_url: base_url
                .unwrap_or(DEFAULT_BASE_URL)
                .trim_end_matches('/')
                .to_string(),
            http: super::http_client()?,
        })
    }

    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, SearchError> {
        // Tavily 不支持地区参数（设计文档 §3.3），region 忽略。
        let body = TavilyRequest {
            query: query.text.clone(),
            max_results: query.max_results,
            include_answer: false,
            include_raw_content: false,
            include_images: false,
        };

        let response = self
            .http
            .post(format!("{}/search", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(request_error)?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(status_error(status, &text));
        }

        let parsed: TavilyResponse =
            response
                .json::<TavilyResponse>()
                .await
                .map_err(|e| SearchError::Parse {
                    message: e.to_string(),
                })?;

        Ok(parsed
            .results
            .into_iter()
            .map(|item| make_result(item.title, item.url, item.content))
            .collect())
    }
}

#[derive(Debug, Serialize)]
struct TavilyRequest {
    query: String,
    max_results: usize,
    include_answer: bool,
    include_raw_content: bool,
    include_images: bool,
}

#[derive(Debug, Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    results: Vec<TavilyItem>,
}

#[derive(Debug, Deserialize)]
struct TavilyItem {
    title: String,
    url: String,
    #[serde(default)]
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture_with_optional_content() {
        let raw = r#"{
            "query": "rust tokio",
            "results": [
                {"title": "Tokio", "url": "https://tokio.rs", "content": "An asynchronous runtime"},
                {"title": "Bare result", "url": "https://example.com"}
            ]
        }"#;
        let parsed: TavilyResponse = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.results.len(), 2);
        assert!(parsed.results[1].content.is_none());
    }

    #[test]
    fn missing_results_field_defaults_empty() {
        let parsed: TavilyResponse =
            serde_json::from_str(r#"{"answer": "nothing"}"#).expect("parse");
        assert!(parsed.results.is_empty());
    }

    #[test]
    fn new_resolves_env_var_syntax() {
        // 字面量 key 直接通过
        let client = TavilyClient::new("tvly-literal", None).expect("client");
        assert_eq!(client.base_url, DEFAULT_BASE_URL);

        // ${ENV_VAR} 引用了不存在的环境变量 → NotConfigured（文案含变量名）
        let err = TavilyClient::new("${PECO_TEST_TAVILY_MISSING_KEY}", None)
            .expect_err("env var missing");
        assert!(
            err.to_string().contains("PECO_TEST_TAVILY_MISSING_KEY"),
            "got: {err}"
        );
    }

    #[test]
    fn new_strips_trailing_slash_from_base_url() {
        let client = TavilyClient::new("k", Some("https://proxy.example.com/")).expect("client");
        assert_eq!(client.base_url, "https://proxy.example.com");
    }
}
