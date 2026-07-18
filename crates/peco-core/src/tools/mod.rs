// ============================================================================
// tools — peco-core's tool abstraction + concrete implementations
// ============================================================================
//
// This module provides:
// - [`Tool`] trait — compile-time tool definition with sync `definition()`
// - [`ToolDyn`] trait — object-safe version for heterogeneous tool storage
// - [`ToolDefinition`] — JSON Schema description sent to LLM providers
// - [`ToolError`] — unified error type for tool execution
// - blanket impl `impl<T: Tool> ToolDyn for T`
// - [`ToolFactory`] — global registry of built-in tools
// - Concrete tool implementations: shell, fetch, skill
//
// Named after the Rust std convention: `error/` (trait) vs `errors/` (impls).

mod fetch;
mod shell;
mod skill;
mod sub_agent;
mod tool_factory;

pub use fetch::Fetch;
pub use shell::ShellExec;
pub use skill::ReadSkill;
pub use sub_agent::{
    DelegateSubAgent, RunParallelSubAgents,
};
#[allow(unused_imports)]
pub use tool_factory::{DefaultToolsExecutor, StringError, ToolFactory};

use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;

pub use model_provider::ToolDefinition;

// ── ToolError ─────────────────────────────────────────────────────────────────

/// Unified error type for tool execution.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// The tool's own logic produced an error.
    #[error("{0}")]
    ToolCallError(#[from] Box<dyn std::error::Error + Send + Sync>),

    /// JSON serialization or deserialization failed.
    #[error("JsonError: {0}")]
    JsonError(#[from] serde_json::Error),
}

// ── Tool trait ────────────────────────────────────────────────────────────────

/// LLM-callable tool abstraction.
///
/// Each tool has:
/// - A compile-time [`NAME`](Tool::NAME) constant
/// - A set of JSON Schema-described parameters ([`Args`](Tool::Args))
/// - An async [`call`](Tool::call) method that produces [`Output`](Tool::Output)
///   or [`Error`](Tool::Error)
///
///
/// The sync `definition()` is possible because the JSON Schema is generated
/// at compile time via `#[peco_tool]`.
pub trait Tool: Send + Sync {
    /// Globally unique tool name.
    const NAME: &'static str;

    /// Parameter type, automatically generated from the function signature.
    type Args: for<'a> serde::Deserialize<'a> + Send + Sync;
    /// Success return type, must implement `Serialize`.
    type Output: serde::Serialize;
    /// Error type produced by the tool.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Returns the tool name (delegates to [`NAME`](Tool::NAME) by default).
    fn name(&self) -> String {
        Self::NAME.to_string()
    }

    /// Returns the tool definition (name, description, parameter JSON Schema).
    fn definition(&self) -> ToolDefinition;

    /// Execute the tool with the given deserialized arguments.
    fn call(
        &self,
        args: Self::Args,
    ) -> impl Future<Output = Result<Self::Output, Self::Error>> + Send;
}

// ── ToolDyn — object-safe version ─────────────────────────────────────────────

/// Object-safe version of [`Tool`], designed for `Box<dyn ToolDyn>` storage.
///
/// Dynamic dispatch happens at the cost of:
/// - JSON-encoded `String` args instead of typed deserialization
/// - `Pin<Box<dyn Future>>` instead of `impl Future`
pub trait ToolDyn: Send + Sync {
    /// Returns the tool name.
    fn name(&self) -> String;
    /// Returns the tool definition.
    fn definition(&self) -> ToolDefinition;
    /// Execute the tool with JSON-encoded arguments.
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
            // Serialize output: strings returned verbatim, other types JSON-serialized
            match serde_json::to_value(output)? {
                serde_json::Value::String(s) => Ok(s),
                other => Ok(other.to_string()),
            }
        })
    }
}

// ── ToolExecutor trait ──────────────────────────────────────────────────────

