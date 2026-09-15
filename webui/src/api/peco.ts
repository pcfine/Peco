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

// ── 记忆自动整理（consolidation）────────────────────────────────────────────

/** `GET/PUT /memory/consolidation/optin` 响应：用户级 opt-in 意愿。 */
export interface ConsolidationOptinResponse {
  enabled: boolean;
}

/** 单轮整理的统计（对应后端 `RunStats`）；字段可能为 null。 */
export interface ConsolidationRunStats {
  scanned: number | null;
  candidates: number | null;
  clustered_groups: number | null;
  merged: number | null;
  dedup_deleted: number | null;
  ttl_deleted: number | null;
  audit_purged: number | null;
  llm_calls: number | null;
  /** 机器判定步骤被跳过的原因（基建不可用时非空）。 */
  machine_steps_skipped: string | null;
}

/** `GET /memory/consolidation/state` 响应：最近一次整理的观测口。 */
export interface ConsolidationStateResponse {
  /** RFC 3339；从未整理过为 null。 */
  last_run_at: string | null;
  /** 最近一轮统计；从未整理过为 null。 */
  last_run_stats: ConsolidationRunStats | null;
}

/** 读取自动整理 opt-in 开关（无行 = false）。 */
export async function getConsolidationOptin(): Promise<ConsolidationOptinResponse> {
  const resp = await api.get<ConsolidationOptinResponse>(
    `${PATH}/memory/consolidation/optin`,
  );
  return resp.data;
}

/** 写入自动整理 opt-in 开关；服务器总开关未放开时同样可保存（存意愿不即时生效）。 */
export async function setConsolidationOptin(
  enabled: boolean,
): Promise<ConsolidationOptinResponse> {
  const resp = await api.put<ConsolidationOptinResponse>(
    `${PATH}/memory/consolidation/optin`,
    { enabled },
  );
  return resp.data;
}

/** 读取最近一次整理的时刻与统计。 */
export async function getConsolidationState(): Promise<ConsolidationStateResponse> {
  const resp = await api.get<ConsolidationStateResponse>(
    `${PATH}/memory/consolidation/state`,
  );
  return resp.data;
}
