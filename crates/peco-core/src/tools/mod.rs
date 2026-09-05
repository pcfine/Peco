// ============================================================================
// tools — peco-core's tool abstraction + concrete implementations
// ============================================================================

mod agent_tools;
mod deps;
mod fetch;
mod knowledge_tools;
mod mcp_tools;
mod shell;
mod skill_tools;
mod sub_agent;
mod tool_factory;
mod tool_register;
mod web_search;
mod workspace_info;

pub use agent_tools::{DeleteAgent, ReadAgent, SaveAgent};
pub use deps::{
    AgentAccess, KnowledgeAccess, McpAccess, McpServerInfo, SkillProvider, ToolDependencies,
};
pub use fetch::Fetch;
pub use knowledge_tools::{
    AddFactsToKnowledgeBase, AddToKnowledgeBase, GetKnowledgeBaseDocs, ListKnowledgeBases,
    QueryEntityFacts, SearchKnowledge, SyncKnowledgeBase,
};
pub use mcp_tools::{DeleteMcpServer, ListMcpServers, SaveMcpServer, TestMcpConnection};
pub use shell::{ShellExec, ShellTool};
pub use skill_tools::{DeleteSkill, ListSkills, ReadSkill, SaveSkill};
pub use sub_agent::{DelegateSubAgent, RunParallelSubAgents};
pub use tool_factory::{DefaultToolsExecutor, StringError};
pub use tool_register::ToolRegister;
pub use web_search::WebSearchTool;
pub use workspace_info::ShowWorkspace;

use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;

pub use model_provider::Content;
pub use model_provider::ToolDefinition;

// ── ToolError ─────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{0}")]
    ToolCallError(#[from] Box<dyn std::error::Error + Send + Sync>),
    #[error("JsonError: {0}")]
    JsonError(#[from] serde_json::Error),
}

// ── Tool trait ────────────────────────────────────────────────────────────────

pub trait Tool: Send + Sync {
    const NAME: &'static str;
    type Args: for<'a> serde::Deserialize<'a> + Send + Sync;
    type Output: serde::Serialize;
    type Error: std::error::Error + Send + Sync + 'static;

    fn name(&self) -> String {
        Self::NAME.to_string()
    }
    fn definition(&self) -> ToolDefinition;
    fn call(
        &self,
        args: Self::Args,
    ) -> impl Future<Output = Result<Self::Output, Self::Error>> + Send;
}

// ── ToolDyn — object-safe version ─────────────────────────────────────────────

/// 对象安全的动态工具接口。
///
/// [`call`](ToolDyn::call) 返回中立的
/// [`Content`](model_provider::response::Content)：纯文本工具结果为
/// `Content::Text`，需要回传图片（截图、图表）的工具可返回
/// `Content::Parts`（文本 + `data:` URI 图片部件混排）。
pub trait ToolDyn: Send + Sync {
    fn name(&self) -> String;
    fn definition(&self) -> ToolDefinition;
    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>>;
}

// ── Blanket impl: Tool → ToolDyn ──────────────────────────────────────────────

impl<T: Tool> ToolDyn for T {
    fn name(&self) -> String {
        <Self as Tool>::name(self)
    }
    fn definition(&self) -> ToolDefinition {
        <Self as Tool>::definition(self)
    }
    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let parsed = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let output = <Self as Tool>::call(self, parsed)
                .await
                .map_err(|e| ToolError::ToolCallError(Box::new(e)))?;
            match serde_json::to_value(output)? {
                serde_json::Value::String(s) => Ok(Content::Text(s)),
                other => Ok(Content::Text(other.to_string())),
            }
        })
    }
}

// ── ToolExecutor trait ──────────────────────────────────────────────────────

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, name: &str, args: &str) -> Result<Content, String>;
    fn definitions(&self) -> Vec<ToolDefinition>;
    fn add_tool(&self, tool: Box<dyn ToolDyn>) -> Result<(), String> {
        let _ = tool.name();
        Err("add_tool: dynamic tool registration not supported by this executor".into())
    }
    fn remove_tool(&self, name: &str) -> Result<(), String> {
        let _ = name;
        Err("remove_tool: dynamic tool removal not supported by this executor".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 宏分派矩阵验证 ──────────────────────────────────────────────────
    //
    // 返回 `String` 的 fn 走 Tool + blanket 路径（裸文本，非 JSON dump）；
    // 返回 `Content` 的 fn 直接生成 ToolDyn impl（部件透传零转换）。
    // 两条路径都以 ToolDyn::call 的动态分发请求路径为准断言。

    use peco_derive::peco_tool;

    /// 返回 String 的样例工具（Tool 路径）。
    #[peco_tool(
        name = "echo_text",
        description = "echo text",
        params(text = "text to echo")
    )]
    async fn echo_text_tool(text: String) -> Result<String, ToolError> {
        Ok(format!("echo: {text}"))
    }

    /// 返回 Content 的样例工具（ToolDyn 直通路径）。
    #[peco_tool(
        name = "echo_parts",
        description = "echo parts",
        params(text = "text to echo")
    )]
    async fn echo_parts_tool(text: String) -> Result<Content, ToolError> {
        Ok(Content::Parts(vec![
            model_provider::ContentPart::Text {
                text: format!("echo: {text}"),
            },
            model_provider::ContentPart::Image {
                url: "data:image/png;base64,AAAA".to_string(),
                detail: None,
            },
        ]))
    }

    #[tokio::test]
    async fn string_output_tool_yields_bare_text_content() {
        // 裸文本而非 JSON dump：`"echo: hi"` 不带引号、无转义
        let out = ToolDyn::call(&ECHO_TEXT_TOOL, r#"{"text": "hi"}"#.to_string())
            .await
            .unwrap();
        assert_eq!(out, Content::Text("echo: hi".to_string()));
    }

    #[tokio::test]
    async fn content_output_tool_passthrough_parts_unconverted() {
        let out = ToolDyn::call(&ECHO_PARTS_TOOL, r#"{"text": "hi"}"#.to_string())
            .await
            .unwrap();
        let Content::Parts(parts) = out else {
            panic!("expected Parts passthrough, got {out:?}");
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[0],
            model_provider::ContentPart::Text {
                text: "echo: hi".to_string()
            }
        );
        assert_eq!(
            parts[1],
            model_provider::ContentPart::Image {
                url: "data:image/png;base64,AAAA".to_string(),
                detail: None,
            }
        );
    }
}
