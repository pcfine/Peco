//! MCP tool adapter — bridges MCP protocol tools into the `ToolDyn` system.
//!
//! [`McpTool`] wraps an [`rmcp::model::Tool`] definition and a
//! [`Peer<RoleClient>`] handle, implementing [`ToolDyn`](crate::tools::ToolDyn)
//! so MCP-discovered tools can be stored alongside built-in tools and dispatched
//! through a [`ToolExecutor`](crate::tools::ToolExecutor).
//!
//! # Timeout protection
//!
//! MCP transports (especially Streamable HTTP) can drop in-flight responses,
//! causing the caller to hang indefinitely.  By default every tool call is
//! guarded with a 300 s timeout ([`DEFAULT_MCP_TOOL_TIMEOUT`]).  Use
//! [`McpTool::with_timeout`] to customize it per-tool, or pass `None` to
//! opt-out entirely (for long-running tools).

use std::pin::Pin;
use std::time::Duration;

use rmcp::model::CallToolRequestParams;
use rmcp::{Peer, RoleClient, model};
use tracing::warn;

use crate::tools::{ToolDyn, ToolError};
use model_provider::ToolDefinition;

// ── Defaults ──────────────────────────────────────────────────────────────────

/// Default timeout for MCP tool calls (5 minutes).
pub const DEFAULT_MCP_TOOL_TIMEOUT: Duration = Duration::from_secs(300);

// ── McpTool ───────────────────────────────────────────────────────────────────

/// A [`ToolDyn`] adapter for a single MCP tool.
///
/// Each instance holds:
/// - The MCP-level tool definition (name, description, JSON Schema)
/// - A cloned [`Peer<RoleClient>`] handle for invoking the tool via the
///   [`tools/call`](https://spec.modelcontextprotocol.io/specification/2025-06-18/server/tools/) MCP request
/// - An optional timeout guard
///
/// # Example
///
/// ```ignore
/// use peco_core::mcp::McpTool;
///
/// // Build from a connected McpConnection
/// let mcp_tool = McpTool::new(tool_definition, connection.peer.clone());
///
/// // Use like any other ToolDyn
/// let name = mcp_tool.name();
/// let def = mcp_tool.definition();
/// let result = mcp_tool.call(r#"{"key": "value"}"#.to_string()).await?;
/// ```
pub struct McpTool {
    /// The MCP tool definition (name, description, input schema).
    definition: rmcp::model::Tool,
    /// Cloned peer handle — used to send `tools/call` requests.
    peer: Peer<RoleClient>,
    /// Per-tool timeout. `None` means no timeout (opt-out).
    timeout: Option<Duration>,
}

impl McpTool {
    /// Create a new `McpTool` with the default 300 s timeout.
    pub fn new(definition: rmcp::model::Tool, peer: Peer<RoleClient>) -> Self {
        Self {
            definition,
            peer,
            timeout: Some(DEFAULT_MCP_TOOL_TIMEOUT),
        }
    }

    /// Override the timeout for this tool.
    ///
    /// Pass `None` to disable the timeout entirely (use with caution —
    /// only for tools where you know the server will always respond).
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Borrow the MCP tool name.
    pub fn tool_name(&self) -> &str {
        &self.definition.name
    }
}

// ── ToolDyn impl ──────────────────────────────────────────────────────────────

