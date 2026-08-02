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
