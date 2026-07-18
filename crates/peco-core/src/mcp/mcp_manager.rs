//! MCP Manager — per-agent MCP connection orchestration.
//!
//! [`McpManager`] holds a collection of active [`McpClientHandler`](super::McpClientHandler)
//! connections, each wrapped in a [`McpServerHandler`].  Unlike [`McpConfig`](crate::config::McpConfig)
//! which is the **global** registry of all known MCP server configurations,
//! `McpManager` is **per-agent**: each agent connects only to the MCP servers
//! it declares.
//!
//! # Architecture
//!
//! ```text
//! // Caller resolves server names → McpServerConfig from the global McpConfig:
//! let servers: Vec<(String, McpServerConfig)> = agent_mcp_names
//!     .iter()
//!     .filter_map(|name| mcp_config.get_server(name).map(|c| (name.clone(), c.clone())))
//!     .filter(|(_, c)| c.enabled)
//!     .collect();
//!
//! // McpManager only deals with resolved configs:
//! McpManager::new(&servers, executor)
//!   → for each (name, config):
//!       connect_one(name, config, executor.clone())
//!         → create transport → McpClientHandler::connect()
//!           → serve() + list_all_tools() + executor.add_tool() × N
//!       store McpServerHandler (keeps connection alive)
//! ```
//!
//! # Example
//!
//! ```ignore
//! use std::sync::Arc;
//! use peco_core::mcp::McpManager;
//! use peco_core::tools::DefaultToolsExecutor;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let executor = Arc::new(DefaultToolsExecutor::new(vec![]));
//! // servers: Vec<(String, McpServerConfig)> resolved by caller from McpConfig
//! let manager = McpManager::new(&[], executor).await?;
//! println!("Connected to {} servers", manager.server_count());
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use futures::future::join_all;
use tracing::{info, warn};

use crate::config::{McpServerConfig, TransportType};
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

/// Manages the lifecycle of MCP client connections for a single agent.
///
/// Created from pre-resolved `(name, McpServerConfig)` pairs.  The caller is
/// responsible for looking up server names in the global [`McpConfig`] and
/// filtering by enabled status before passing configs to `McpManager`.
///
/// # Lifecycle
///
/// 1. **Construction** — `McpManager::new(servers, executor)` concurrently
///    connects to each server.  Tools are automatically registered with the
///    executor via [`McpClientHandler::connect`].
///
/// 2. **Runtime** — when an MCP server sends `notifications/tools/list_changed`,
///    the corresponding handler automatically refreshes tools in the executor.
///
/// 3. **Query** — use [`server_count`](McpManager::server_count) and
///    [`server_names`](McpManager::server_names) for connection status.
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
    /// Create an MCP manager that connects to the specified servers.
    ///
    /// Each element in `servers` is a `(name, McpServerConfig)` pair — already
    /// resolved by the caller from the global [`McpConfig`] registry.
    ///
    /// Connections are established concurrently (all servers at once).
    /// Individual connection failures are logged and skipped — a single
    /// unreachable server does not prevent the manager from being created.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Resolve server configs from the global McpConfig first:
    /// let servers: Vec<(String, McpServerConfig)> = agent_mcp_names
    ///     .iter()
    ///     .filter_map(|name| {
    ///         mcp_config.get_server(name)
    ///             .filter(|c| c.enabled)
    ///             .map(|c| (name.clone(), c.clone()))
    ///     })
    ///     .collect();
    ///
    /// let manager = McpManager::new(&servers, executor).await?;
    /// ```
    pub async fn new(
        servers: &[(String, McpServerConfig)],
        tools_executor: Arc<dyn crate::tools::ToolExecutor>,
    ) -> Self {
        info!(
            requested = servers.len(),
            "Initializing MCP connections for agent"
        );

        let futures: Vec<_> = servers
            .iter()
            .map(|(name, config)| {
                let name = name.clone();
                let config = config.clone();
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

        info!(connected = services.len(), "MCP initialization complete");

        Self {
            tools_executor,
            services,
        }
    }

    /// Create an empty MCP manager with no connections.
    ///
    /// Useful for agents that don't declare any MCP servers, or for testing.
    pub fn empty(tools_executor: Arc<dyn crate::tools::ToolExecutor>) -> Self {
        Self {
            tools_executor,
            services: Vec::new(),
        }
    }

    /// Connect to a single MCP server and add it to this manager.
    ///
    /// The caller must have already resolved `name` against the global
    /// [`McpConfig`] to obtain `server_config`.
    ///
    /// # Errors
    ///
    /// Returns [`McpClientError`] if the connection fails.
    pub async fn add_server(
        &mut self,
        name: &str,
        server_config: &McpServerConfig,
    ) -> Result<(), McpClientError> {
        let service = connect_one(name, server_config, self.tools_executor.clone()).await?;
        self.services.push(McpServerHandler {
            name: name.to_string(),
            service,
        });

        info!(
            server = %name,
            total = self.services.len(),
            "Added MCP server to manager"
        );
        Ok(())
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

    #[tokio::test]
    async fn test_new_with_empty_servers() {
        let executor = Arc::new(crate::tools::DefaultToolsExecutor::new(vec![]));
        let manager = McpManager::new(&[], executor).await;
        assert_eq!(manager.server_count(), 0);
        assert!(manager.server_names().is_empty());
    }

    #[tokio::test]
    async fn test_empty_manager() {
        let executor = Arc::new(crate::tools::DefaultToolsExecutor::new(vec![]));
        let manager = McpManager::empty(executor);
        assert_eq!(manager.server_count(), 0);
        assert!(manager.server_names().is_empty());
    }
}
