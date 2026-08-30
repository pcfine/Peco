// ============================================================================
// search — 内置 web 搜索引擎层
// ============================================================================
//
// 统一的搜索结果结构 + 枚举式后端分发。各引擎的请求构造与响应解析
// 在各自模块内完成，对外只暴露 `SearchBackend::search()`。
// 引擎集合封闭且配置期已知，因此用枚举而非 trait object：
// match 分发编译期穷尽，免 Box<dyn> / async_trait 开销。
// 设计文档：docs/design/web-search-design.md

pub mod brave;
pub mod searxng;
pub mod tavily;

use crate::config::WebSearchConfig;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use self::brave::BraveClient;
use self::searxng::SearxngClient;
use self::tavily::TavilyClient;

/// snippet 统一截断上限（字符数）。只搜不读：控制 token 消耗。
pub const SNIPPET_MAX_CHARS: usize = 500;

/// 统一搜索结果（引擎差异在各自 client 内抹平）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    /// 摘要文本。引擎未返回时为空串。
    #[serde(default)]
    pub snippet: String,
}

/// 一次搜索请求的参数。
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub text: String,
    /// 期望返回条数（缺省 5，clamp 1..=20）。
    pub max_results: usize,
    /// 地区/语言提示，语义按引擎映射（见设计文档 §3.3）；None 时用引擎默认。
    pub region: Option<String>,
}

impl SearchQuery {
    /// 构造请求参数，规范化 `max_results`（None → 默认 5，clamp 到 1..=20）。
    pub fn new(
        text: impl Into<String>,
        max_results: Option<usize>,
        region: Option<String>,
    ) -> Self {
        Self {
            text: text.into(),
            max_results: max_results
                .unwrap_or(DEFAULT_MAX_RESULTS)
                .clamp(1, MAX_RESULTS_CAP),
            region,
        }
    }
}

/// max_results 缺省值与 clamp 上限（工具参数 → 引擎请求共用）。
const DEFAULT_MAX_RESULTS: usize = 5;
const MAX_RESULTS_CAP: usize = 20;

/// 搜索层错误。Display 文案含机器可读关键字，
/// 工具层原样透传给 agent（`rate limit` / `429`、`timeout`、`unauthorized`、`401`）。
#[derive(Debug, Error)]
pub enum SearchError {
    #[error("web_search not configured: {0}")]
    NotConfigured(String),
    #[error("web_search HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("web_search request timeout")]
    Timeout,
    #[error("web_search unauthorized (HTTP {status}): check search API key")]
    Unauthorized { status: u16 },
    /// key 类引擎的 403 与 SearXNG 的 403 语义不同：前者是鉴权失败（走
    /// `Unauthorized`），后者几乎总是实例未启用 JSON 输出格式，用 `hint`
    /// 给出部署侧修复指引，避免误导 agent 去检查不存在的 API key。
    #[error("web_search forbidden (HTTP {status}): {hint}")]
    Forbidden { status: u16, hint: String },
    #[error("web_search rate limit (HTTP {status}): 429")]
    RateLimited { status: u16 },
    #[error("web_search request failed (HTTP {status}): {body}")]
    Status { status: u16, body: String },
    #[error("web_search failed to parse response: {message}")]
    Parse { message: String },
}

/// 搜索后端 — 配置期确定的封闭枚举。
#[derive(Debug, Clone)]
pub enum SearchBackend {
    Searxng(SearxngClient),
    Tavily(TavilyClient),
    Brave(BraveClient),
}

