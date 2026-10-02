// 「记忆」页类型定义 —— 字段与后端 E1/E2 响应逐字对齐（design §3.1）
// E1 `GET /peco/memory/graph`、E2 `GET /peco/memory/documents`

/** 图谱节点。`id` 为稳定 id（`entity:Entity:<hash8>`），`name` 缺失时为 `""`。 */
export interface MemoryNode {
  id: string;
  name: string;
}

/** 图谱边。**无稳定边 id**（后端不返回），前端用复合键生成 React key。 */
export interface MemoryEdge {
  source: string;
  target: string;
  predicate: string;
  weight: number;
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
