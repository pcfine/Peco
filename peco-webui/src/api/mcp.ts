import { api, spaceScope } from "@/lib/api";

/** `mcpconfig.json` 的原始投影：`{ mcpServers: { <name>: {...} } }`。 */
export interface McpConfig {
  mcpServers: Record<string, unknown>;
}

export async function getMcpConfig(wsId: string): Promise<McpConfig> {
  const res = await api.get<McpConfig>("/mcp", spaceScope(wsId));
  // 服务端缺文件时回 `{mcpServers:{}}`；空对象也不补别的键，前端只读不写。
  return { mcpServers: res.data?.mcpServers ?? {} };
}
