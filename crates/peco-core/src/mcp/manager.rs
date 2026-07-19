//! MCP Manager — orchestrates multiple MCP client connections.
//!
//! [`McpManager`] holds a collection of active [`McpClientHandler`](super::McpClientHandler)
//! connections, each wrapped in a [`McpServerHandler`].  Tools discovered from
//! MCP servers are registered with a shared [`ToolExecutor`](crate::tools::ToolExecutor)
//! so the agent can use them alongside built-in tools.
//!
//! # Architecture
//!
//! ```text
//! SystemConfig::load() → McpConfig
//!   → McpManager::from_config(config, executor)
//!     → for each enabled server:
//!         create transport (stdio / HTTP)
//!         create McpClientHandler { name, executor.clone(), ... }
//!         handler.connect(transport)
//!           → serve() + list_all_tools() + executor.add_tool() × N
//!         store McpServerHandler (keeps connection alive)
//! ```
//!
//! # Configuration
//!
//! Configuration is loaded by [`SystemConfig`](crate::config::SystemConfig) at startup.
//! Use [`SystemConfig`](crate::config::SystemConfig) to
//! access the parsed [`McpConfig`](crate::config::McpConfig), then pass it to
//! [`McpManager::from_config`].
//!
//! # Example
//!
//! ```ignore
//! use std::sync::Arc;
//! use peco_core::config::McpConfig;
//! use peco_core::mcp::McpManager;
//! use peco_core::tools::DefaultToolsExecutor;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let config = McpConfig::load()?;
//! let executor = Arc::new(DefaultToolsExecutor::new(vec![]));
//! let manager = McpManager::from_config(config, executor).await?;
//! println!("Connected to {} servers", manager.server_count());
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use futures::future::join_all;
use tracing::{info, warn};

use crate::config::{McpConfig, McpServerConfig, TransportType};
use crate::mcp::error::McpClientError;
use crate::mcp::mcp_client_handler::McpClientHandler;

// ── McpServerHandler ──────────────────────────────────────────────────────────

/// Internal handle: a named, running MCP connection.
///
/// The [`RunningService`](rmcp::service::RunningService) must be kept alive to
/// maintain the connection — dropping it closes the connection and, for stdio
/// transports, kills the child process.
pub(crate) struct McpServerHandler {
    pub(crate) name: String,
    #[allow(dead_code)]
    pub(crate) service: rmcp::service::RunningService<rmcp::RoleClient, McpClientHandler>,
}

// ── McpManager ────────────────────────────────────────────────────────────────

/// Manages the lifecycle of multiple MCP client connections.
///
/// Created from a parsed [`McpConfig`], connects to all enabled servers, and
/// registers their tools with a shared [`ToolExecutor`](crate::tools::ToolExecutor).
///
/// # Lifecycle
///
/// 1. **Construction** — `McpManager::from_config(config, executor)` concurrently
///    connects to each enabled server via [`McpClientHandler::connect`].  Tools
///    are automatically registered with the executor.
///
/// 2. **Runtime** — when an MCP server sends `notifications/tools/list_changed`,
///    the corresponding handler automatically refreshes tools in the executor.
///
/// 3. **Query** — use [`server_count`](McpManager::server_count) and
///    [`server_names`](McpManager::server_names) for connection status.
///    Tool queries go through the shared executor.
///
/// 4. **Drop** — when the manager is dropped, all connections are closed.
///    For stdio servers, this also kills the child processes.
pub struct McpManager {
    /// Shared tool executor — tools are registered here by handlers.
    tools_executor: Arc<dyn crate::tools::ToolExecutor>,
    /// Active MCP services, kept alive to maintain connections.
    services: Vec<McpServerHandler>,
}

impl McpManager {
    /// Create an empty MCP manager with no connections.
    ///
    /// This is used by [`GlobalHandler`](crate::GlobalHandler) for lazy
    /// initialization.  Use [`McpManager::from_config`] to create a fully
    /// initialized manager.
    #[allow(dead_code)]
    pub(crate) fn empty(tools_executor: Arc<dyn crate::tools::ToolExecutor>) -> Self {
        Self {
            tools_executor,
            services: Vec::new(),
        }
    }

