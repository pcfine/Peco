// ============================================================================
// ToolFactory — global registry of built-in tool constructors
// ============================================================================
//
// This module provides:
// - [`ToolFactory`] — singleton registry that creates tool instances by name
// - [`CreateToolFn`] — type alias for the factory function signature
// - [`make_tool_fn!`] — helper macro to generate a [`CreateToolFn`] for a tool struct

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use model_provider::ToolDefinition;

use super::{
    DelegateSubAgent, Fetch, ReadSkill, RunParallelSubAgents, ShellExec, ToolDyn, ToolError,
    ToolExecutor,
};
use crate::knowledge::tools::{
    AddToKnowledgeBase, GetKnowledgeBaseDocs, ListKnowledgeBases, SearchKnowledge,
    SyncKnowledgeBase,
};

// ── CreateToolFn ─────────────────────────────────────────────────────────────

/// A factory function that constructs a fresh [`ToolDyn`] instance.
type CreateToolFn = fn() -> Box<dyn ToolDyn>;

/// Generate a [`CreateToolFn`] function pointer for a tool struct.
macro_rules! make_tool_fn {
    ($type:ident) => {{
        fn f() -> Box<dyn ToolDyn> {
            Box::new($type)
        }
        f as fn() -> Box<dyn ToolDyn>
    }};
}

// ── ToolFactory ──────────────────────────────────────────────────────────────

/// Registry of all built-in tools.
///
/// Each tool is stored as a factory closure keyed by name.
/// Use [`ToolFactory::global()`] to access the singleton, then
/// [`get_tool`](ToolFactory::get_tool) to construct a tool instance.
///
/// # Adding a new tool
///
/// 1. Create `tools/<name>.rs` with a `#[peco_tool]`-annotated `pub async fn`.
/// 2. Add `mod <name>;` and `pub use <name>::<StructName>;` in the parent `mod.rs`.
/// 3. Insert the factory closure in [`ToolFactory::init`].
pub struct ToolFactory {
    tools: HashMap<String, CreateToolFn>,
}

impl ToolFactory {
    /// Initialize the registry with all built-in tools.
    ///
    /// This is called by [`GlobalHandler`](crate::GlobalHandler) and should not be
    /// called directly. Use [`ToolFactory::global()`] to access the global singleton.
    pub(crate) fn init() -> Self {
        let mut tools: HashMap<String, CreateToolFn> = HashMap::new();
        tools.insert("shell".into(), make_tool_fn!(ShellExec));
        tools.insert("fetch".into(), make_tool_fn!(Fetch));
        tools.insert("read_skill".into(), make_tool_fn!(ReadSkill));
        // 知识库工具
        tools.insert("search_knowledge".into(), make_tool_fn!(SearchKnowledge));
        tools.insert(
            "list_knowledge_bases".into(),
            make_tool_fn!(ListKnowledgeBases),
        );
        tools.insert(
            "sync_knowledge_base".into(),
            make_tool_fn!(SyncKnowledgeBase),
        );
        tools.insert(
            "add_to_knowledge_base".into(),
            make_tool_fn!(AddToKnowledgeBase),
        );
        tools.insert(
            "get_knowledge_base_docs".into(),
            make_tool_fn!(GetKnowledgeBaseDocs),
        );
        // 子 Agent 工具
        tools.insert("delegate_sub_agent".into(), make_tool_fn!(DelegateSubAgent));
        tools.insert(
            "run_parallel_sub_agents".into(),
            make_tool_fn!(RunParallelSubAgents),
        );
        Self { tools }
    }

    /// Returns a reference to the global [`ToolFactory`] singleton.
    ///
    /// Delegates to [`GlobalHandler`](crate::GlobalHandler). Initialization
    /// happens on the first call.
    pub fn global() -> &'static ToolFactory {
        crate::GlobalHandler::global().tool_factory()
    }

    /// Construct a new tool instance by name.
    ///
    /// Returns `None` if no tool with the given name is registered.
    /// The returned [`ToolDyn`] can be registered with an agent via
    /// `handle.add_tool(boxed_tool)` or passed to any API that accepts
    /// `impl ToolDyn`.
    pub fn get_tool(&self, name: &str) -> Option<Box<dyn ToolDyn>> {
        self.tools.get(name).map(|f| f())
    }

    /// Returns all registered tool names.
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    /// Returns the number of registered tools.
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// Returns `true` if a tool with the given name is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn get_tools(&self, tools: &[impl AsRef<str>]) -> Vec<Box<dyn ToolDyn>> {
        let mut result = Vec::new();
        for tool_name in tools {
            if let Some(tool) = self.get_tool(tool_name.as_ref()) {
                result.push(tool);
            } else {
                tracing::warn!(
                    tool = %tool_name.as_ref(),
                    "Tool not registered in ToolFactory, skipping"
                );
            }
        }
        result
    }

    pub fn make_tools_executor(&self, tools: &[impl AsRef<str>]) -> DefaultToolsExecutor {
        let tool_instances = self.get_tools(tools);
        DefaultToolsExecutor::new(tool_instances)
    }
}

