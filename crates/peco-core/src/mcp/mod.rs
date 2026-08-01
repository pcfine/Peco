//! MCP (Model Context Protocol) management module.
//!
//! This module provides:
//!
//! - **Configuration** ([`McpConfig`], [`McpServerConfig`], [`TransportType`]) —
//!   JSON-based configuration for MCP servers (re-exported from [`config`](crate::config)).
//! - **Handler** ([`McpClientHandler`]) — per-server connection manager that
//!   implements [`rmcp::ClientHandler`] to automatically synchronize MCP tools
//!   with a shared [`ToolExecutor`](crate::tools::ToolExecutor).
//! - **Manager** ([`McpManager`]) — orchestrates loading config, creating handlers,
//!   and holding connections alive.
//! - **Tool adapter** ([`McpTool`]) — bridges MCP tools into the
//!   [`ToolDyn`](crate::tools::ToolDyn) system.
//!
//! # Architecture
//!
//! ```text
//! SystemConfig::load() → McpConfig (registry of all servers)
//!
//! Caller resolves server names → McpServerConfig from McpConfig:
//!   let servers: Vec<(String, McpServerConfig)> = names
//!       .iter()
//!       .filter_map(|n| mcp_config.get_server(n).map(|c| (n.clone(), c.clone())))
//!       .filter(|(_, c)| c.enabled)
//!       .collect();
//!
//! Per agent:
//!   McpManager::new(&servers, executor)
//!     → for each (name, config):
//!         create transport (stdio / HTTP)
//!         create McpClientHandler { name, executor.clone(), ... }
//!         handler.connect(transport)
//!           → serve() + list_all_tools() + executor.add_tool() × N
//!         store McpServerHandler (keeps connection alive)
//!
//! MCP Server pushes tools/list_changed
//!   → McpClientHandler::on_tool_list_changed()
//!     → executor.remove_tool() all managed
//!     → list_all_tools()
//!     → executor.add_tool() new tools
//! ```
//!
//! # Quick start
//!
//! ```no_run
//! use std::sync::Arc;
//! use peco_core::config::{McpConfig, McpServerConfig};
//! use peco_core::mcp::McpManager;
//! use peco_core::tools::DefaultToolsExecutor;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let config = McpConfig::load()?;
//! let executor = Arc::new(DefaultToolsExecutor::new(vec![]));
//! // Resolve server configs from the global McpConfig
//! let servers: Vec<(String, McpServerConfig)> = config
//!     .enabled_servers()
//!     .into_iter()
//!     .map(|(n, c)| (n, c.clone()))
//!     .collect();
//! // Create manager with pre-resolved configs
//! let manager = McpManager::new(&servers, executor).await;
//! println!("Connected to {} servers", manager.server_count());
//! # Ok(())
//! # }
//! ```

pub mod connection;
pub mod error;
pub mod mcp_client_handler;
pub mod mcp_config_store;
pub mod mcp_manager;
pub mod tool;

// Re-export config types for convenience
pub use crate::config::{McpConfig, McpServerConfig, TransportType, resolve_env_vars};
pub use error::{McpClientError, McpError};
pub use mcp_client_handler::McpClientHandler;
pub use mcp_config_store::McpConfigStore;
pub use mcp_manager::McpManager;
pub use tool::McpTool;
