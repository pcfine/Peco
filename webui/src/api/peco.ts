// Peco 永续对话 API

import api from "./client";
import type { SessionSnapshotResponse } from "@/types/chat";

// axios baseURL is '/api', so paths here are relative to that.
const PATH = "/peco";

// Native fetch() doesn't use axios baseURL — use the full path.
const SSE_BASE = "/api/peco";

export async function getPecoSession(): Promise<SessionSnapshotResponse> {
  const resp = await api.get<SessionSnapshotResponse>(`${PATH}/session`);
  return resp.data;
}

export async function clearPecoSession(): Promise<{
  success: boolean;
  message?: string;
}> {
  const resp = await api.delete<{ success: boolean; message?: string }>(
    `${PATH}/session`,
  );
  return resp.data;
}

export function pecoStreamUrl(message: string): string {
  return `${SSE_BASE}/stream?message=${encodeURIComponent(message)}`;
}

/** 纯附着模式：无 message 参数，接上当前用户进行中的任务流。 */
export function pecoAttachUrl(): string {
  return `${SSE_BASE}/stream`;
}

/** 取消当前用户进行中的任务（无任务时服务端返回 404）。 */
export async function cancelPecoStream(): Promise<{
  success: boolean;
  message?: string;
}> {
  const resp = await api.post<{ success: boolean; message?: string }>(
    `${PATH}/stream/cancel`,
  );
  return resp.data;
}
