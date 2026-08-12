// ============================================================================
// TestMcpConnection — 测试 MCP Server 连接的 Agent Tool
// ============================================================================

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use super::deps::McpAccess;
use super::{StringError, ToolDyn, ToolError};

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
