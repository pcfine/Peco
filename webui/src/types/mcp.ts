// MCP server configuration types — aligned with peco-server /api/mcp/*
// JSON field names match the Rust McpServerConfig serde serialization:
//   timeout_secs → timeoutSecs, max_retries → maxRetries (explicit #[serde(rename)])
//   all other fields use snake_case, TransportType uses snake_case

export type TransportType = "stdio" | "sse" | "streamable_http";

export interface McpServerConfig {
  transport: TransportType;
  enabled?: boolean;
  // stdio fields
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  // sse / streamable_http fields
  url?: string;
  headers?: Record<string, string>;
  // general
  timeoutSecs?: number;
  maxRetries?: number;
}

export interface McpConfigResponse {
  mcpServers: Record<string, McpServerConfig>;
}

/** MCP 连接测试错误类别 — 与 Rust 端 McpTestErrorType 保持一致 */
export type McpTestErrorType =
  | "config_not_found"
  | "invalid_config"
  | "connection_refused"
  | "connection_timeout"
  | "handshake_failed"
  | "transport_error"
  | "tool_list_failed";

export interface McpTestResult {
  success: boolean;
  server: string;
  /** 传输类型。ConfigNotFound 时为 undefined */
  transport?: TransportType;
  tool_count: number;
  tools: string[];
  message: string;
  /** 测试耗时（毫秒），成功和失败时均返回 */
  duration_ms: number;
  error_type?: McpTestErrorType;
}