impl ToolDyn for McpTool {
    fn name(&self) -> String {
        self.definition.name.to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.definition.name.to_string(),
            description: self
                .definition
                .description
                .clone()
                .unwrap_or_default()
                .to_string(),
            parameters: self.definition.schema_as_json_value(),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            // 1. Parse JSON arguments into rmcp's JsonObject
            let arguments: model::JsonObject = if args.trim().is_empty() {
                serde_json::Map::new()
            } else {
                serde_json::from_str(&args).map_err(|e| {
                    ToolError::JsonError(serde_json::Error::io(std::io::Error::other(format!(
                        "MCP tool '{}' received invalid JSON arguments: {e}",
                        self.definition.name
                    ))))
                })?
            };

            // 2. Build the call_tool request
            let params =
                CallToolRequestParams::new(self.definition.name.clone()).with_arguments(arguments);

            // 3. Issue the RPC call (with optional timeout)
            let call_future = self.peer.call_tool(params);
            let call_result = match self.timeout {
                Some(timeout) => {
                    match tokio::time::timeout(timeout, call_future).await {
                        Ok(result) => result,
                        Err(_elapsed) => {
                            // Timeout elapsed — return an error that the model can see
                            return Err(ToolError::ToolCallError(Box::new(std::io::Error::other(
                                format!(
                                    "MCP tool '{}' timed out after {timeout:?}",
                                    self.definition.name
                                ),
                            ))));
                        }
                    }
                }
                None => call_future.await,
            };

            let result = call_result.map_err(|e| {
                ToolError::ToolCallError(Box::new(std::io::Error::other(format!(
                    "MCP tool '{}' call failed: {e}",
                    self.definition.name
                ))))
            })?;

            // 4. If the server flagged this as an error, surface it
            if result.is_error == Some(true) {
                // Still try to extract meaningful text from the content
                let error_text = result
                    .content
                    .iter()
                    .filter_map(extract_text_from_content)
                    .collect::<Vec<_>>()
                    .join("\n");
                return Err(ToolError::ToolCallError(Box::new(std::io::Error::other(
                    if error_text.is_empty() {
                        format!(
                            "MCP tool '{}' returned an error with no content",
                            self.definition.name
                        )
                    } else {
                        error_text
                    },
                ))));
            }

            // 5. Parse the response content
            let output: Vec<String> = result.content.iter().map(format_content).collect();

            Ok(output.join("\n"))
        })
    }
}

// ── Content formatting helpers ────────────────────────────────────────────────

/// Extract human-readable text from any [`rmcp::model::RawContent`] variant.
fn extract_text_from_content(content: &rmcp::model::Content) -> Option<String> {
    use rmcp::model::RawContent;
    match &content.raw {
        RawContent::Text(t) => Some(t.text.clone()),
        RawContent::Image(img) => Some(format!("data:{};base64,{}", img.mime_type, img.data)),
        RawContent::Resource(res) => match &res.resource {
            rmcp::model::ResourceContents::TextResourceContents {
                uri,
                text,
                mime_type,
                ..
            } => {
                let mime = mime_type.as_deref().unwrap_or("text/plain");
                Some(format!("{mime}:{uri}:{text}",))
            }
            rmcp::model::ResourceContents::BlobResourceContents {
                uri,
                blob,
                mime_type,
                ..
            } => {
                let mime = mime_type.as_deref().unwrap_or("application/octet-stream");
                Some(format!("{mime}:{uri}:{blob}",))
            }
            #[allow(unreachable_patterns)]
            _ => None,
        },
        RawContent::Audio(_) => None,
        RawContent::ResourceLink(link) => {
            Some(format!("resource_link:{} ({})", link.uri, link.name))
        }
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

/// Format a single [`rmcp::model::Content`] block into a string suitable for
/// returning as tool output.
///
/// - **Text** → raw text
/// - **Image** → `data:{mime_type};base64,{data}` data URL
/// - **Resource** → `{mime_type}:{uri}:{text_or_blob}`
/// - **Audio** → warning (not supported)
fn format_content(content: &rmcp::model::Content) -> String {
    // Delegate to extract_text_from_content; fallback is an empty string
    extract_text_from_content(content).unwrap_or_else(|| {
        warn!("MCP tool returned unsupported content, skipping");
        String::new()
    })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::Tool;

    /// Build a minimal [`Tool`] for testing.  Uses the raw JSON fallback
    /// since `Tool` has several required fields that aren't easy to construct
    /// by hand.
    fn make_test_tool(name: &str) -> Tool {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "description": "A test tool",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "x": { "type": "number" }
                }
            }
        }))
        .unwrap()
    }

    #[test]
    fn test_mcp_tool_name() {
        // We cannot construct McpTool without a real Peer handle,
        // but we can test the Tool → ToolDefinition conversion pattern.
        let tool = make_test_tool("test_tool");
        assert_eq!(tool.name, "test_tool");
    }

    #[test]
    fn test_mcp_tool_definition_conversion() {
        let tool = make_test_tool("adder");
        // The Definition conversion is tested indirectly:
        // McpTool::definition() calls schema_as_json_value() and description
        assert_eq!(tool.name, "adder");
        let desc: &str = tool.description.as_deref().unwrap_or("");
        assert_eq!(desc, "A test tool");

        let schema = tool.schema_as_json_value();
        assert!(schema.is_object());
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn test_default_timeout_value() {
        assert_eq!(DEFAULT_MCP_TOOL_TIMEOUT, Duration::from_secs(300));
    }
}
