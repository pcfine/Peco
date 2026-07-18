//! MCP Client Handler — bridges MCP connections into the [`ToolExecutor`] system.
//!
//! [`McpClientHandler`] implements [`rmcp::ClientHandler`] to automatically
//! synchronize MCP server tools with a shared [`ToolExecutor`](crate::tools::ToolExecutor).
//! When an MCP server connects, the handler fetches the initial tool list and
//! registers each tool as an [`McpTool`](super::McpTool).  When the server sends
//! `notifications/tools/list_changed`, the handler re-fetches the tool list and
//! replaces the previously registered tools.
//!
//! # Architecture
//!
//! Each MCP server gets its own `McpClientHandler` instance.  The handler is
//! consumed by [`connect`](McpClientHandler::connect), which returns a
//! [`RunningService`](rmcp::service::RunningService) that keeps the connection
//! alive.  The [`ToolExecutor`](crate::tools::ToolExecutor) is shared across
//! all handlers (and the agent), so tool changes are reflected immediately.
//!
//! # Example
//!
//! ```ignore
//! use peco_core::mcp::McpClientHandler;
//! use peco_core::tools::{DefaultToolsExecutor, ToolExecutor};
//! use std::sync::Arc;
//!
//! let executor: Arc<dyn ToolExecutor> = Arc::new(DefaultToolsExecutor::new(vec![]));
//! let handler = McpClientHandler::new("my-server".into(), executor.clone());
//! let _service = handler.connect(transport).await?;
//! // Tools are now registered in executor — agent sees them immediately.
//! ```

use std::sync::Arc;
use std::time::Duration;

use rmcp::ServiceExt;
use tracing::{error, info, warn};

use crate::mcp::error::McpClientError;
use crate::mcp::tool::McpTool;
use crate::tools::ToolExecutor;

/// Default per-call timeout applied to MCP tools (5 minutes).
///
/// Default per-call timeout applied to MCP tools (5 minutes).
pub const DEFAULT_MCP_TOOL_TIMEOUT: Duration = Duration::from_secs(300);

/// An MCP client handler that automatically registers and refreshes MCP tools
/// in a shared [`ToolExecutor`](crate::tools::ToolExecutor).
///
/// Implements [`rmcp::ClientHandler`] to react to `notifications/tools/list_changed`.
///
/// # Lifecycle
///
/// 1. **Construction** — create with a server name and shared executor.
/// 2. **Connection** — [`connect`](McpClientHandler::connect) establishes the
///    MCP connection, fetches the initial tool list, and registers tools.
/// 3. **Runtime sync** — when the server pushes `tools/list_changed`, the handler
///    automatically removes old tools, re-fetches the list, and registers new ones.
/// 4. **Drop** — when the returned [`RunningService`] is dropped, the connection
///    closes.  Tool cleanup is the caller's responsibility.
pub struct McpClientHandler {
    /// MCP server name (for logging and identification).
    server_name: String,

    /// Shared tool executor — tools are registered/removed here.
    tools_executor: Arc<dyn ToolExecutor>,

    /// Per-call timeout applied to every MCP tool this handler registers.
    /// `None` means unbounded (opt-out).
    timeout: Option<Duration>,

    /// Tool names registered by this handler, tracked so they can be
    /// removed and replaced on `tools/list_changed` notifications.
    ///
    /// Uses `std::sync::Mutex` (not tokio) because:
    /// - The guard is `Send`, keeping the async future `Send`.
    /// - Lock is held only for brief, synchronous Vec operations.
    managed_tools: std::sync::Mutex<Vec<String>>,
}