// ── FnToolAdapter ──────────────────────────────────────────────────────────────

/// A [`ToolDyn`] adapter that wraps a closure-based tool executor.
///
/// This allows tools created via [`ToolExecutor::add_tool`] (which accepts a
/// closure) to be stored in a [`DefaultToolsExecutor`] alongside regular
/// [`ToolDyn`] instances.
#[allow(dead_code)]
struct FnToolAdapter {
    definition: ToolDefinition,
    executor: Box<dyn Fn(String) -> Result<String, String> + Send + Sync>,
}

impl ToolDyn for FnToolAdapter {
    fn name(&self) -> String {
        self.definition.name.clone()
    }

    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        let result = (self.executor)(args);
        Box::pin(
            async move { result.map_err(|e| ToolError::ToolCallError(Box::new(StringError(e)))) },
        )
    }
}

/// Minimal [`std::error::Error`] wrapper so string errors can be stored in
/// [`ToolError::ToolCallError`].
#[derive(Debug)]
pub struct StringError(pub String);

impl std::fmt::Display for StringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for StringError {}

// ── DefaultToolsExecutor ──────────────────────────────────────────────────────

/// A [`ToolExecutor`] that owns a set of [`ToolDyn`] instances and dispatches
/// execution by tool name.
///
/// Unlike [`ToolFactory`] which stores factory functions and creates fresh
/// instances on each lookup, `DefaultToolsExecutor` holds fully-constructed
/// tools and calls them directly.
///
/// # Examples
///
/// ```ignore
/// use peco_core::tools::{DefaultToolsExecutor, ToolExecutor};
///
/// // Build from ToolFactory
/// let executor = DefaultToolsExecutor::from_factory(ToolFactory::global());
///
/// // Execute a tool by name
/// let result = executor.execute("shell", r#"{"command": "echo hello"}"#).await?;
///
/// // Get tool definitions for the model
/// let defs = executor.definitions();
/// ```
pub struct DefaultToolsExecutor {
    tools: RwLock<HashMap<String, Arc<dyn ToolDyn>>>,
}

impl DefaultToolsExecutor {
    /// Create a new executor from a collection of [`ToolDyn`] instances.
    ///
    /// Tool names are derived from [`ToolDyn::name()`]. If two tools have the
    /// same name, the later one overwrites the earlier.
    pub fn new(tools: Vec<Box<dyn ToolDyn>>) -> Self {
        let map = tools
            .into_iter()
            .map(|t| (t.name(), Arc::from(t)))
            .collect();
        Self {
            tools: RwLock::new(map),
        }
    }

    /// Add a single tool to the executor.
    ///
    /// If a tool with the same name already exists, it is replaced.
    pub fn add_tool(&self, tool: Box<dyn ToolDyn>) {
        self.tools
            .write()
            .unwrap()
            .insert(tool.name(), Arc::from(tool));
    }

    pub fn remove_tool(&self, name: &str) -> Result<(), String> {
        match self.tools.write().unwrap().remove(name) {
            Some(_) => Ok(()),
            None => Err(format!("Tool not found: {name}")),
        }
    }

    /// Returns `true` if a tool with the given name is available.
    pub fn contains(&self, name: &str) -> bool {
        self.tools.read().unwrap().contains_key(name)
    }

    /// Returns the number of tools held by this executor.
    pub fn tool_count(&self) -> usize {
        self.tools.read().unwrap().len()
    }
}

#[async_trait]
impl ToolExecutor for DefaultToolsExecutor {
    /// Execute a tool by name with JSON-encoded arguments.
    ///
    /// Returns `Ok(result_string)` on success, or `Err(error_message)` on failure.
    /// The error message includes both lookup failures (tool not found) and
    /// execution errors from the tool itself.
    async fn execute(&self, name: &str, args: &str) -> Result<String, String> {
        let args = args.to_string();
        let tool: Option<Arc<dyn ToolDyn>> = { self.tools.read().unwrap().get(name).cloned() };
        match tool {
            Some(t) => t.call(args).await.map_err(|e| e.to_string()),
            None => Err(format!("Tool not found: {name}")),
        }
    }

    /// Collect [`ToolDefinition`] from all held tools for advertising to the model.
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .read()
            .unwrap()
            .values()
            .map(|t| t.definition())
            .collect()
    }

    fn add_tool(&self, tool: Box<dyn ToolDyn>) -> Result<(), String> {
        self.tools
            .write()
            .unwrap()
            .insert(tool.name(), Arc::from(tool));
        Ok(())
    }

    fn remove_tool(&self, name: &str) -> Result<(), String> {
        match self.tools.write().unwrap().remove(name) {
            Some(_) => Ok(()),
            None => Err(format!("Tool not found: {name}")),
        }
    }
}