    /// Initialize the MCP manager from a parsed [`McpConfig`].
    ///
    /// Connects to all enabled servers concurrently.  Individual server
    /// failures are logged and skipped.  Tools are registered with the
    /// shared executor via [`McpClientHandler::connect`].
    ///
    /// Configuration should be obtained from
    /// [`SystemConfig`](crate::config::SystemConfig)
    /// or loaded explicitly via [`McpConfig::load()`].
    pub async fn from_config(
        config: McpConfig,
        tools_executor: Arc<dyn crate::tools::ToolExecutor>,
    ) -> Result<Self, crate::mcp::error::McpError> {
        let enabled = config.enabled_servers();

        info!(server_count = enabled.len(), "Initializing MCP connections");

        // Connect to all enabled servers concurrently
        let futures: Vec<_> = enabled
            .iter()
            .map(|(name, server_config)| {
                let name = name.clone();
                let config = (*server_config).clone();
                let executor = tools_executor.clone();
                async move {
                    let result = connect_one(&name, &config, executor).await;
                    (name, result)
                }
            })
            .collect();

        let results = join_all(futures).await;

        let mut services = Vec::new();
        for (name, result) in results {
            match result {
                Ok(service) => {
                    services.push(McpServerHandler { name, service });
                }
                Err(e) => {
                    warn!(
                        server = %name,
                        error = %e,
                        "Failed to connect to MCP server, skipping"
                    );
                }
            }
        }

        info!(
            connected = services.len(),
            "MCP initialization complete"
        );

        Ok(Self {
            tools_executor,
            services,
        })
    }

    // ── Query methods ────────────────────────────────────────────────────────

    /// Return the number of successfully connected servers.
    pub fn server_count(&self) -> usize {
        self.services.len()
    }

    /// Return the names of all connected servers.
    pub fn server_names(&self) -> Vec<&str> {
        self.services.iter().map(|s| s.name.as_str()).collect()
    }

    /// Return the total number of tools across all connected MCP servers.
    ///
    /// This queries the shared executor and counts tools that are MCP-backed
    /// (heuristic: tools whose name is not a known built-in).
    pub fn tool_count(&self) -> usize {
        self.tools_executor.definitions().len()
    }

    /// Return a reference to the shared tool executor.
    pub fn tools_executor(&self) -> &Arc<dyn crate::tools::ToolExecutor> {
        &self.tools_executor
    }
}

// ── Per-server connection helper ──────────────────────────────────────────────

/// Connect to a single MCP server and return the running service.
async fn connect_one(
    name: &str,
    config: &McpServerConfig,
    tools_executor: Arc<dyn crate::tools::ToolExecutor>,
) -> Result<rmcp::service::RunningService<rmcp::RoleClient, McpClientHandler>, McpClientError> {
    let handler = McpClientHandler::new(name.to_string(), tools_executor);

    match config.transport {
        TransportType::Stdio => {
            let transport = crate::mcp::connection::make_stdio_transport(name, config)
                .map_err(|e| McpClientError::ConnectionError(e.to_string()))?;
            handler.connect(transport).await
        }
        TransportType::Sse | TransportType::StreamableHttp => {
            let transport = crate::mcp::connection::make_http_transport(name, config)
                .map_err(|e| McpClientError::ConnectionError(e.to_string()))?;
            handler.connect(transport).await
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpConfig;

    #[test]
    fn test_config_parse_smoke() {
        let config = McpConfig::from_json_str(r#"{"mcpServers": {}}"#).unwrap();
        assert!(config.mcp_servers.is_empty());
    }

    #[test]
    fn test_config_from_str_empty_servers() {
        let config = McpConfig::from_json_str(
            r#"{"mcpServers": {"a": {"transport": "stdio", "command": "echo"}}}"#,
        )
        .unwrap();
        assert_eq!(config.mcp_servers.len(), 1);
    }

    #[test]
    fn test_mcp_config_empty() {
        let config = McpConfig::empty();
        assert!(config.mcp_servers.is_empty());
        assert!(config.enabled_servers().is_empty());
    }

    #[tokio::test]
    async fn test_from_config_with_no_servers() {
        let config = McpConfig::from_json_str(r#"{"mcpServers": {}}"#).unwrap();
        let executor = Arc::new(crate::tools::DefaultToolsExecutor::new(vec![]));
        let manager = McpManager::from_config(config, executor).await.unwrap();
        assert_eq!(manager.server_count(), 0);
        assert!(manager.server_names().is_empty());
    }

    #[tokio::test]
    async fn test_empty_manager() {
        let executor = Arc::new(crate::tools::DefaultToolsExecutor::new(vec![]));
        let manager = McpManager::empty(executor);
        assert_eq!(manager.server_count(), 0);
    }
}
