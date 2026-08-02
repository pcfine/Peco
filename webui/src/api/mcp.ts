import api from "./client";
import type { McpConfigResponse } from "@/types/mcp";

export async function getMcpConfig(): Promise<McpConfigResponse> {
  const res = await api.get<McpConfigResponse>("/mcp");
  return res.data;
}

export async function saveMcpConfig(
  data: McpConfigResponse,
): Promise<{ success: boolean; message?: string }> {
  const res = await api.put<{ success: boolean; message?: string }>(
    "/mcp",
    data,
  );
  return res.data;
}

export async function testMcpConnection(
  name: string,
): Promise<{ success: boolean; message?: string }> {
  const res = await api.post<{ success: boolean; message?: string }>(
    `/mcp/${encodeURIComponent(name)}/test`,
  );
  return res.data;
}
