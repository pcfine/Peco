// ============================================================================
// MCP 工具 — list / save / delete / test MCP Servers
// ============================================================================

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use super::deps::McpAccess;
use super::{StringError, ToolDyn, ToolError};

// ── ListMcpServers ──────────────────────────────────────────────────────────

pub struct ListMcpServers {
    mcp_access: Arc<dyn McpAccess>,
}

impl ListMcpServers {
    pub fn new(mcp_access: Arc<dyn McpAccess>) -> Self {
        Self { mcp_access }
    }
}

impl ToolDyn for ListMcpServers {
    fn name(&self) -> String {
        "list_mcp_servers".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_mcp_servers".to_string(),
            description: "List all configured MCP servers with their transport type, \
                enabled status, and connection details (url or command)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let _ = args;
            let servers = self.mcp_access.list_mcp_servers();
            let json = serde_json::to_string_pretty(&servers)
                .map_err(|e| ToolError::ToolCallError(Box::new(StringError(e.to_string()))))?;
            Ok(json)
        })
    }
}

// ── SaveMcpServer ───────────────────────────────────────────────────────────

pub struct SaveMcpServer {
    mcp_access: Arc<dyn McpAccess>,
}

impl SaveMcpServer {
    pub fn new(mcp_access: Arc<dyn McpAccess>) -> Self {
        Self { mcp_access }
    }
}

impl ToolDyn for SaveMcpServer {
    fn name(&self) -> String {
        "save_mcp_server".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "save_mcp_server".to_string(),
            description: "Add or update an MCP server configuration. \
                Operates on a single server — other server configs are not affected.\n\
                \n\
                transport: \"stdio\" | \"sse\" | \"streamable_http\"\n\
                For stdio: provide command, args (optional), env (optional).\n\
                For sse/streamable_http: provide url, headers (optional).\n\
                \n\
                Changes take effect after reloading the agent."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Unique MCP server name."
                    },
                    "config": {
                        "type": "object",
                        "description": "Server configuration object.",
                        "properties": {
                            "transport": {
                                "type": "string",
                                "enum": ["stdio", "sse", "streamable_http"],
                                "description": "Transport method."
                            },
                            "command": {
                                "type": "string",
                                "description": "Executable command (required for stdio)."
                            },
                            "args": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Command arguments (stdio only)."
                            },
                            "env": {
                                "type": "object",
                                "description": "Environment variables (stdio only)."
                            },
                            "url": {
                                "type": "string",
                                "description": "Server URL (required for sse/streamable_http)."
                            },
                            "headers": {
                                "type": "object",
                                "description": "HTTP headers (sse/streamable_http only)."
                            },
                            "enabled": {
                                "type": "boolean",
                                "description": "Enable this server (default true)."
                            },
                            "timeoutSecs": {
                                "type": "integer",
                                "description": "Connection timeout in seconds (default 30)."
                            },
                            "maxRetries": {
                                "type": "integer",
                                "description": "Maximum retry attempts on connection failure (default 3)."
                            }
                        },
                        "required": ["transport"]
                    }
                },
                "required": ["name", "config"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct SaveMcpServerArgs {
                name: String,
                config: crate::config::McpServerConfig,
            }

            let parsed: SaveMcpServerArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "server name is required and cannot be empty".into(),
                ))));
            }

            // 传输特定的前置验证 — 在调用 trait 方法之前提供清晰的错误消息
            match parsed.config.transport {
                crate::config::TransportType::Stdio => {
                    if parsed
                        .config
                        .command
                        .as_ref()
                        .is_none_or(|c| c.trim().is_empty())
                    {
                        return Err(ToolError::ToolCallError(Box::new(StringError(
                            "stdio transport requires a non-empty 'command' field".into(),
                        ))));
                    }
                }
                crate::config::TransportType::Sse
                | crate::config::TransportType::StreamableHttp => {
                    if parsed
                        .config
                        .url
                        .as_ref()
                        .is_none_or(|u| u.trim().is_empty())
                    {
                        return Err(ToolError::ToolCallError(Box::new(StringError(
                            "sse/streamable_http transport requires a non-empty 'url' field".into(),
                        ))));
                    }
                }
            }

            self.mcp_access
                .add_mcp_server(name, parsed.config)
                .map_err(|e| {
                    ToolError::ToolCallError(Box::new(StringError(format!(
                        "failed to save MCP server '{name}': {e}"
                    ))))
                })?;

            Ok(format!(
                "MCP server '{name}' saved successfully. Reload the agent for changes to take effect."
            ))
        })
    }
}

// ── DeleteMcpServer ─────────────────────────────────────────────────────────

pub struct DeleteMcpServer {
    mcp_access: Arc<dyn McpAccess>,
}

impl DeleteMcpServer {
    pub fn new(mcp_access: Arc<dyn McpAccess>) -> Self {
        Self { mcp_access }
    }
}

impl ToolDyn for DeleteMcpServer {
    fn name(&self) -> String {
        "delete_mcp_server".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delete_mcp_server".to_string(),
            description: "Remove an MCP server from the configuration. \
                This is irreversible — the server config is permanently removed. \
                Requires explicit confirmation."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The MCP server name to remove."
                    },
                    "confirm": {
                        "type": "boolean",
                        "description": "Must be explicitly set to true to confirm deletion."
                    }
                },
                "required": ["name", "confirm"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct DeleteMcpServerArgs {
                name: String,
                confirm: bool,
            }

            let parsed: DeleteMcpServerArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            if !parsed.confirm {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "Deletion not confirmed. Set 'confirm' to true to proceed.".into(),
                ))));
            }

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "server name is required and cannot be empty".into(),
                ))));
            }

            self.mcp_access.remove_mcp_server(name).map_err(|e| {
                ToolError::ToolCallError(Box::new(StringError(format!(
                    "failed to delete MCP server '{name}': {e}"
                ))))
            })?;

            Ok(format!(
                "MCP server '{name}' deleted successfully. Reload the agent for changes to take effect."
            ))
        })
    }
}

// ── TestMcpConnection ─────────────────────────────────────────────────────────

pub struct TestMcpConnection {
    mcp_access: Arc<dyn McpAccess>,
}

impl TestMcpConnection {
    pub const TOOL_NAME: &str = "test_mcp_connection";

    pub fn new(mcp_access: Arc<dyn McpAccess>) -> Self {
        Self { mcp_access }
    }
}

impl ToolDyn for TestMcpConnection {
    fn name(&self) -> String {
        Self::TOOL_NAME.to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: Self::TOOL_NAME.to_string(),
            description: "Test connectivity to a configured MCP server. \
                Attempts to establish a connection, perform the MCP handshake, \
                and list available tools. \
                Use this to verify MCP server configurations before relying on them."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The MCP server name to test."
                    }
                },
                "required": ["name"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct TestMcpConnectionArgs {
                name: String,
            }

            let parsed: TestMcpConnectionArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "server name is required and cannot be empty".into(),
                ))));
            }

            // 获取配置
            let config = match self.mcp_access.get_mcp_server_config(name) {
                Some(c) => c,
                None => {
                    let result = crate::mcp::fail_result_config_not_found(
                        name,
                        format!("MCP server '{name}' not found in configuration"),
                    );
                    return serde_json::to_string_pretty(&result).map_err(|e| {
                        ToolError::ToolCallError(Box::new(StringError(e.to_string())))
                    });
                }
            };

            // 执行连接测试
            let result = crate::mcp::test_mcp_connection(name, &config).await;

            serde_json::to_string_pretty(&result)
                .map_err(|e| ToolError::ToolCallError(Box::new(StringError(e.to_string()))))
        })
    }
}
