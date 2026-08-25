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
pub use shell::ShellExec;
pub use skill_tools::{DeleteSkill, ListSkills, ReadSkill, SaveSkill};
pub use sub_agent::{DelegateSubAgent, RunParallelSubAgents};
pub use tool_factory::{DefaultToolsExecutor, StringError};
pub use tool_register::ToolRegister;
pub use workspace_info::ShowWorkspace;

use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;

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

pub trait ToolDyn: Send + Sync {
    fn name(&self) -> String;
    fn definition(&self) -> ToolDefinition;
    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>>;
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
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let parsed = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let output = <Self as Tool>::call(self, parsed)
                .await
                .map_err(|e| ToolError::ToolCallError(Box::new(e)))?;
            match serde_json::to_value(output)? {
                serde_json::Value::String(s) => Ok(s),
                other => Ok(other.to_string()),
            }
        })
    }
}

// ── ToolExecutor trait ──────────────────────────────────────────────────────

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, name: &str, args: &str) -> Result<String, String>;
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
