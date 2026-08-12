//! MCP 连接测试 — 短暂连接生命周期：连接 → 握手 → 列工具 → 断开。
//!
//! 供 REST API（peco-server）和 Agent Tool（`test_mcp_connection`）共用。
//! 整个操作有超时保护，使用 `McpServerConfig.timeout_secs`（默认 30 秒）。

use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use crate::config::{McpServerConfig, TransportType};
use crate::mcp::error::McpClientError;
use crate::mcp::mcp_client_handler::McpClientHandler;
use crate::tools::{DefaultToolsExecutor, ToolExecutor};

// ── McpTestErrorType ──────────────────────────────────────────────────────────

/// MCP 连接测试的错误类别。
///
/// 使用 enum 而非 string，确保前后端类型一致，
/// Swagger/OpenAPI 自动生成 enum 文档，避免拼写错误。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpTestErrorType {
    /// Server 名称不在配置中（由调用方产生，本模块仅定义）。
    ConfigNotFound,
    /// 配置不合法（如 stdio 缺少 command、HTTP 缺少 url）。
    InvalidConfig,
    /// TCP 连接被拒绝。
    ConnectionRefused,
    /// 连接超时（含 DNS、TCP、TLS、MCP 握手）。
    ConnectionTimeout,
    /// MCP 协议握手失败。
    HandshakeFailed,
    /// 传输层错误（DNS 解析失败、TLS 证书错误、代理错误等）。
    TransportError,
    /// 工具列表获取失败。
    ToolListFailed,
}

// ── McpTestResult ─────────────────────────────────────────────────────────────

/// MCP 连接测试结果。
#[derive(Debug, Clone, Serialize)]
pub struct McpTestResult {
    /// 测试是否成功（连接建立 + 工具列表获取）。
    pub success: bool,
    /// 被测试的 MCP Server 名称。
    pub server: String,
    /// 传输类型。
    /// 当配置不存在时为 `None`（`error_type = ConfigNotFound`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportType>,
    /// 发现的工具数量（成功时有效）。
    pub tool_count: usize,
    /// 发现的工具名称列表（成功时有效）。
    pub tools: Vec<String>,
    /// 人类可读的结果描述。
    pub message: String,
    /// 从发起连接到获取工具列表的总耗时（毫秒）。
    /// 成功和失败时均填充，方便诊断网络问题。
    pub duration_ms: u64,
    /// 错误类别（失败时有效）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<McpTestErrorType>,
}

// ── 辅助函数 ──────────────────────────────────────────────────────────────────

/// 构建失败结果，自动计算耗时。
fn fail_result(
    name: &str,
    config: &McpServerConfig,
    start: Instant,
    error_type: McpTestErrorType,
    message: String,
) -> McpTestResult {
    McpTestResult {
        success: false,
        server: name.to_string(),
        transport: Some(config.transport.clone()),
        tool_count: 0,
        tools: Vec::new(),
        message,
        duration_ms: start.elapsed().as_millis() as u64,
        error_type: Some(error_type),
    }
}

/// 构建 ConfigNotFound 失败结果。
///
/// 与 [`fail_result`] 不同，此函数不需要 `McpServerConfig` 引用
///（因为配置本身不存在），因此 `transport` 为 `None`，`duration_ms` 为 0。
pub(crate) fn fail_result_config_not_found(name: &str, message: String) -> McpTestResult {
    McpTestResult {
        success: false,
        server: name.to_string(),
        transport: None,
        tool_count: 0,
        tools: Vec::new(),
        message,
        duration_ms: 0,
        error_type: Some(McpTestErrorType::ConfigNotFound),
    }
}

/// 判断连接错误是否为 "connection refused" 模式。
///
/// 仅匹配明确的 TCP 连接拒绝特征字符串，
/// 不使用宽松的 `"refused"` 单字匹配以避免误判协议层错误。
fn is_connection_refused(error_msg: &str) -> bool {
    let lower = error_msg.to_lowercase();
    lower.contains("connection refused") || lower.contains("econnrefused")
}