impl McpClientHandler {
    /// Create a new handler for the given MCP server.
    ///
    /// `tools_executor` should be the same [`Arc`] shared with the agent,
    /// so that tool changes are reflected in agent requests immediately.
    pub fn new(server_name: String, tools_executor: Arc<dyn ToolExecutor>) -> Self {
        Self {
            server_name,
            tools_executor,
            timeout: Some(DEFAULT_MCP_TOOL_TIMEOUT),
            managed_tools: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Set (or clear) the per-call timeout applied to every MCP tool this
    /// handler registers.
    ///
    /// Pass a [`Duration`] to bound calls, or `None` to disable the timeout
    /// (use with caution — only for tools where the server will always respond).
    pub fn with_timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.timeout = timeout.into();
        self
    }

    /// Return the server name this handler was created with.
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Build an [`McpTool`] from an MCP tool definition and peer handle,
    /// applying this handler's configured timeout.
    fn build_tool(&self, tool: rmcp::model::Tool, peer: rmcp::Peer<rmcp::RoleClient>) -> McpTool {
        McpTool::new(tool, peer).with_timeout(self.timeout)
    }

    /// Connect to an MCP server, fetch the initial tool list, and register
    /// all tools with the shared [`ToolExecutor`].
    ///
    /// Consumes `self` (moved into the [`RunningService`]) and returns the
    /// service handle.  The caller **must** keep the returned service alive
    /// to maintain the connection — dropping it closes the connection and
    /// kills any child process (for stdio transport).
    ///
    /// # Errors
    ///
    /// Returns [`McpClientError`] if the connection fails, the tool list
    /// fetch fails, or any tool cannot be registered.
    pub async fn connect<T, E, A>(
        self,
        transport: T,
    ) -> Result<rmcp::service::RunningService<rmcp::RoleClient, Self>, McpClientError>
    where
        T: rmcp::transport::IntoTransport<rmcp::RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let server_name = self.server_name.clone();

        // 1. Establish the MCP connection — self is consumed here.
        let service = ServiceExt::serve(self, transport)
            .await
            .map_err(|e| McpClientError::ConnectionError(e.to_string()))?;

        // 2. Fetch the initial tool list from the MCP server.
        let tools = service
            .peer()
            .list_all_tools()
            .await
            .map_err(|e| McpClientError::ToolListError(e.to_string()))?;

        // 3. Register tools via the shared executor.
        {
            let handler = service.service();
            let mut managed = handler.managed_tools.lock().unwrap();

            for tool in tools {
                let tool_name = tool.name.to_string();
                let mcp_tool = handler.build_tool(tool, service.peer().clone());
                handler
                    .tools_executor
                    .add_tool(Box::new(mcp_tool))
                    .map_err(|e| McpClientError::ToolRegistrationError(e))?;
                managed.push(tool_name);
            }

            info!(
                server = %server_name,
                tool_count = managed.len(),
                "MCP server connected, tools registered"
            );
        }

        Ok(service)
    }
}

// ── rmcp::ClientHandler impl ───────────────────────────────────────────────────

impl rmcp::ClientHandler for McpClientHandler {
    fn get_info(&self) -> rmcp::model::ClientInfo {
        rmcp::model::ClientInfo::default()
    }

    /// Called by rmcp when the MCP server sends `notifications/tools/list_changed`.
    ///
    /// The handler:
    /// 1. Removes all previously registered tools from the executor.
    /// 2. Re-fetches the full tool list from the server.
    /// 3. Registers the updated tools.
    fn on_tool_list_changed(
        &self,
        context: rmcp::service::NotificationContext<rmcp::RoleClient>,
    ) -> impl std::future::Future<Output = ()> + rmcp::service::MaybeSendFuture + '_ {
        async move {
            // 1. Re-fetch the full tool list from the MCP server.
            let tools = match context.peer.list_all_tools().await {
                Ok(tools) => tools,
                Err(e) => {
                    error!(
                        server = %self.server_name,
                        error = %e,
                        "Failed to re-fetch MCP tool list on list_changed notification"
                    );
                    return;
                }
            };

            // 2. Remove old tools and register new ones.
            //    All operations are synchronous after the initial fetch, so we can
            //    hold the lock through the entire update.
            let mut managed = self.managed_tools.lock().unwrap();

            for name in managed.drain(..) {
                if let Err(e) = self.tools_executor.remove_tool(&name) {
                    warn!(
                        server = %self.server_name,
                        tool = %name,
                        error = %e,
                        "Failed to remove MCP tool during refresh"
                    );
                }
            }

            for tool in tools {
                let tool_name = tool.name.to_string();
                let mcp_tool = self.build_tool(tool, context.peer.clone());
                match self.tools_executor.add_tool(Box::new(mcp_tool)) {
                    Ok(()) => managed.push(tool_name),
                    Err(e) => {
                        error!(
                            server = %self.server_name,
                            tool = %tool_name,
                            error = %e,
                            "Failed to register MCP tool during refresh"
                        );
                    }
                }
            }

            info!(
                server = %self.server_name,
                tool_count = managed.len(),
                "MCP tool list refreshed successfully"
            );
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rmcp::model::*;
    use rmcp::service::RequestContext;
    use rmcp::{RoleServer, ServerHandler, ServiceExt};
    use tokio::sync::RwLock;

    use super::*;
    use crate::tools::DefaultToolsExecutor;

    /// An MCP server whose tool list can be swapped at runtime.
    #[derive(Clone)]
    struct DynamicToolServer {
        tools: Arc<RwLock<Vec<Tool>>>,
    }

    impl DynamicToolServer {
        fn new(tools: Vec<Tool>) -> Self {
            Self {
                tools: Arc::new(RwLock::new(tools)),
            }
        }

        async fn set_tools(&self, tools: Vec<Tool>) {
            *self.tools.write().await = tools;
        }
    }

    impl ServerHandler for DynamicToolServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
                .with_protocol_version(ProtocolVersion::LATEST)
                .with_server_info(Implementation::new("test-dynamic-server", "0.1.0"))
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            let tools = self.tools.read().await.clone();
            Ok(ListToolsResult::with_all_items(tools))
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResult, ErrorData> {
            Ok(CallToolResult::success(vec![Content::text(format!(
                "called {}",
                request.name
            ))]))
        }
    }

