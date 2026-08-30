// ============================================================================
// BraveClient — Brave Search API（官方 Search API，需 API key）
// ============================================================================

use serde::Deserialize;

use crate::config::resolve_key;

use super::{SearchError, SearchQuery, SearchResult, make_result, request_error, status_error};

const DEFAULT_BASE_URL: &str = "https://api.search.brave.com/res/v1/web/search";

#[derive(Debug, Clone)]
pub struct BraveClient {
    api_key: String,
    http: reqwest::Client,
}

impl BraveClient {
    /// api_key 支持 `${ENV_VAR}` 语法（与 providers.toml 的 api_key 一致）。
    pub fn new(api_key: &str) -> Result<Self, SearchError> {
        let key = resolve_key(api_key).map_err(|_| {
            SearchError::NotConfigured(format!(
                "cannot resolve [web_search.brave] api_key '{api_key}' (referenced env var is not set)"
            ))
        })?;
        Ok(Self {
            api_key: key,
            http: super::http_client()?,
        })
    }

    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, SearchError> {
        let mut params: Vec<(&str, String)> = vec![
            ("q", query.text.clone()),
            ("count", query.max_results.to_string()),
        ];
        if let Some(region) = &query.region {
            // Brave 的 region 映射为 country 国家码（如 "us"），见设计文档 §3.3。
            params.push(("country", region.clone()));
        }

        let response = self
            .http
            .get(DEFAULT_BASE_URL)
            .query(&params)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", &self.api_key)
            .send()
            .await
            .map_err(request_error)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(status_error(status, &body));
        }

        let parsed: BraveResponse =
            response
                .json::<BraveResponse>()
                .await
                .map_err(|e| SearchError::Parse {
                    message: e.to_string(),
                })?;

        Ok(parsed
            .web
            .map(|web| {
                web.results
                    .into_iter()
                    .map(|item| make_result(item.title, item.url, item.description))
                    .collect()
            })
            .unwrap_or_default())
    }
}

#[derive(Debug, Deserialize)]
struct BraveResponse {
    /// 无结果时 Brave 可能省略 web 节点。
    web: Option<BraveWeb>,
}

#[derive(Debug, Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveItem>,
}

#[derive(Debug, Deserialize)]
struct BraveItem {
    title: String,
    url: String,
    #[serde(default)]
    description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture_with_optional_description() {
        let raw = r#"{
            "web": {
                "results": [
                    {"title": "Rust", "url": "https://rust-lang.org", "description": "A language"},
                    {"title": "Bare", "url": "https://example.com"}
                ]
            }
        }"#;
        let parsed: BraveResponse = serde_json::from_str(raw).expect("parse");
        let results = parsed.web.expect("web node").results;
        assert_eq!(results.len(), 2);
        assert!(results[1].description.is_none());
    }

    #[test]
    fn missing_web_node_yields_no_results() {
        let parsed: BraveResponse =
            serde_json::from_str(r#"{"query": {"original": "x"}}"#).expect("parse");
        assert!(parsed.web.is_none());
    }

    #[test]
    fn new_resolves_env_var_syntax() {
        let err = BraveClient::new("${PECO_TEST_BRAVE_MISSING_KEY}").expect_err("env var missing");
        assert!(
            err.to_string().contains("PECO_TEST_BRAVE_MISSING_KEY"),
            "got: {err}"
        );
    }
}