/// 将 `McpClientError` 分类为 `McpTestErrorType` 并构建失败结果。
///
/// 注：当前基于错误消息子串做分类（受限于 `McpClientError` 使用
/// `String` 而非结构化变体）。若未来 `McpClientError` 添加结构化变体，
/// 此函数可改为纯枚举判别式匹配。
fn classify_error(
    name: &str,
    config: &McpServerConfig,
    start: Instant,
    error: McpClientError,
) -> McpTestResult {
    match &error {
        McpClientError::ConnectionError(msg) => {
            let lower = msg.to_lowercase();
            if is_connection_refused(msg) {
                fail_result(
                    name,
                    config,
                    start,
                    McpTestErrorType::ConnectionRefused,
                    format!("Connection refused: {msg}"),
                )
            } else if lower.contains("timeout") || lower.contains("timed out") {
                fail_result(
                    name,
                    config,
                    start,
                    McpTestErrorType::ConnectionTimeout,
                    format!("Connection timed out: {msg}"),
                )
            } else if lower.contains("handshake") || lower.contains("protocol") {
                fail_result(
                    name,
                    config,
                    start,
                    McpTestErrorType::HandshakeFailed,
                    format!("MCP handshake failed: {msg}"),
                )
            } else {
                // DNS 解析失败、TLS 证书错误、代理错误等传输层问题
                fail_result(
                    name,
                    config,
                    start,
                    McpTestErrorType::TransportError,
                    format!("Transport error: {msg}"),
                )
            }
        }
        McpClientError::ToolListError(msg) => fail_result(
            name,
            config,
            start,
            McpTestErrorType::ToolListFailed,
            format!("Failed to fetch tool list: {msg}"),
        ),
        McpClientError::ToolRegistrationError(msg) => fail_result(
            name,
            config,
            start,
            McpTestErrorType::ToolListFailed,
            format!("Failed to register tools: {msg}"),
        ),
    }
}

/// 构建成功结果。
fn success_result(
    name: &str,
    config: &McpServerConfig,
    start: Instant,
    executor: &DefaultToolsExecutor,
) -> McpTestResult {
    let definitions = executor.definitions();
    let tool_count = definitions.len();
    let tools: Vec<String> = definitions.iter().map(|d| d.name.clone()).collect();

    McpTestResult {
        success: true,
        server: name.to_string(),
        transport: Some(config.transport.clone()),
        tool_count,
        tools,
        message: format!(
            "Successfully connected to MCP server '{}', found {} tool(s)",
            name, tool_count
        ),
        duration_ms: start.elapsed().as_millis() as u64,
        error_type: None,
    }
}

/// 验证配置：stdio 必须有 command，HTTP/SSE 必须有 url。
fn validate_config(config: &McpServerConfig) -> Option<String> {
    match config.transport {
        TransportType::Stdio => {
            if config.command.as_ref().is_none_or(|c| c.trim().is_empty()) {
                return Some("stdio transport requires a non-empty 'command' field".into());
            }
        }
        TransportType::Sse | TransportType::StreamableHttp => {
            if config.url.as_ref().is_none_or(|u| u.trim().is_empty()) {
                return Some(
                    "sse/streamable_http transport requires a non-empty 'url' field".into(),
                );
            }
        }
    }
    None
}

// ── 核心测试函数 ──────────────────────────────────────────────────────────────

