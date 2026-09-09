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

use crate::tools::{Content, ToolDyn, ToolError};
use model_provider::ToolDefinition;

// ── Defaults ──────────────────────────────────────────────────────────────────

/// Default timeout for MCP tool calls (5 minutes).
pub const DEFAULT_MCP_TOOL_TIMEOUT: Duration = Duration::from_secs(300);

/// 嵌入文本资源进入 text 视图的字符上限。超限截断并标注总长，
/// 防止单个资源把 token 估算与压缩管线撑爆。
pub const MAX_EMBEDDED_TEXT_RESOURCE_CHARS: usize = 8 * 1024;

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
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Content, ToolError>> + Send + 'a>> {
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
            if let Some(error_text) = error_message_from_result(&result) {
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
            Ok(rmcp_content_to_output(&result.content))
        })
    }
}

// ── Content formatting helpers ────────────────────────────────────────────────

/// is_error 响应的错误文本；正常响应返回 `None`。
/// 纯函数以便对错误内容抽取做单元测试（`Peer` 无法脱离连接构造）。
fn error_message_from_result(result: &model::CallToolResult) -> Option<String> {
    if result.is_error != Some(true) {
        return None;
    }
    Some(
        result
            .content
            .iter()
            .filter_map(extract_text_from_content)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Extract human-readable text from any [`rmcp::model::RawContent`] variant.
fn extract_text_from_content(content: &rmcp::model::Content) -> Option<String> {
    use rmcp::model::RawContent;
    match &content.raw {
        RawContent::Text(t) => Some(t.text.clone()),
        RawContent::Resource(res) => match &res.resource {
            rmcp::model::ResourceContents::TextResourceContents {
                uri,
                text,
                mime_type,
                ..
            } => {
                let mime = mime_type.as_deref().unwrap_or("text/plain");
                Some(format!("{mime}:{uri}:{}", truncate_embedded_text(text)))
            }
            rmcp::model::ResourceContents::BlobResourceContents {
                uri,
                blob,
                mime_type,
                ..
            } => {
                // 二进制资源永不内联 base64 —— 完整内联会原样进入 token 估算
                // 与压缩管线，单个资源即 text bomb。保留 uri/mime 让模型仍可
                // 通过相应 MCP 工具按需请求该资源。
                let mime = mime_type.as_deref().unwrap_or("application/octet-stream");
                Some(format!(
                    "{mime}:{uri}:<binary resource omitted, {} base64 chars>",
                    blob.len()
                ))
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

/// 嵌入文本资源超 [`MAX_EMBEDDED_TEXT_RESOURCE_CHARS`] 时截断并标注总长。
fn truncate_embedded_text(text: &str) -> std::borrow::Cow<'_, str> {
    if text.chars().count() <= MAX_EMBEDDED_TEXT_RESOURCE_CHARS {
        return std::borrow::Cow::Borrowed(text);
    }
    let truncated: String = text
        .chars()
        .take(MAX_EMBEDDED_TEXT_RESOURCE_CHARS)
        .collect();
    std::borrow::Cow::Owned(format!(
        "{truncated}\n[truncated, {} chars total]",
        text.chars().count()
    ))
}

/// Convert MCP `tools/call` 响应内容为工具输出 [`Content`]。
///
/// - **Text** → text 部件
/// - **Image** → `data:{mime_type};base64,{data}` 图片部件（MCP 截图类服务器
///   零改动获得回图能力）
/// - **Resource / ResourceLink** → 描述性文本部件
/// - **Audio** → warning 后跳过
///
/// 全部为文本时折叠为 `Content::Text`（与既有纯文本输出形状逐字节一致），
/// 出现任意图片时保持 `Content::Parts`。
fn rmcp_content_to_output(contents: &[rmcp::model::Content]) -> Content {
    use model_provider::ContentPart;

    let mut parts: Vec<ContentPart> = Vec::with_capacity(contents.len());
    let mut has_image = false;
    for content in contents {
        match &content.raw {
            rmcp::model::RawContent::Text(t) => {
                parts.push(ContentPart::Text {
                    text: t.text.clone(),
                });
            }
            rmcp::model::RawContent::Image(img) => {
                has_image = true;
                parts.push(ContentPart::Image {
                    url: format!("data:{};base64,{}", img.mime_type, img.data),
                    detail: None,
                });
            }
            _ => match extract_text_from_content(content) {
                Some(text) => parts.push(ContentPart::Text { text }),
                None => warn!("MCP tool returned unsupported content, skipping"),
            },
        }
    }

    if parts.is_empty() {
        return Content::Text(String::new());
    }
    if !has_image {
        let text = parts
            .iter()
            .map(|p| match p {
                ContentPart::Text { text } => text.as_str(),
                _ => "",
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Content::Text(text);
    }
    Content::Parts(parts)
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

    // ── rmcp_content_to_output 映射 ──

    fn rmcp_text(text: &str) -> rmcp::model::Content {
        rmcp::model::Annotated::new(rmcp::model::RawContent::text(text), None)
    }

    fn rmcp_image(data: &str, mime_type: &str) -> rmcp::model::Content {
        rmcp::model::Annotated::new(rmcp::model::RawContent::image(data, mime_type), None)
    }

    #[test]
    fn rmcp_all_text_collapses_to_text_content() {
        // 全文本响应折叠为 Content::Text（\n 连接），与既有纯文本输出形状一致。
        let contents = vec![rmcp_text("第一行"), rmcp_text("第二行")];
        let out = rmcp_content_to_output(&contents);
        assert_eq!(out, Content::Text("第一行\n第二行".to_string()));
    }

    #[test]
    fn rmcp_image_maps_to_data_uri_part() {
        // Image{data, mime_type} → `data:{mime};base64,{data}` 图片部件。
        let out = rmcp_content_to_output(&[rmcp_image("QUJD", "image/png")]);
        assert_eq!(
            out,
            Content::Parts(vec![model_provider::ContentPart::Image {
                url: "data:image/png;base64,QUJD".to_string(),
                detail: None,
            }])
        );
    }

    #[test]
    fn rmcp_mixed_text_image_keeps_parts_shape() {
        // 文本 + 图片混排保持 Parts（不折叠为 Text）。
        let out =
            rmcp_content_to_output(&[rmcp_text("截图如下"), rmcp_image("QUJD", "image/jpeg")]);
        let Content::Parts(parts) = out else {
            panic!("expected Parts for mixed content, got {out:?}");
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[0],
            model_provider::ContentPart::Text {
                text: "截图如下".to_string()
            }
        );
        assert_eq!(
            parts[1],
            model_provider::ContentPart::Image {
                url: "data:image/jpeg;base64,QUJD".to_string(),
                detail: None,
            }
        );
    }

    #[test]
    fn rmcp_empty_content_yields_empty_text() {
        let out = rmcp_content_to_output(&[]);
        assert_eq!(out, Content::Text(String::new()));
    }

    // ── 资源 text 视图截断 ──

    fn rmcp_resource(res: rmcp::model::ResourceContents) -> rmcp::model::Content {
        rmcp::model::Annotated::new(rmcp::model::RawContent::resource(res), None)
    }

    #[test]
    fn blob_resource_never_inlines_base64() {
        // 二进制资源输出占位符 + base64 长度，blob 内容不得进入 text 视图。
        let content = rmcp_resource(
            rmcp::model::ResourceContents::blob("QUJDREVGRw==", "file:///tmp/report.pdf")
                .with_mime_type("application/pdf"),
        );
        let text = extract_text_from_content(&content).unwrap();
        assert_eq!(
            text,
            "application/pdf:file:///tmp/report.pdf:<binary resource omitted, 12 base64 chars>"
        );
        assert!(!text.contains("QUJDREVGRw=="));
    }

    #[test]
    fn text_resource_within_limit_is_kept() {
        let content = rmcp_resource(rmcp::model::ResourceContents::text(
            "短文本内容",
            "file:///notes.txt",
        ));
        let text = extract_text_from_content(&content).unwrap();
        assert!(text.contains("短文本内容"));
        assert!(!text.contains("[truncated"));
    }

    #[test]
    fn text_resource_over_limit_is_truncated() {
        // 1 万中文字符远超 8 KiB 上限，须截断并标注总长。
        let long_text = "字".repeat(10_000);
        let content = rmcp_resource(rmcp::model::ResourceContents::text(
            long_text,
            "file:///big.txt",
        ));
        let text = extract_text_from_content(&content).unwrap();
        assert!(text.contains("[truncated, 10000 chars total]"));
        assert!(text.chars().count() < MAX_EMBEDDED_TEXT_RESOURCE_CHARS + 100);
    }

    #[test]
    fn audio_content_is_skipped_in_output() {
        let audio = rmcp::model::Annotated::new(
            rmcp::model::RawContent::Audio(rmcp::model::RawAudioContent {
                data: "QUJD".to_string(),
                mime_type: "audio/wav".to_string(),
            }),
            None,
        );
        assert_eq!(extract_text_from_content(&audio), None);
        let out = rmcp_content_to_output(&[audio]);
        assert_eq!(out, Content::Text(String::new()));
    }

    // ── is_error 错误文本抽取 ──

    fn call_result(
        is_error: bool,
        content: Vec<rmcp::model::Content>,
    ) -> rmcp::model::CallToolResult {
        if is_error {
            rmcp::model::CallToolResult::error(content)
        } else {
            rmcp::model::CallToolResult::success(content)
        }
    }

    #[test]
    fn error_message_none_for_success() {
        let result = call_result(false, vec![rmcp_text("ok")]);
        assert_eq!(error_message_from_result(&result), None);
        let result = call_result(false, vec![]);
        assert_eq!(error_message_from_result(&result), None);
    }

    #[test]
    fn error_message_extracts_content_text() {
        let result = call_result(
            true,
            vec![rmcp_text("file not found"), rmcp_text("path: /a/b")],
        );
        assert_eq!(
            error_message_from_result(&result),
            Some("file not found\npath: /a/b".to_string())
        );
    }

    #[test]
    fn error_message_empty_content_yields_empty_string() {
        // 空内容交由调用方回退到固定文案，纯函数只负责如实抽取。
        let result = call_result(true, vec![]);
        assert_eq!(error_message_from_result(&result), Some(String::new()));
    }
}
