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

use super::{Content, ToolDyn, ToolError, ToolExecutor};

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
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        let result = (self.executor)(args);
        Box::pin(async move {
            result
                .map_err(|e| ToolError::ToolCallError(Box::new(StringError(e))))
                .map(Content::Text)
        })
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
/// 一条注册记录：工具实例 + 注册时物化的 [`ToolDefinition`]，避免每迭代重算 schema。
///
/// definition 是注册时刻的快照：定义变更必须经 `remove_tool` + `add_tool` 重新注册。
struct ToolEntry {
    tool: Arc<dyn ToolDyn>,
    definition: ToolDefinition,
}

impl ToolEntry {
    /// 物化条目 —— definition 在此一次性从工具取定。
    fn new(tool: Box<dyn ToolDyn>) -> Self {
        let definition = tool.definition();
        Self {
            tool: Arc::from(tool),
            definition,
        }
    }
}

pub struct DefaultToolsExecutor {
    tools: RwLock<HashMap<String, ToolEntry>>,
}

impl DefaultToolsExecutor {
    /// Create a new executor from a collection of [`ToolDyn`] instances.
    ///
    /// Tool names are derived from [`ToolDyn::name()`]. If two tools have the
    /// same name, the later one overwrites the earlier.
    pub fn new(tools: Vec<Box<dyn ToolDyn>>) -> Self {
        let map = tools
            .into_iter()
            .map(|t| (t.name(), ToolEntry::new(t)))
            .collect();
        Self {
            tools: RwLock::new(map),
        }
    }

    /// Add a single tool to the executor.
    ///
    /// If a tool with the same name already exists, it is replaced.
    pub fn add_tool(&self, tool: Box<dyn ToolDyn>) {
        let name = tool.name();
        self.tools
            .write()
            .unwrap()
            .insert(name, ToolEntry::new(tool));
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
    /// Returns `Ok(Content)` on success, or `Err(error_message)` on failure.
    /// The error message includes both lookup failures (tool not found) and
    /// execution errors from the tool itself.
    async fn execute(&self, name: &str, args: &str) -> Result<Content, String> {
        let args = args.to_string();
        let tool: Option<Arc<dyn ToolDyn>> =
            { self.tools.read().unwrap().get(name).map(|e| e.tool.clone()) };
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
            .map(|e| e.definition.clone())
            .collect()
    }

    fn add_tool(&self, tool: Box<dyn ToolDyn>) -> Result<(), String> {
        // 委托固有方法，避免两处插入逻辑漂移
        DefaultToolsExecutor::add_tool(self, tool);
        Ok(())
    }

    fn remove_tool(&self, name: &str) -> Result<(), String> {
        match self.tools.write().unwrap().remove(name) {
            Some(_) => Ok(()),
            None => Err(format!("Tool not found: {name}")),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

    /// `definition()` 每被调用一次计数 +1 —— 用于验证 executor 只在注册时取定快照。
    struct CountingTool {
        calls: Arc<AtomicUsize>,
    }

    impl ToolDyn for CountingTool {
        fn name(&self) -> String {
            "counting".to_string()
        }

        fn definition(&self) -> ToolDefinition {
            self.calls.fetch_add(1, Ordering::SeqCst);
            ToolDefinition {
                name: "counting".to_string(),
                description: "counting tool".to_string(),
                parameters: json!({"type": "object"}),
            }
        }

        fn call<'a>(
            &'a self,
            _args: String,
        ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
            Box::pin(async { Ok(Content::Text("ok".to_string())) })
        }
    }

    #[test]
    fn definitions_are_snapshotted_at_registration() {
        let calls = Arc::new(AtomicUsize::new(0));
        let executor = DefaultToolsExecutor::new(vec![Box::new(CountingTool {
            calls: calls.clone(),
        })]);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "new() 物化一次");

        // 热路径多次取定义，不触发重算
        let first = executor.definitions();
        let second = executor.definitions();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "definitions() 不重算 schema"
        );
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].name, "counting");
        assert_eq!(second[0].name, "counting");
    }

    #[test]
    fn re_register_refreshes_snapshot() {
        let calls = Arc::new(AtomicUsize::new(0));
        let executor = DefaultToolsExecutor::new(vec![]);

        executor.add_tool(Box::new(CountingTool {
            calls: calls.clone(),
        }));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "add_tool 物化一次");
        assert_eq!(executor.definitions().len(), 1);

        // remove + add 走完整重新注册路径，快照随之刷新
        executor.remove_tool("counting").unwrap();
        assert!(executor.definitions().is_empty());
        executor.add_tool(Box::new(CountingTool {
            calls: calls.clone(),
        }));
        assert_eq!(calls.load(Ordering::SeqCst), 2, "重新注册再物化一次");
        assert_eq!(executor.definitions().len(), 1);
    }
}
