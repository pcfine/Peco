//! Error types for the MCP module.

use std::path::PathBuf;

use crate::config::ConfigError;

/// Errors that can occur during MCP manager operations.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// The MCP configuration file does not exist at the given path.
    #[error("MCP config file not found: {0}")]
    ConfigNotFound(PathBuf),

    /// Failed to parse the MCP configuration JSON.
    #[error("failed to parse MCP config: {0}")]
    ConfigParse(#[from] serde_json::Error),

    /// Configuration validation failed (missing required fields, etc.).
    #[error("MCP config validation error: {0}")]
    Validation(String),

    /// Failed to connect to a specific MCP server.
    #[error("connection failed for MCP server '{server}': {source}")]
    Connection {
        /// Name of the server that failed to connect.
        server: String,
        /// Underlying error from the transport or protocol layer.
        source: anyhow::Error,
    },
}

// ── McpClientError ─────────────────────────────────────────────────────────────

/// Errors that can occur during [`McpClientHandler`](super::McpClientHandler) operations.
#[derive(Debug, thiserror::Error)]
pub enum McpClientError {
    /// Failed to establish the MCP connection or complete the handshake.
    #[error("MCP connection error: {0}")]
    ConnectionError(String),

    /// Failed to fetch the tool list from the MCP server.
    #[error("Failed to fetch MCP tool list: {0}")]
    ToolListError(String),

    /// Failed to register a tool with the tool executor.
    #[error("Failed to register MCP tool: {0}")]
    ToolRegistrationError(String),
}

// ── Conversions ──────────────────────────────────────────────────────────────

impl From<ConfigError> for McpError {
    fn from(err: ConfigError) -> Self {
        match err {
            ConfigError::ConfigFileNotFound(path) => McpError::ConfigNotFound(path),
            ConfigError::JsonParse(e) => McpError::ConfigParse(e),
            ConfigError::Validation(msg) => McpError::Validation(msg),
            ConfigError::Io(e) => McpError::ConfigParse(serde_json::Error::io(e)),
            other => McpError::Validation(other.to_string()),
        }
    }
}
