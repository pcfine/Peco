// 「记忆」页 API client —— 路径 / 查询参数与后端 E1/E2 逐字对齐（design §3.1）

import api from "./client";
import type {
  MemoryAuditItem,
  MemoryDocumentDetail,
  MemoryDocumentPage,
  MemoryGraphResponse,
  MemorySearchResponse,
  RestoreMemoryResponse,
  SupersedeHealth,
} from "@/types/memory";

// axios baseURL 为 "/api"，故此处路径相对它。
const PATH = "/peco/memory";

/**
 * 读取当前用户私人记忆（`@private_memory`）的 Entity 子图。
 *
 * 无参时不带 `params`（走后端默认 `node_limit=200` / `edge_limit=500`）；
 * 有参时只传**实际给了的键**，不补默认值。
 */
export async function getMemoryGraph(
  nodeLimit?: number,
  edgeLimit?: number,
): Promise<MemoryGraphResponse> {
  if (nodeLimit === undefined && edgeLimit === undefined) {
    const res = await api.get<MemoryGraphResponse>(`${PATH}/graph`);
    return res.data;
  }

  const params: { node_limit?: number; edge_limit?: number } = {};
  if (nodeLimit !== undefined) params.node_limit = nodeLimit;
  if (edgeLimit !== undefined) params.edge_limit = edgeLimit;

  const res = await api.get<MemoryGraphResponse>(`${PATH}/graph`, { params });
  return res.data;
}

/**
 * 读取 `@private_memory` 文档列表（`has_more` 分页）。
 *
 * `source`（E2 v2 新增）仅在给出**且非空**时放进 `params`；否则不带该键
 * （与 `getMemoryGraph` 的「只传实际给了的键」口径一致）。
 */
export async function listMemoryDocuments(
  offset: number,
  limit: number,
  source?: string,
): Promise<MemoryDocumentPage> {
  const params: { offset: number; limit: number; source?: string } = {
    offset,
    limit,
  };
  if (source) params.source = source;

  const res = await api.get<MemoryDocumentPage>(`${PATH}/documents`, {
    params,
  });
  return res.data;
}

/** 读取单条记忆文档详情（E3，含全文）。`id` 需 URL 编码。 */
export async function getMemoryDocument(
  id: string,
): Promise<MemoryDocumentDetail> {
  const res = await api.get<MemoryDocumentDetail>(
    `${PATH}/documents/${encodeURIComponent(id)}`,
  );
  return res.data;
}

/**
 * 按正文内容检索记忆文档（E4，服务端子串扫描）。
 *
 * `source`（可选）仅在给出**且非空**时放进 `params`；空则不带该键。
 */
export async function searchMemoryDocuments(
  q: string,
  limit: number,
  source?: string,
): Promise<MemorySearchResponse> {
  const params: { q: string; limit: number; source?: string } = { q, limit };
  if (source) params.source = source;

  const res = await api.get<MemorySearchResponse>(`${PATH}/search`, { params });
  return res.data;
}

/**
 * 分页列出当前用户的记忆删除审计（`GET /audit`，`deleted_at` 倒序）。
 *
 * **裸数组响应 —— 无 `has_more` 信封**，到底判定由调用方按返回条数做
 * （返回条数 < limit ⇒ 到底）。`reason` 仅在给出**且非空**时放进 `params`
 * （口径同 `listMemoryDocuments` 的 `source`）。
 */
export async function listMemoryAudit(
  offset: number,
  limit: number,
  reason?: string,
): Promise<MemoryAuditItem[]> {
  const params: { offset: number; limit: number; reason?: string } = {
    offset,
    limit,
  };
  if (reason) params.reason = reason;

  const res = await api.get<MemoryAuditItem[]>(`${PATH}/audit`, { params });
  return res.data;
}

/** 按审计行回滚一条记忆删除（`POST /audit/{id}/restore`）。失败为 4xx/5xx，错误信息原样上抛。 */
export async function restoreMemoryAudit(
  id: number,
): Promise<RestoreMemoryResponse> {
  const res = await api.post<RestoreMemoryResponse>(
    `${PATH}/audit/${id}/restore`,
  );
  return res.data;
}

/** 取代对账健康计数（`GET /supersede/health`）。 */
export async function getSupersedeHealth(): Promise<SupersedeHealth> {
  const res = await api.get<SupersedeHealth>(`${PATH}/supersede/health`);
  return res.data;
}

/** 手动触发一次取代对账（`POST /supersede/reconcile`），返回与 health 同形的计数。 */
export async function triggerSupersedeReconcile(): Promise<SupersedeHealth> {
  const res = await api.post<SupersedeHealth>(`${PATH}/supersede/reconcile`);
  return res.data;
}
