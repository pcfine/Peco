// 「记忆」页类型定义 —— 字段与后端 E1/E2 响应逐字对齐
// E1 `GET /peco/memory/graph`、E2 `GET /peco/memory/documents`

/**
 * 图谱节点。`id` 为稳定 id（`entity:Entity:<hash8>`），`name` 缺失时为 `""`。
 *
 * `type` 是实体类型（后端闭集 + `Other` 兜底）；后端把**缺失或闭集外**的取值
 * 一律下发为 `"Other"` —— 正常响应里不会缺这个字段，但前端仍按「未知」兜底。
 */
export interface MemoryNode {
  id: string;
  name: string;
  type: string;
}

/**
 * 图谱边。**无稳定边 id**（后端不返回），前端用复合键生成 React key。
 *
 * `predicate` 是边 label 原文（未映射也不改写）；`group` 是读侧派生的关系组，
 * **未映射谓词无此字段**（缺省不是错误，只是没有对应组）。
 */
export interface MemoryEdge {
  source: string;
  target: string;
  predicate: string;
  weight: number;
  group?: string;
}

/** E1 响应：`nodes` 或 `edges` 任一被上限截断时 `truncated` 为 `true`。 */
export interface MemoryGraphResponse {
  nodes: MemoryNode[];
  edges: MemoryEdge[];
  truncated: boolean;
}

/** 单条记忆文档摘要。`file_type` 由后端 `metadata` 解析，可能为 `null`。 */
export interface MemoryDocumentItem {
  id: string;
  title: string;
  source: string;
  file_type: string | null;
}

/** E2 响应：分页用 `has_more`（`limit+1` 探测），无 `total`。 */
export interface MemoryDocumentPage {
  documents: MemoryDocumentItem[];
  offset: number;
  limit: number;
  has_more: boolean;
}

/**
 * `DELETE /peco/memory/documents/{id}` 响应。`audit_id` 是可追溯锚点
 * （删除审计行 id），删除已生效、调用方无需再查列表确认。
 */
export interface DeleteMemoryDocumentResponse {
  doc_id: string;
  removed_chunks: number;
  audit_id: number;
}

/**
 * `DELETE /peco/memory/graph/nodes/{id}` 响应。边数是**下界**（读-删之间有并发
 * 窗口），仅用于告知用户，不参与一致性判断。
 */
export interface DeleteMemoryNodeResponse {
  entity_id: string;
  removed_edges: number;
  removed_nodes: number;
  audit_id: number;
}

/** E3 响应：单条记忆文档详情（含全文）。`content` 可能为空串。 */
export interface MemoryDocumentDetail {
  id: string;
  title: string;
  source: string;
  file_type: string | null;
  created_at: string | null;
  content: string;
}

/**
 * 记忆删除审计行（`GET /peco/memory/audit` 裸数组元素）。
 *
 * 后端对 `restored_at` / `restored_doc_id` / `topic_key` / `successor_doc_id` /
 * `successor_title` / `retention_days_remaining` 均 `skip_serializing_if=None`：
 * **缺省即不存在**，TS 侧一律可选。`successor_*` 与 `retention_days_remaining`
 * 仅 `reason === "superseded"` 行有值。
 */
export interface MemoryAuditItem {
  id: number;
  kb_name: string;
  doc_id: string;
  title: string;
  content: string;
  source: string;
  reason: string;
  deleted_by: string;
  /** 'done' | 'pending' | 'cancelled' */
  status: string;
  /** RFC3339。 */
  deleted_at: string;
  /** 有值 ⇒ 已回滚。 */
  restored_at?: string;
  restored_doc_id?: string;
  /** 取代槽键（仅 superseded）。 */
  topic_key?: string;
  /** 后继 doc id（仅 superseded）。 */
  successor_doc_id?: string;
  /** 后继标题（仅 superseded；可能缺失，也可能为空串）。 */
  successor_title?: string;
  /** superseded 保留期剩余天数（仅 superseded；∈[0,30]）。 */
  retention_days_remaining?: number;
}

/** `POST /peco/memory/audit/{id}/restore` 响应。 */
export interface RestoreMemoryResponse {
  success: boolean;
  doc_id: string;
  restored_at: string;
}

/**
 * 取代对账健康计数（`GET /memory/supersede/health` 与手动对账共用形状）。
 * `degraded` 为进程级内存态（重启清零）；`last_converged_at` 从未收口则为 null。
 */
export interface SupersedeHealth {
  pending: number;
  processing: number;
  failed: number;
  degraded: number;
  last_converged_at: string | null;
}

/**
 * E4 检索的一条文档级命中。**无 `score`** —— 服务端为正文子串扫描，
 * 无相关性信号，不承诺任何恒定分数；排序恒为 `id` 升序。
 */
export interface MemorySearchHit {
  id: string;
  title: string;
  source: string;
  file_type: string | null;
  snippet: string;
}

/** E4 响应：一次返回 ≤ `limit` 条命中（无分页 / `total` / `has_more`）。 */
export interface MemorySearchResponse {
  hits: MemorySearchHit[];
}
