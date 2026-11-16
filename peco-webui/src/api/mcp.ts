import { api, resourceScope, type ResourceScope } from "@/lib/api";
import type { SuccessResponse } from "@/types/common";
import type { McpTestResult } from "@/types/daemon";

/** `mcpconfig.json` 的原始投影：`{ mcpServers: { <name>: {...} } }`。 */
export interface McpConfig {
  mcpServers: Record<string, unknown>;
}

export async function getMcpConfig(scope: ResourceScope): Promise<McpConfig> {
  const res = await api.get<McpConfig>("/mcp", resourceScope(scope));
  // 服务端缺文件时回 `{mcpServers:{}}`；顶层其它键原样留着 —— PUT 是整份替换，
  // 在这里丢掉就等于保存时把它们删了。
  return { ...res.data, mcpServers: res.data?.mcpServers ?? {} };
}

/**
 * 整体替换：body 就是一份完整的 mcpconfig.json，没出现的 server 等于删掉。
 * `config` 收 `unknown` —— 前端只保证它是能解析的 JSON，字段校验归服务端，
 * 失败时 400 的原文原样展示给用户。
 */
export async function updateMcpConfig(
  scope: ResourceScope,
  config: unknown,
): Promise<SuccessResponse> {
  const res = await api.put<SuccessResponse>(
    "/mcp",
    config,
    resourceScope(scope),
  );
  return res.data;
}

/** 真连一次：会起子进程 / 打网络请求，所以只在用户点「试连」时调用。 */
export async function testMcpServer(
  scope: ResourceScope,
  name: string,
): Promise<McpTestResult> {
  const res = await api.post<McpTestResult>(
    `/mcp/${encodeURIComponent(name)}/test`,
    undefined,
    resourceScope(scope),
  );
  return res.data;
}
