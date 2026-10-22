// Peco 永续对话 API

import api from "./client";
import type {
  SessionMetricsResponse,
  SessionSnapshotResponse,
} from "@/types/chat";
import type { ArtifactPage } from "@/types/artifact";

// axios baseURL is '/api', so paths here are relative to that.
const PATH = "/peco";

// Native fetch() doesn't use axios baseURL — use the full path.
const SSE_BASE = "/api/peco";

export async function getPecoSession(params?: {
  turns?: number;
  before?: number;
}): Promise<SessionSnapshotResponse> {
  const resp = await api.get<SessionSnapshotResponse>(`${PATH}/session`, {
    params,
  });
  return resp.data;
}

/** 上下文指标轻量端点：只回 `context_metrics`，不拖全量 turn 历史。 */
export async function getPecoSessionMetrics(): Promise<SessionMetricsResponse> {
  const resp = await api.get<SessionMetricsResponse>(`${PATH}/session/metrics`);
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

/**
 * 会话产物分页列表（服务端按 `created_at DESC` 返回）。
 *
 * 路径相对 axios 实例的 `baseURL`（`/api`），不得再写一遍 `/api`。
 */
export async function listPecoArtifacts(
  params: { limit?: number; offset?: number } = {},
): Promise<ArtifactPage> {
  const resp = await api.get<ArtifactPage>(`${PATH}/session/artifacts`, {
    params,
  });
  return resp.data;
}

/** 上传一张随消息发送的图片，返回引用 id 与渲染 URL。 */
export async function uploadPecoImage(
  file: File,
): Promise<{ id: string; url: string }> {
  const form = new FormData();
  form.append("file", file);
  const resp = await api.post<{ id: string; url: string }>(
    `${PATH}/images`,
    form,
    { headers: { "Content-Type": "multipart/form-data" } },
  );
  return resp.data;
}

/** 统一附件端点的上传响应：服务端按扩展名分类，`url` 仅图片有。 */
export interface PecoUploadedFile {
  id: string;
  kind: "image" | "document";
  /** 原始文件名 */
  name: string;
  size: number;
  /** 图片的渲染 URL；文档缺省（不进静态目录） */
  url?: string;
}

/**
 * 上传一个随消息发送的附件（图片或文档），随 `file_ids` 发送。
 *
 * 服务端按扩展名白名单分类；文档不返回 url，原始名只随 `name` 回传。
 */
export async function uploadPecoFile(file: File): Promise<PecoUploadedFile> {
  const form = new FormData();
  form.append("file", file);
  const resp = await api.post<PecoUploadedFile>(`${PATH}/files`, form, {
    headers: { "Content-Type": "multipart/form-data" },
  });
  return resp.data;
}

export function pecoStreamUrl(
  message: string,
  imageIds: string[] = [],
  fileIds: string[] = [],
): string {
  const images = imageIds.length > 0 ? `&image_ids=${imageIds.join(",")}` : "";
  const files = fileIds.length > 0 ? `&file_ids=${fileIds.join(",")}` : "";
  return `${SSE_BASE}/stream?message=${encodeURIComponent(message)}${images}${files}`;
}

/** 纯附着模式：无 message 参数，接上当前用户进行中的任务流。 */
export function pecoAttachUrl(): string {
  return `${SSE_BASE}/stream`;
}

/**
 * 向已注册的 run 排队一条消息（复用已持有的 SSE 连接，不新开连接）。
 *
 * 无活跃 run 时抛 404 —— 调用方应改走 `pecoStreamUrl` 的引导路径
 * （附着到已有 run 或新建，语义自洽）；控制通道积压时抛 409。
 */
export async function queryPecoStream(message: string): Promise<void> {
  await api.post(`${PATH}/stream/query`, { message });
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

/**
 * 请求暂停当前用户进行中的 ReAct 轮（无在途轮次时服务端返回 409）。
 *
 * 暂停在边界检查点落地：请求发出前，或工具批执行前。请求在途时须等本次模型调用
 * 收尾；工具批执行中不打断，批跑完才停；无工具调用的轮次暂停被吸收。本调用只
 * 表示「请求已收到」；实际暂停态以 SSE `looper_state` 事件为准，调用方不得据返回值乐观置位。
 */
export async function pausePecoStream(): Promise<{
  success: boolean;
  message?: string;
}> {
  const resp = await api.post<{ success: boolean; message?: string }>(
    `${PATH}/stream/pause`,
  );
  return resp.data;
}

/** 恢复被暂停的 ReAct 轮（无在途轮次时服务端返回 409）。 */
export async function resumePecoStream(): Promise<{
  success: boolean;
  message?: string;
}> {
  const resp = await api.post<{ success: boolean; message?: string }>(
    `${PATH}/stream/resume`,
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