impl SearchBackend {
    /// 从 `[web_search]` 配置段构造后端。
    ///
    /// 返回 Err(String) 时的文案用于 warn 日志与 skip 决策：
    /// provider 名缺失/未知、或所选引擎的配置段缺失。
    pub fn from_config(config: &WebSearchConfig) -> Result<Self, String> {
        let provider = config.provider.as_deref().unwrap_or_default();
        match provider {
            "searxng" => {
                let searxng = config.searxng.as_ref().ok_or_else(|| {
                    "provider is 'searxng' but the [web_search.searxng] section is missing"
                        .to_string()
                })?;
                SearxngClient::new(&searxng.base_url)
                    .map(Self::Searxng)
                    .map_err(|e| e.to_string())
            }
            "tavily" => {
                let tavily = config.tavily.as_ref().ok_or_else(|| {
                    "provider is 'tavily' but the [web_search.tavily] section is missing"
                        .to_string()
                })?;
                TavilyClient::new(&tavily.api_key, tavily.base_url.as_deref())
                    .map(Self::Tavily)
                    .map_err(|e| e.to_string())
            }
            "brave" => {
                let brave = config.brave.as_ref().ok_or_else(|| {
                    "provider is 'brave' but the [web_search.brave] section is missing".to_string()
                })?;
                BraveClient::new(&brave.api_key)
                    .map(Self::Brave)
                    .map_err(|e| e.to_string())
            }
            other => {
                if other.is_empty() {
                    Err("no search provider configured".to_string())
                } else {
                    Err(format!(
                        "unknown search provider '{other}' (expected: searxng, tavily, brave)"
                    ))
                }
            }
        }
    }

    /// 后端名（用于日志与调试）。
    pub fn name(&self) -> &'static str {
        match self {
            Self::Searxng(_) => "searxng",
            Self::Tavily(_) => "tavily",
            Self::Brave(_) => "brave",
        }
    }

    /// 从可选配置段构造后端。未配置（None）返回 None；
    /// 配置存在但无效时 warn 并返回 None — 调用方据此 skip web_search 工具。
    pub fn from_config_opt(config: Option<&WebSearchConfig>) -> Option<Self> {
        let config = config?;
        match Self::from_config(config) {
            Ok(backend) => Some(backend),
            Err(reason) => {
                tracing::warn!(
                    reason = %reason,
                    "web_search tool disabled: invalid [web_search] config"
                );
                None
            }
        }
    }

    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, SearchError> {
        match self {
            Self::Searxng(client) => client.search(query).await,
            Self::Tavily(client) => client.search(query).await,
            Self::Brave(client) => client.search(query).await,
        }
    }
}

/// HTTP 非成功状态 → 错误映射（各引擎 client 共用）。
///
/// 401/403 → Unauthorized；429 → RateLimited；其余 → Status（附 body 摘要）。
pub(crate) fn status_error(status: reqwest::StatusCode, body: &str) -> SearchError {
    let code = status.as_u16();
    match code {
        401 | 403 => SearchError::Unauthorized { status: code },
        429 => SearchError::RateLimited { status: code },
        _ => SearchError::Status {
            status: code,
            body: truncate_chars(body, 200),
        },
    }
}

/// reqwest 请求错误 → 错误映射（区分超时与其他 HTTP 层失败）。
pub(crate) fn request_error(err: reqwest::Error) -> SearchError {
    if err.is_timeout() {
        SearchError::Timeout
    } else {
        SearchError::Http(err)
    }
}

/// 按字符数截断（不感知编码/HTML 语义，见设计文档 §8）。
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// 从引擎原始字段生成统一结果（snippet 缺省为空串并截断）。
pub(crate) fn make_result(title: String, url: String, raw_snippet: Option<String>) -> SearchResult {
    SearchResult {
        title,
        url,
        snippet: raw_snippet
            .map(|s| truncate_chars(&s, SNIPPET_MAX_CHARS))
            .unwrap_or_default(),
    }
}

