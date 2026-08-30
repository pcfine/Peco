// ============================================================================
// SearxngClient — 自托管 SearXNG 元搜索引擎（免 key，默认引擎）
// ============================================================================
//
// 依赖实例在 settings.yml 的 search.formats 中启用 "json" 输出格式，
// 否则请求返回 403（部署注意事项，见设计文档 §8）。

use serde::Deserialize;

use super::{SearchError, SearchQuery, SearchResult, make_result, request_error, status_error};

#[derive(Debug, Clone)]
pub struct SearxngClient {
    base_url: String,
    http: reqwest::Client,
}

impl SearxngClient {
    pub fn new(base_url: &str) -> Result<Self, SearchError> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: super::http_client()?,
        })
    }

    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, SearchError> {
        let mut params: Vec<(&str, String)> =
            vec![("q", query.text.clone()), ("format", "json".to_string())];
        if let Some(region) = &query.region {
            params.push(("language", region.clone()));
        }

        let response = self
            .http
            .get(format!("{}/search", self.base_url))
            .query(&params)
            .send()
            .await
            .map_err(request_error)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(status_error_searxng(status, &body));
        }

        let parsed: SearxngResponse =
            response
                .json::<SearxngResponse>()
                .await
                .map_err(|e| SearchError::Parse {
                    message: e.to_string(),
                })?;

        Ok(parsed
            .results
            .into_iter()
            .map(|item| make_result(item.title, item.url, item.content))
            .take(query.max_results)
            .collect())
    }
}

#[derive(Debug, Deserialize)]
struct SearxngResponse {
    #[serde(default)]
    results: Vec<SearxngItem>,
}

/// SearXNG 的 403 与 key 类引擎含义不同：免 key，403 几乎总是实例未启用
/// JSON 输出格式。用专用 hint 替代通用的"检查 API key"误导文案。
fn status_error_searxng(status: reqwest::StatusCode, body: &str) -> SearchError {
    if status.as_u16() == 403 {
        return SearchError::Forbidden {
            status: 403,
            hint: "self-hosted SearXNG must enable 'json' in settings.yml search.formats"
                .to_string(),
        };
    }
    status_error(status, body)
}

#[derive(Debug, Deserialize)]
struct SearxngItem {
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
            "query": "rust async",
            "results": [
                {"title": "Tokio docs", "url": "https://tokio.rs", "content": "async runtime"},
                {"title": "No snippet here", "url": "https://example.com"}
            ]
        }"#;
        let parsed: SearxngResponse = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.results.len(), 2);
        assert_eq!(parsed.results[0].content.as_deref(), Some("async runtime"));
        assert!(parsed.results[1].content.is_none());
    }

    #[test]
    fn parses_empty_results() {
        let parsed: SearxngResponse =
            serde_json::from_str(r#"{"query": "x", "results": []}"#).expect("parse");
        assert!(parsed.results.is_empty());
    }

    #[test]
    fn missing_results_field_defaults_empty() {
        let parsed: SearxngResponse = serde_json::from_str(r#"{"query": "x"}"#).expect("parse");
        assert!(parsed.results.is_empty());
    }

    #[test]
    fn new_trims_trailing_slash() {
        let client = SearxngClient::new("http://localhost:8888/").expect("client");
        assert_eq!(client.base_url, "http://localhost:8888");
    }

    #[test]
    fn forbidden_maps_to_json_format_hint() {
        let err = status_error_searxng(reqwest::StatusCode::FORBIDDEN, "Invalid format");
        let msg = err.to_string();
        assert!(msg.contains("search.formats"), "got: {msg}");

        // 其他状态码仍走通用映射（429 → RateLimited）
        let rate = status_error_searxng(reqwest::StatusCode::TOO_MANY_REQUESTS, "slow");
        assert!(matches!(rate, SearchError::RateLimited { status: 429 }));
    }
}
