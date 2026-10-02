// 「记忆」页 API client —— 路径 / 查询参数与后端 E1/E2 逐字对齐（design §3.1）

import api from "./client";
import type { MemoryDocumentPage, MemoryGraphResponse } from "@/types/memory";

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

/** 读取 `@private_memory` 文档列表（`has_more` 分页）。 */
export async function listMemoryDocuments(
  offset: number,
  limit: number,
): Promise<MemoryDocumentPage> {
  const res = await api.get<MemoryDocumentPage>(`${PATH}/documents`, {
    params: { offset, limit },
  });
  return res.data;
}
