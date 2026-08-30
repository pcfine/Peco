// ============================================================================
// WebSearchTool — 内置 web_search 工具壳
// ============================================================================
//
// 仅做参数解析、调用 SearchBackend、格式化输出。引擎逻辑在
// crates/peco-core/src/search/（设计文档 docs/design/web-search-design.md）。
// 未配置 [web_search] 时本工具不注册（warn + skip，同 workflow_access 模式）。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use crate::search::{SearchBackend, SearchError, SearchQuery};

use super::{ToolDyn, ToolError};

/// 搜索引擎层错误 → 工具层错误（Display 文案保留机器可读关键字）。
fn search_err(err: SearchError) -> ToolError {
    ToolError::ToolCallError(err.into())
}

pub struct WebSearchTool {
    backend: Arc<SearchBackend>,
}

impl WebSearchTool {
    pub fn new(backend: Arc<SearchBackend>) -> Self {
        Self { backend }
    }
}

impl ToolDyn for WebSearchTool {
    fn name(&self) -> String {
        "web_search".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_search".to_string(),
            description:
                "Search the web for real-time information. Returns a list of results with \
                title, url and snippet. Use this to answer questions that depend on current events \
                or facts beyond your knowledge cutoff; combine with the 'fetch' tool to read the \
                full page content of promising results."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default 5, max 20)."
                    },
                    "region": {
                        "type": "string",
                        "description": "Optional region/language hint, engine-dependent \
                            (SearXNG: language like 'en'; Brave: country code like 'us'; \
                            Tavily: ignored)."
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args {
                query: String,
                max_results: Option<usize>,
                region: Option<String>,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let query = SearchQuery::new(parsed.query, parsed.max_results, parsed.region);

            let results = self.backend.search(&query).await.map_err(search_err)?;

            // 输出契约：{"results": [...]}；零结果是合法业务态，返回空数组。
            // compact JSON — 结果直接进模型上下文，缩进空格是纯 token 开销。
            serde_json::to_string(&json!({ "results": results })).map_err(ToolError::JsonError)
        })
    }
}

// ============================================================================
// 测试 — 参数解析、clamp、输出契约、未配置语义（无网络）
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::searxng::SearxngClient;

    fn tool() -> WebSearchTool {
        WebSearchTool::new(Arc::new(SearchBackend::Searxng(
            SearxngClient::new("http://localhost:8888").expect("client"),
        )))
    }

    fn definition_param_names(def: &ToolDefinition) -> Vec<String> {
        def.parameters["properties"]
            .as_object()
            .expect("properties object")
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn definition_has_query_required_and_optional_params() {
        let def = tool().definition();
        assert_eq!(def.name, "web_search");
        let required = def.parameters["required"].as_array().expect("required");
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].as_str(), Some("query"));

        let params = definition_param_names(&def);
        assert!(params.contains(&"query".to_string()));
        assert!(params.contains(&"max_results".to_string()));
        assert!(params.contains(&"region".to_string()));
    }

    /// 非法 JSON 参数 → JsonError（非 panic）。
    #[tokio::test]
    async fn invalid_args_json_is_rejected() {
        let tool = tool();
        let result = tool.call("not json".to_string()).await;
        assert!(matches!(result, Err(ToolError::JsonError(_))));
    }
}
