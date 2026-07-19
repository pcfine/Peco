//! MCP configuration types and JSON deserialization.
//!
//! Configuration is read from a JSON file following the format:
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "server-name": {
//!       "transport": "stdio",
//!       "command": "npx",
//!       "args": ["-y", "@scope/server"],
//!       "env": { "KEY": "value" },
//!       "enabled": true
//!     },
//!     "remote-server": {
//!       "transport": "streamable_http",
//!       "url": "http://localhost:8000/mcp",
//!       "headers": { "Authorization": "Bearer ${TOKEN}" },
//!       "timeoutSecs": 30,
//!       "maxRetries": 3,
//!       "enabled": true
//!     }
//!   }
//! }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::error::ConfigError;

// ── Defaults ──────────────────────────────────────────────────────────────────

fn default_enabled() -> bool {
    true
}
fn default_timeout() -> u64 {
    30
}
fn default_max_retries() -> u32 {
    3
}

// ── Transport type ────────────────────────────────────────────────────────────

/// Transport method for connecting to an MCP server.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TransportType {
    /// Standard input/output — spawn a local child process.
    Stdio,
    /// Server-Sent Events — remote streaming via HTTP SSE.
    Sse,
    /// Streamable HTTP — recommended remote transport (bi-directional).
    StreamableHttp,
}

// ── Server config ─────────────────────────────────────────────────────────────

/// Configuration for a single MCP server.
///
/// The server name is NOT stored in this struct — it comes from the
/// key in the `mcpServers` map of the config file.
///
/// ## Transport-specific fields
///
/// | Field(s) | Required by | Purpose |
/// |---|---|---|
/// | `command`, `args`, `env` | `stdio` | Child process invocation |
/// | `url`, `headers` | `sse`, `streamable_http` | Remote server connection |
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    /// Transport method for this server.
    pub transport: TransportType,

    /// Whether this server should be connected on startup.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    // ── stdio fields ──────────────────────────────────────────────────────
    /// Executable command (required for `stdio` transport).
    #[serde(default)]
    pub command: Option<String>,

    /// Arguments passed to the command.
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables injected into the child process.
    /// Values may reference existing env vars with `${VAR_NAME}` syntax.
    #[serde(default)]
    pub env: HashMap<String, String>,

    // ── HTTP/SSE fields ───────────────────────────────────────────────────
    /// Server URL (required for `sse` and `streamable_http` transports).
    #[serde(default)]
    pub url: Option<String>,

    /// Custom HTTP headers sent with every request.
    /// Values may reference existing env vars with `${VAR_NAME}` syntax.
    #[serde(default)]
    pub headers: HashMap<String, String>,

    // ── General ───────────────────────────────────────────────────────────
    /// Connection timeout in seconds.
    #[serde(default = "default_timeout", rename = "timeoutSecs")]
    pub timeout_secs: u64,

    /// Maximum retry attempts on connection failure.
    #[serde(default = "default_max_retries", rename = "maxRetries")]
    pub max_retries: u32,
}

// ── Root config ───────────────────────────────────────────────────────────────

/// Root structure of the MCP configuration JSON file.
///
/// Maps server names (the keys in `mcpServers`) to their configurations.
#[derive(Debug, Clone, Deserialize)]
pub struct McpConfig {
    /// Named MCP server configurations.
    #[serde(rename = "mcpServers")]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

impl McpConfig {
    /// Load MCP configuration from the default config file.
    ///
    /// Resolves the config path via:
    /// 1. `PECO_MCP_CONFIG` environment variable
    /// 2. `./mcpconfig.json` (default)
    ///
    /// Returns an empty config (no servers) if the file does not exist.
    /// Returns [`ConfigError`] on parse or validation failure.
    ///
    /// This is the primary entry point for loading MCP configuration.
    /// It is called by [`SystemConfig::load`](super::SystemConfig::load).
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::default_config_path();
        match Self::from_file(&path) {
            Ok(config) => Ok(config),
            Err(ConfigError::ConfigFileNotFound(_)) => {
                tracing::debug!(
                    "MCP config file not found at {}, using empty config",
                    path.display()
                );
                Ok(McpConfig::empty())
            }
            Err(e) => Err(e),
        }
    }

    /// Create an empty MCP configuration with no servers.
    pub fn empty() -> Self {
        McpConfig {
            mcp_servers: HashMap::new(),
        }
    }