/// 共享 HTTP client 构造：统一超时（设计 D-8：单请求 30s）与 User-Agent。
pub(crate) fn http_client() -> Result<reqwest::Client, SearchError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("peco/0.1")
        .build()
        .map_err(SearchError::Http)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── SearchQuery::new ─────────────────────────────────────────────

    #[test]
    fn search_query_clamps_max_results() {
        let default = SearchQuery::new("rust async", None, None);
        assert_eq!(default.max_results, 5);
        assert_eq!(default.text, "rust async");
        assert_eq!(default.region, None);

        assert_eq!(SearchQuery::new("q", Some(0), None).max_results, 1);
        assert_eq!(SearchQuery::new("q", Some(3), None).max_results, 3);
        assert_eq!(SearchQuery::new("q", Some(100), None).max_results, 20);
    }

    // ── from_config ──────────────────────────────────────────────────

    fn config(toml_str: &str) -> WebSearchConfig {
        toml::from_str(toml_str).expect("valid toml")
    }

    #[test]
    fn from_config_builds_searxng_backend() {
        let cfg = config(
            r#"
provider = "searxng"
[searxng]
base_url = "http://localhost:8888"
"#,
        );
        let backend = SearchBackend::from_config(&cfg).expect("backend");
        assert_eq!(backend.name(), "searxng");
    }

    #[test]
    fn from_config_builds_tavily_backend_with_env_key() {
        // resolve_key 在 client 构造内展开 ${ENV_VAR}；此处无该环境变量时应报错
        let cfg = config(
            r#"
provider = "tavily"
[tavily]
api_key = "${PECO_TEST_MISSING_ENV_VAR}"
"#,
        );
        let err = SearchBackend::from_config(&cfg).expect_err("env var missing");
        assert!(err.contains("PECO_TEST_MISSING_ENV_VAR"), "got: {err}");
    }

    #[test]
    fn from_config_rejects_missing_provider_section() {
        let cfg = config("provider = \"searxng\"");
        let err = SearchBackend::from_config(&cfg).expect_err("section missing");
        assert!(err.contains("[web_search.searxng]"), "got: {err}");
    }

    #[test]
    fn from_config_rejects_unknown_provider() {
        let cfg = config("provider = \"google\"");
        let err = SearchBackend::from_config(&cfg).expect_err("unknown provider");
        assert!(
            err.contains("unknown search provider 'google'"),
            "got: {err}"
        );
    }

    #[test]
    fn from_config_rejects_empty_provider() {
        let cfg = WebSearchConfig::default();
        let err = SearchBackend::from_config(&cfg).expect_err("no provider");
        assert!(err.contains("no search provider configured"), "got: {err}");
    }

    // ── status_error 映射 ────────────────────────────────────────────

    #[test]
    fn status_error_maps_auth_and_rate_limit() {
        let auth = status_error(reqwest::StatusCode::UNAUTHORIZED, "denied");
        assert!(matches!(auth, SearchError::Unauthorized { status: 401 }));
        assert!(auth.to_string().contains("unauthorized"));

        let forbidden = status_error(reqwest::StatusCode::FORBIDDEN, "denied");
        assert!(matches!(
            forbidden,
            SearchError::Unauthorized { status: 403 }
        ));

        let rate = status_error(reqwest::StatusCode::TOO_MANY_REQUESTS, "slow down");
        assert!(matches!(rate, SearchError::RateLimited { status: 429 }));
        let msg = rate.to_string();
        assert!(
            msg.contains("rate limit") && msg.contains("429"),
            "got: {msg}"
        );

        let other = status_error(reqwest::StatusCode::BAD_GATEWAY, "upstream boom");
        assert!(matches!(other, SearchError::Status { status: 502, .. }));
        assert!(other.to_string().contains("upstream boom"));
    }

    // ── snippet 截断与结果构造 ───────────────────────────────────────

    #[test]
    fn truncate_chars_caps_by_char_count() {
        assert_eq!(truncate_chars("abc", 5), "abc");
        let truncated = truncate_chars("你好世界这话有点长", 4);
        assert_eq!(truncated, "你好世界");
        assert_eq!(truncated.chars().count(), 4);
    }

    #[test]
    fn make_result_defaults_empty_snippet_and_truncates() {
        let no_snippet = make_result("t".into(), "https://a".into(), None);
        assert_eq!(no_snippet.snippet, "");

        let long = "x".repeat(600);
        let truncated = make_result("t".into(), "https://a".into(), Some(long));
        assert_eq!(truncated.snippet.chars().count(), SNIPPET_MAX_CHARS);
    }
}