    fn make_tool(name: &str, description: &str) -> Tool {
        Tool::new(
            name.to_string(),
            description.to_string(),
            Arc::new(serde_json::Map::new()),
        )
    }

    #[tokio::test]
    async fn test_handler_registers_tools_on_connect() {
        let server = DynamicToolServer::new(vec![
            make_tool("tool_a", "First tool"),
            make_tool("tool_b", "Second tool"),
        ]);

        let executor = Arc::new(DefaultToolsExecutor::new(vec![]));

        let (client_to_server, server_from_client) = tokio::io::duplex(8192);
        let (server_to_client, client_from_server) = tokio::io::duplex(8192);

        let server_clone = server.clone();
        tokio::spawn(async move {
            let _service = server_clone
                .serve((server_from_client, server_to_client))
                .await
                .expect("server failed to start");
            _service.waiting().await.expect("server error");
        });

        let handler = McpClientHandler::new("test-server".into(), executor.clone());

        let _mcp_service = handler
            .connect((client_from_server, client_to_server))
            .await
            .expect("connect failed");

        let defs = executor.definitions();
        assert_eq!(defs.len(), 2);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"tool_a"));
        assert!(names.contains(&"tool_b"));
    }

    #[tokio::test]
    async fn test_handler_refreshes_on_tool_list_changed() {
        let initial_tools = vec![make_tool("alpha", "Alpha tool")];
        let server = DynamicToolServer::new(initial_tools);

        let executor = Arc::new(DefaultToolsExecutor::new(vec![]));

        let (client_to_server, server_from_client) = tokio::io::duplex(8192);
        let (server_to_client, client_from_server) = tokio::io::duplex(8192);

        let server_clone = server.clone();
        let server_handle = tokio::spawn(async move {
            server_clone
                .serve((server_from_client, server_to_client))
                .await
                .expect("server failed to start")
        });

        let handler = McpClientHandler::new("test-server".into(), executor.clone());

        let _mcp_service = handler
            .connect((client_from_server, client_to_server))
            .await
            .expect("connect failed");

        // Verify initial state
        let defs = executor.definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "alpha");

        // Update the server's tool list
        server
            .set_tools(vec![
                make_tool("beta", "Beta tool"),
                make_tool("gamma", "Gamma tool"),
            ])
            .await;

        // Send the notification from the server side
        let server_service = server_handle.await.unwrap();
        server_service
            .peer()
            .notify_tool_list_changed()
            .await
            .expect("failed to send notification");

        // Give the handler time to process the notification
        tokio::time::sleep(Duration::from_millis(200)).await;

        let defs = executor.definitions();
        assert_eq!(defs.len(), 2);

        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"beta"), "expected 'beta' in {names:?}");
        assert!(names.contains(&"gamma"), "expected 'gamma' in {names:?}");
        assert!(
            !names.contains(&"alpha"),
            "expected 'alpha' to be removed, found {names:?}"
        );
    }

    #[tokio::test]
    async fn test_handler_with_custom_timeout() {
        let handler =
            McpClientHandler::new("srv".into(), Arc::new(DefaultToolsExecutor::new(vec![])))
                .with_timeout(Duration::from_secs(60));
        assert_eq!(handler.timeout, Some(Duration::from_secs(60)));
    }

    #[tokio::test]
    async fn test_handler_timeout_none() {
        let handler =
            McpClientHandler::new("srv".into(), Arc::new(DefaultToolsExecutor::new(vec![])))
                .with_timeout(None);
        assert_eq!(handler.timeout, None);
    }
}