    /// Load MCP configuration from a JSON file on disk.
    ///
    /// Returns `ConfigError::ConfigFileNotFound` if the file does not exist.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Err(ConfigError::ConfigFileNotFound(path.to_path_buf()));
        }
        let content = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::JsonParse(serde_json::Error::io(std::io::Error::other(e))))?;
        Self::from_json_str(&content)
    }

    /// Parse MCP configuration from a JSON string.
    pub fn from_json_str(json: &str) -> Result<Self, ConfigError> {
        let config: McpConfig = serde_json::from_str(json)?;
        config.validate()?;
        Ok(config)
    }

    /// Resolve the default MCP config file path.
    ///
    /// Returns `PECO_MCP_CONFIG` if set, otherwise `./mcpconfig.json`.
    fn default_config_path() -> PathBuf {
        std::env::var("PECO_MCP_CONFIG")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("./mcpconfig.json"))
    }

    /// Validate the configuration for all servers.
    ///
    /// Ensures each server has the required fields for its transport type.
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (name, server) in &self.mcp_servers {
            match server.transport {
                TransportType::Stdio => {
                    if server.command.is_none() {
                        return Err(ConfigError::Validation(format!(
                            "MCP server '{name}' uses stdio transport but missing 'command' field"
                        )));
                    }
                }
                TransportType::Sse | TransportType::StreamableHttp => {
                    if server.url.is_none() {
                        return Err(ConfigError::Validation(format!(
                            "MCP server '{name}' uses {:?} transport but missing 'url' field",
                            server.transport
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Look up a single server's configuration by name.
    ///
    /// Returns `None` if the server is not configured.
    pub fn get_server(&self, name: &str) -> Option<&McpServerConfig> {
        self.mcp_servers.get(name)
    }

    /// Return all enabled servers as `(name, config)` pairs.
    pub fn enabled_servers(&self) -> Vec<(String, &McpServerConfig)> {
        self.mcp_servers
            .iter()
            .filter(|(_, s)| s.enabled)
            .map(|(n, s)| (n.clone(), s))
            .collect()
    }
}

// ── Env var resolution ────────────────────────────────────────────────────────

/// Resolve `${VAR_NAME}` references in a string against the process environment.
///
/// # Examples
///
/// ```
/// use peco_core::config::resolve_env_vars;
///
/// unsafe { std::env::set_var("MY_VAR", "hello") };
/// assert_eq!(resolve_env_vars("prefix-${MY_VAR}-suffix"), "prefix-hello-suffix");
/// assert_eq!(resolve_env_vars("no vars"), "no vars");
/// ```
pub fn resolve_env_vars(value: &str) -> String {
    let mut result = value.to_string();
    let mut start = 0;
    while let Some(pos) = result[start..].find("${") {
        let abs_pos = start + pos;
        if let Some(end) = result[abs_pos..].find('}') {
            let var_name = &result[abs_pos + 2..abs_pos + end];
            if let Ok(val) = std::env::var(var_name) {
                result.replace_range(abs_pos..=abs_pos + end, &val);
                start = abs_pos + val.len();
            } else {
                start = abs_pos + end + 1;
            }
        } else {
            break;
        }
    }
    result
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Env var resolution ───────────────────────────────────────────────

    #[test]
    fn resolve_simple_var() {
        unsafe { std::env::set_var("MCP_TEST_VAR", "resolved_value") };
        assert_eq!(
            resolve_env_vars("prefix_${MCP_TEST_VAR}_suffix"),
            "prefix_resolved_value_suffix"
        );
    }

    #[test]
    fn resolve_missing_var_unchanged() {
        assert_eq!(
            resolve_env_vars("${MCP_NONEXISTENT_VAR_XYZ}"),
            "${MCP_NONEXISTENT_VAR_XYZ}"
        );
    }

    #[test]
    fn resolve_no_vars() {
        assert_eq!(resolve_env_vars("plain string"), "plain string");
    }

    #[test]
    fn resolve_multiple_vars() {
        unsafe {
            std::env::set_var("MCP_A", "alpha");
            std::env::set_var("MCP_B", "beta");
        };
        assert_eq!(resolve_env_vars("${MCP_A}-${MCP_B}"), "alpha-beta");
    }

    // ── Config parsing ───────────────────────────────────────────────────

    #[test]
    fn parse_minimal_stdio_config() {
        let json = r#"{
            "mcpServers": {
                "test-srv": {
                    "transport": "stdio",
                    "command": "node"
                }
            }
        }"#;
        let config = McpConfig::from_json_str(json).unwrap();
        assert_eq!(config.mcp_servers.len(), 1);
        let srv = &config.mcp_servers["test-srv"];
        assert_eq!(srv.transport, TransportType::Stdio);
        assert_eq!(srv.command.as_deref(), Some("node"));
        assert!(srv.enabled); // default
        assert_eq!(srv.timeout_secs, 30);
        assert_eq!(srv.max_retries, 3);
    }

    #[test]
    fn parse_http_config_with_headers() {
        let json = r#"{
            "mcpServers": {
                "remote": {
                    "transport": "streamable_http",
                    "url": "http://localhost:8000/mcp",
                    "headers": {
                        "X-API-Key": "secret123",
                        "Authorization": "Bearer token"
                    },
                    "timeoutSecs": 60,
                    "maxRetries": 5
                }
            }
        }"#;
        let config = McpConfig::from_json_str(json).unwrap();
        let srv = &config.mcp_servers["remote"];
        assert_eq!(srv.transport, TransportType::StreamableHttp);
        assert_eq!(srv.url.as_deref(), Some("http://localhost:8000/mcp"));
        assert_eq!(srv.headers["X-API-Key"], "secret123");
        assert_eq!(srv.timeout_secs, 60);
        assert_eq!(srv.max_retries, 5);
    }

    #[test]
    fn parse_sse_config() {
        let json = r#"{
            "mcpServers": {
                "sse-srv": {
                    "transport": "sse",
                    "url": "http://localhost:18080/sse",
                    "enabled": false
                }
            }
        }"#;
        let config = McpConfig::from_json_str(json).unwrap();
        let srv = &config.mcp_servers["sse-srv"];
        assert_eq!(srv.transport, TransportType::Sse);
        assert!(!srv.enabled);
    }

    #[test]
    fn parse_disabled_server() {
        let json = r#"{
            "mcpServers": {
                "off": {
                    "transport": "stdio",
                    "command": "echo",
                    "enabled": false
                }
            }
        }"#;
        let config = McpConfig::from_json_str(json).unwrap();
        assert_eq!(config.enabled_servers().len(), 0);
    }

    #[test]
    fn parse_mixed_servers() {
        let json = r#"{
            "mcpServers": {
                "local": {
                    "transport": "stdio",
                    "command": "node",
                    "args": ["./server.js"],
                    "env": { "NODE_ENV": "production" }
                },
                "remote": {
                    "transport": "streamable_http",
                    "url": "http://example.com/mcp",
                    "headers": { "Authorization": "Bearer ${TOKEN}" }
                }
            }
        }"#;
        let config = McpConfig::from_json_str(json).unwrap();
        let enabled = config.enabled_servers();
        assert_eq!(enabled.len(), 2);
    }

    #[test]
    fn parse_empty_servers() {
        let json = r#"{"mcpServers": {}}"#;
        let config = McpConfig::from_json_str(json).unwrap();
        assert!(config.mcp_servers.is_empty());
        assert!(config.enabled_servers().is_empty());
    }

    // ── Validation ───────────────────────────────────────────────────────

    #[test]
    fn validate_stdio_missing_command() {
        let json = r#"{
            "mcpServers": {
                "bad": { "transport": "stdio" }
            }
        }"#;
        let result = McpConfig::from_json_str(json);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing 'command'"));
    }

    #[test]
    fn validate_http_missing_url() {
        let json = r#"{
            "mcpServers": {
                "bad": { "transport": "streamable_http" }
            }
        }"#;
        let result = McpConfig::from_json_str(json);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing 'url'"));
    }

    #[test]
    fn validate_sse_missing_url() {
        let json = r#"{
            "mcpServers": {
                "bad": { "transport": "sse" }
            }
        }"#;
        let result = McpConfig::from_json_str(json);
        assert!(result.is_err());
    }

    // ── TransportType serde ──────────────────────────────────────────────

    #[test]
    fn transport_type_snake_case() {
        let json = r#"{"transport": "streamable_http"}"#;
        #[derive(Deserialize)]
        struct Wrapper {
            transport: TransportType,
        }
        let w: Wrapper = serde_json::from_str(json).unwrap();
        assert_eq!(w.transport, TransportType::StreamableHttp);
    }

    // ── get_server ─────────────────────────────────────────────────────────

    #[test]
    fn get_server_found() {
        let config = McpConfig::from_json_str(
            r#"{"mcpServers": {"srv": {"transport": "stdio", "command": "echo"}}}"#,
        )
        .unwrap();
        let srv = config.get_server("srv");
        assert!(srv.is_some());
        assert_eq!(srv.unwrap().command.as_deref(), Some("echo"));
    }

    #[test]
    fn get_server_not_found() {
        let config = McpConfig::empty();
        assert!(config.get_server("nonexistent").is_none());
    }

    #[test]
    fn mcp_config_empty_servers() {
        let config = McpConfig::empty();
        assert!(config.mcp_servers.is_empty());
        assert!(config.enabled_servers().is_empty());
    }
}