/// A tool executor that can execute tool calls by name.
///
/// This is the **unified** tool-execution interface shared by both the
/// [`agent`](https://crates.io/crates/agent) crate's ReAct loop and
/// peco-core's tool-composition system.  Because it lives in
/// `model-provider` (the foundational crate both depend on), there is no
/// adapter boilerplate needed when passing a [`DefaultToolsExecutor`] or
/// [`McpToolExecutor`] to an [`Agent`].
///
/// ```
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Execute a tool by name with its JSON-encoded arguments.
    ///
    /// Returns `Ok(result_string)` on success, or `Err(error_message)` on failure.
    /// The error message is forwarded to the model as part of the conversation,
    /// allowing the model to self-correct.
    async fn execute(&self, name: &str, args: &str) -> Result<String, String>;

    /// Return the tool definitions to advertise to the model.
    fn definitions(&self) -> Vec<ToolDefinition>;

    /// Add a tool dynamically to this executor.
    ///
    /// Implementors should use interior mutability (e.g. `Mutex` or `RwLock`)
    /// to allow dynamic registration through `&self`.
    fn add_tool(&self, tool: Box<dyn ToolDyn>) -> Result<(), String> {
        let _ = tool.name();
        Err("add_tool: dynamic tool registration not supported by this executor".into())
    }

    /// Remove a previously added tool by name.
    ///
    /// Returns `Ok(())` if the tool was removed, or `Err(...)` if no tool
    /// with the given name was found.
    ///
    /// The default implementation returns an error; override it to support
    /// dynamic tool removal.
    ///
    /// Implementors should use interior mutability (e.g. `Mutex` or `RwLock`)
    /// to allow dynamic removal through `&self`.
    fn remove_tool(&self, name: &str) -> Result<(), String> {
        let _ = name;
        Err("remove_tool: dynamic tool removal not supported by this executor".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_count() {
        assert_eq!(ToolFactory::global().tool_count(), 10);
    }

    #[test]
    fn test_contains_shell() {
        assert!(ToolFactory::global().contains("shell"));
    }

    #[test]
    fn test_contains_fetch() {
        assert!(ToolFactory::global().contains("fetch"));
    }

    #[test]
    fn test_contains_read_skill() {
        assert!(ToolFactory::global().contains("read_skill"));
    }

    #[test]
    fn test_contains_search_knowledge() {
        assert!(ToolFactory::global().contains("search_knowledge"));
    }

    #[test]
    fn test_contains_list_knowledge_bases() {
        assert!(ToolFactory::global().contains("list_knowledge_bases"));
    }

    #[test]
    fn test_contains_sync_knowledge_base() {
        assert!(ToolFactory::global().contains("sync_knowledge_base"));
    }

    #[test]
    fn test_contains_add_to_knowledge_base() {
        assert!(ToolFactory::global().contains("add_to_knowledge_base"));
    }

    #[test]
    fn test_contains_get_knowledge_base_docs() {
        assert!(ToolFactory::global().contains("get_knowledge_base_docs"));
    }

    #[test]
    fn test_get_shell_tool() {
        let tool = ToolFactory::global().get_tool("shell");
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "shell");
    }

    #[test]
    fn test_get_fetch_tool() {
        let tool = ToolFactory::global().get_tool("fetch");
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "fetch");
    }

    #[test]
    fn test_get_read_skill_tool() {
        let tool = ToolFactory::global().get_tool("read_skill");
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "read_skill");
    }

    #[test]
    fn test_contains_delegate_sub_agent() {
        assert!(ToolFactory::global().contains("delegate_sub_agent"));
    }

    #[test]
    fn test_contains_run_parallel_sub_agents() {
        assert!(ToolFactory::global().contains("run_parallel_sub_agents"));
    }

    #[test]
    fn test_get_delegate_sub_agent_tool() {
        let tool = ToolFactory::global().get_tool("delegate_sub_agent");
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "delegate_sub_agent");
    }

    #[test]
    fn test_get_run_parallel_sub_agents_tool() {
        let tool = ToolFactory::global().get_tool("run_parallel_sub_agents");
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "run_parallel_sub_agents");
    }

    #[test]
    fn test_get_nonexistent() {
        assert!(ToolFactory::global().get_tool("nonexistent").is_none());
    }

    #[test]
    fn test_tool_names() {
        let names = ToolFactory::global().tool_names();
        assert_eq!(names.len(), 10);
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"fetch"));
        assert!(names.contains(&"read_skill"));
        assert!(names.contains(&"search_knowledge"));
        assert!(names.contains(&"list_knowledge_bases"));
        assert!(names.contains(&"delegate_sub_agent"));
        assert!(names.contains(&"run_parallel_sub_agents"));
    }
}