/// 测试到单个 MCP Server 的连接。
///
/// 执行完整但短暂的连接生命周期：
/// 1. 验证配置有效性
/// 2. 根据 transport 类型创建传输层
/// 3. 建立 MCP 连接（含握手）
/// 4. 获取工具列表
/// 5. 关闭连接并返回结果
///
/// 整个操作有超时保护，使用 `config.timeout_secs`（默认 30 秒）。
/// 返回的 `McpTestResult.duration_ms` 记录实际耗时（毫秒），
/// 成功和失败时均填充，方便诊断网络延迟问题。
///
/// 注：连接成功后返回的 `RunningService` 在此函数结束时 drop。
/// 对于 stdio transport，drop 会 kill 子进程；
/// 对于 HTTP transport，drop 会断开网络连接。
pub async fn test_mcp_connection(name: &str, config: &McpServerConfig) -> McpTestResult {
    let start = Instant::now();

    // 1. 验证配置有效性
    if let Some(err_msg) = validate_config(config) {
        return fail_result(
            name,
            config,
            start,
            McpTestErrorType::InvalidConfig,
            err_msg,
        );
    }

    // 2. 创建临时 executor（仅用于接收工具注册，测试结束后丢弃）
    let executor = Arc::new(DefaultToolsExecutor::new(vec![]));

    // 3. 根据 transport 类型创建传输层，建立连接，收集工具列表
    //    分两个分支以避免 trait object（TokioChildProcess 和
    //    StreamableHttpClientTransport 是不同的具体类型）。
    let timeout = std::time::Duration::from_secs(config.timeout_secs);

    match config.transport {
        TransportType::Stdio => {
            let transport = match crate::mcp::connection::make_stdio_transport(name, config) {
                Ok(t) => t,
                Err(e) => {
                    return fail_result(
                        name,
                        config,
                        start,
                        McpTestErrorType::InvalidConfig,
                        format!("Failed to create stdio transport: {e}"),
                    );
                }
            };

            let handler = McpClientHandler::new(name.to_string(), executor.clone());

            match tokio::time::timeout(timeout, handler.connect(transport)).await {
                Err(_elapsed) => fail_result(
                    name,
                    config,
                    start,
                    McpTestErrorType::ConnectionTimeout,
                    format!("Connection timed out after {}s", config.timeout_secs),
                ),
                Ok(Err(e)) => classify_error(name, config, start, e),
                Ok(Ok(_svc)) => success_result(name, config, start, &executor),
            }
        }
        TransportType::Sse | TransportType::StreamableHttp => {
            let transport = match crate::mcp::connection::make_http_transport(name, config) {
                Ok(t) => t,
                Err(e) => {
                    return fail_result(
                        name,
                        config,
                        start,
                        McpTestErrorType::InvalidConfig,
                        format!("Failed to create HTTP transport: {e}"),
                    );
                }
            };

            let handler = McpClientHandler::new(name.to_string(), executor.clone());

            match tokio::time::timeout(timeout, handler.connect(transport)).await {
                Err(_elapsed) => fail_result(
                    name,
                    config,
                    start,
                    McpTestErrorType::ConnectionTimeout,
                    format!("Connection timed out after {}s", config.timeout_secs),
                ),
                Ok(Err(e)) => classify_error(name, config, start, e),
                Ok(Ok(_svc)) => success_result(name, config, start, &executor),
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_stdio_config() -> McpServerConfig {
        McpServerConfig {
            transport: TransportType::Stdio,
            enabled: true,
            command: Some("echo".into()),
            args: Vec::new(),
            env: std::collections::HashMap::new(),
            url: None,
            headers: std::collections::HashMap::new(),
            timeout_secs: 30,
            max_retries: 3,
        }
    }

    #[test]
    fn test_validate_config_stdio_missing_command() {
        let config = McpServerConfig {
            transport: TransportType::Stdio,
            command: None,
            ..make_stdio_config()
        };
        assert!(validate_config(&config).is_some());
    }

    #[test]
    fn test_validate_config_stdio_empty_command() {
        let config = McpServerConfig {
            transport: TransportType::Stdio,
            command: Some("  ".into()),
            ..make_stdio_config()
        };
        assert!(validate_config(&config).is_some());
    }

    #[test]
    fn test_validate_config_stdio_valid() {
        assert!(validate_config(&make_stdio_config()).is_none());
    }

    fn make_http_config() -> McpServerConfig {
        McpServerConfig {
            transport: TransportType::Sse,
            enabled: true,
            command: None,
            args: Vec::new(),
            env: std::collections::HashMap::new(),
            url: Some("http://localhost:8080".into()),
            headers: std::collections::HashMap::new(),
            timeout_secs: 30,
            max_retries: 3,
        }
    }

    #[test]
    fn test_validate_config_http_missing_url() {
        let config = McpServerConfig {
            transport: TransportType::Sse,
            url: None,
            ..make_http_config()
        };
        assert!(validate_config(&config).is_some());
    }

    #[test]
    fn test_validate_config_http_empty_url() {
        let config = McpServerConfig {
            transport: TransportType::StreamableHttp,
            url: Some("  ".into()),
            ..make_http_config()
        };
        assert!(validate_config(&config).is_some());
    }

    #[test]
    fn test_validate_config_http_valid() {
        assert!(validate_config(&make_http_config()).is_none());
    }

    #[test]
    fn test_is_connection_refused() {
        assert!(is_connection_refused("Connection refused (os error 111)"));
        assert!(is_connection_refused(
            "tcp connect error: Connection refused"
        ));
        assert!(is_connection_refused("econnrefused"));
        // 仅含 "refused" 但不含 "connection refused" — 不应匹配
        assert!(!is_connection_refused("request refused by server"));
        assert!(!is_connection_refused("timeout"));
    }

    #[test]
    fn test_mcp_test_error_type_serialization() {
        assert_eq!(
            serde_json::to_string(&McpTestErrorType::ConfigNotFound).unwrap(),
            r#""config_not_found""#
        );
        assert_eq!(
            serde_json::to_string(&McpTestErrorType::InvalidConfig).unwrap(),
            r#""invalid_config""#
        );
        assert_eq!(
            serde_json::to_string(&McpTestErrorType::ConnectionRefused).unwrap(),
            r#""connection_refused""#
        );
        assert_eq!(
            serde_json::to_string(&McpTestErrorType::ConnectionTimeout).unwrap(),
            r#""connection_timeout""#
        );
        assert_eq!(
            serde_json::to_string(&McpTestErrorType::HandshakeFailed).unwrap(),
            r#""handshake_failed""#
        );
        assert_eq!(
            serde_json::to_string(&McpTestErrorType::TransportError).unwrap(),
            r#""transport_error""#
        );
        assert_eq!(
            serde_json::to_string(&McpTestErrorType::ToolListFailed).unwrap(),
            r#""tool_list_failed""#
        );
    }

    #[test]
    fn test_fail_result_config_not_found() {
        let result = fail_result_config_not_found("ghost", "MCP server 'ghost' not found".into());
        assert!(!result.success);
        assert_eq!(result.server, "ghost");
        assert_eq!(result.error_type, Some(McpTestErrorType::ConfigNotFound));
        assert_eq!(result.transport, None);
        assert_eq!(result.duration_ms, 0);
        assert_eq!(result.tool_count, 0);
        assert!(result.tools.is_empty());
    }

    #[test]
    fn test_fail_result_duration() {
        let config = make_stdio_config();
        let start = Instant::now();
        let result = fail_result(
            "test",
            &config,
            start,
            McpTestErrorType::InvalidConfig,
            "bad".into(),
        );
        assert!(!result.success);
        assert_eq!(result.server, "test");
        assert_eq!(result.error_type, Some(McpTestErrorType::InvalidConfig));
        assert_eq!(result.transport, Some(TransportType::Stdio));
        assert_eq!(result.tool_count, 0);
        assert!(result.tools.is_empty());
        assert!(result.duration_ms < 1000);
    }

    #[tokio::test]
    async fn test_invalid_config_returns_error() {
        let config = McpServerConfig {
            transport: TransportType::Stdio,
            command: None,
            ..make_stdio_config()
        };
        let result = test_mcp_connection("bad-server", &config).await;
        assert!(!result.success);
        assert_eq!(result.error_type, Some(McpTestErrorType::InvalidConfig));
        assert!(result.duration_ms < 1000);
        assert!(result.message.contains("command"));
    }
}
