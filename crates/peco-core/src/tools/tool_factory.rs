// ============================================================================
// DefaultToolsExecutor — owns tool instances and dispatches execution by name.
// ============================================================================
//
// Also provides:
// - [`StringError`] — minimal error wrapper for string-based tool errors
// - [`FnToolAdapter`] — adapts closure-based tools to the ToolDyn trait

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use model_provider::ToolDefinition;

use super::{ToolDyn, ToolError, ToolExecutor};

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
/// Tools can be added via [`add_tool`](DefaultToolsExecutor::add_tool) or
/// passed directly to [`new`](DefaultToolsExecutor::new).
///
/// # Examples
///
/// ```ignore
/// use peco_core::tools::{DefaultToolsExecutor, ToolExecutor};
///
/// let executor = DefaultToolsExecutor::new(vec![]);
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
