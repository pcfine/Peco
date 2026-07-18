//! HelixDB JSON 查询构建器。
//!
//! 所有查询函数接受 `&HelixSchema` 参数，从 schema 配置中读取
//! 节点标签、属性名和边标签，避免硬编码。
//!
//! 查询格式参考 HelixDB 文档的 JSON AST 规范（v2 API）：
//! `POST /v1/query` 接受 `{"request_type": "...", "query": {...}}` 格式。

use serde_json::{Value, json};

use super::types::HelixSchema;
use crate::traits::combined_search::CombinedQuery;
use crate::types::{Chunk, Document, SearchFilters};

// ── 辅助函数 ──────────────────────────────────────────────────────────────

/// 将 `&[f32]` 转换为 `Vec<f64>`，满足 HelixDB JSON 的数值格式要求。
fn vec_f32_to_f64(v: &[f32]) -> Vec<f64> {
    v.iter().map(|&x| x as f64).collect()
}

// ── PropertyValue 包装器 ─────────────────────────────────────────────────

/// 包装字符串值为 HelixDB `PropertyValue` JSON 格式。
///
/// 生成 `{"Value": {"String": "..."}}`。
fn prop_str(s: &str) -> Value {
    json!({"Value": {"String": s}})
}

/// 包装 f32 数组为 HelixDB `PropertyValue` JSON 格式。
///
/// 生成 `{"Value": {"F32Array": [...]}}`。
fn prop_f32_array(v: &[f32]) -> Value {
    json!({"Value": {"F32Array": vec_f32_to_f64(v)}})
}

/// 包装 f64 为 HelixDB `PropertyValue` JSON 格式。
///
/// 生成 `{"Value": {"F64": ...}}`。
pub(crate) fn prop_f64(v: f64) -> Value {
    json!({"Value": {"F64": v}})
}

/// 包装 i64 为 HelixDB `PropertyValue` JSON 格式。
///
/// 生成 `{"Value": {"I64": ...}}`。
fn prop_i64(v: i64) -> Value {
    json!({"Value": {"I64": v}})
}

// ── 属性构建器 ────────────────────────────────────────────────────────────

/// 构建文档属性数组（v2 AddN `properties` 格式）。
///
/// 返回 `[["key", PropertyValue], ...]` 的元组数组。
fn document_props(doc: &Document, metadata_json: &str, embedding: &[f32]) -> Value {
    let embedding_val = if embedding.is_empty() {
        json!({"Value": {"F32Array": []}})
    } else {
        prop_f32_array(embedding)
    };

    json!([
        ["id", prop_str(&doc.id)],
        ["title", prop_str(&doc.title)],
        ["source_path", prop_str(&doc.source_path)],
        ["content", prop_str(&doc.content)],
        ["metadata", prop_str(metadata_json)],
        ["embedding", embedding_val],
    ])
}

/// 构建分块属性数组（v2 AddN `properties` 格式）。
fn chunk_props(chunk: &Chunk) -> Value {
    let metadata = json!({
        "start_char": chunk.metadata.start_char,
        "end_char": chunk.metadata.end_char,
        "heading_path": chunk.metadata.heading_path,
    });

    let mut props: Vec<Value> = vec![
        json!(["id", prop_str(&chunk.id)]),
        json!(["document_id", prop_str(&chunk.document_id)]),
        json!(["text", prop_str(&chunk.text)]),
        json!(["sequence_index", prop_i64(chunk.sequence_index as i64)]),
        json!(["metadata", prop_str(&metadata.to_string())]),
        json!(["embedding", prop_f32_array(&chunk.embedding)]),
    ];

    // page_number 是可选的 — 只在 Some 时加入
    if let Some(pn) = chunk.page_number {
        props.push(json!(["page_number", prop_i64(pn as i64)]));
    }

    Value::Array(props)
}

// ── 搜索过滤 Where 步骤 ──────────────────────────────────────────────────

/// 构建搜索过滤器 Where 步骤（如果有过滤器）。
fn filter_step(filters: Option<&SearchFilters>) -> Option<Value> {
    let f = filters?;
    let mut predicates = Vec::new();

    if let Some(ref doc_ids) = f.document_ids {
        let ids: Vec<Value> = doc_ids.iter().map(|id| json!({"String": id})).collect();
        predicates.push(json!({"IsIn": ["document_id", ids]}));
    }

    if let Some(ref file_types) = f.file_types {
        let types: Vec<Value> = file_types.iter().map(|ft| json!({"String": ft})).collect();
        // 文件类型存储在 metadata JSON 中 — 此处用简单的相等检查
        predicates.push(json!({"IsIn": ["metadata", types]}));
    }

    if predicates.is_empty() {
        None
    } else if predicates.len() == 1 {
        Some(json!({"Where": predicates.remove(0)}))
    } else {
        Some(json!({"Where": {"And": predicates}}))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Write 操作
// ═══════════════════════════════════════════════════════════════════════════

/// 创建 Document 节点（v2 AddN 对象格式）。
pub fn create_document_node(
    schema: &HelixSchema,
    doc: &Document,
    metadata_json: &str,
    embedding: &[f32],
) -> Value {
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "doc", "steps": [
                    {"AddN": {
                        "label": schema.content_node_label,
                        "properties": document_props(doc, metadata_json, embedding)
                    }},
                    {"Project": [{"source": "$id", "alias": "id"}]}
                ], "condition": null}}
            ],
            "returns": ["doc"]
        }
    })
}

/// 创建 Chunk 节点（v2 AddN 对象格式）。
pub fn create_chunk_node(schema: &HelixSchema, chunk: &Chunk) -> Value {
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "chunk", "steps": [
                    {"AddN": {
                        "label": schema.fragment_node_label,
                        "properties": chunk_props(chunk)
                    }},
                    {"Project": [{"source": "$id", "alias": "id"}]}
                ], "condition": null}}
            ],
            "returns": ["chunk"]
        }
    })
}

/// 创建 Document → Chunk 的 CONTAINS 边（v2 AddE 对象格式）。
pub fn create_contains_edge(schema: &HelixSchema, doc_id: &str, chunk_id: &str) -> Value {
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "doc", "steps": [
                    {"NWhere": {"Eq": [schema.id_property, {"String": doc_id}]}}
                ], "condition": null}},
                {"Query": {"name": "chunk", "steps": [
                    {"NWhere": {"Eq": [schema.id_property, {"String": chunk_id}]}}
                ], "condition": null}},
                {"Query": {"name": "edge", "steps": [
                    {"N": {"Var": "doc"}},
                    {"AddE": {
                        "label": schema.contains_edge,
                        "to": {"Var": "chunk"},
                        "properties": []
                    }},
                    {"Count": null}
                ], "condition": null}}
            ],
            "returns": ["edge"]
        }
    })
}

/// 创建 NEXT_CHUNK 边（v2 AddE 对象格式）。
pub fn create_next_chunk_edge(schema: &HelixSchema, chunk_a_id: &str, chunk_b_id: &str) -> Value {
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "a", "steps": [
                    {"NWhere": {"Eq": [schema.id_property, {"String": chunk_a_id}]}}
                ], "condition": null}},
                {"Query": {"name": "b", "steps": [
                    {"NWhere": {"Eq": [schema.id_property, {"String": chunk_b_id}]}}
                ], "condition": null}},
                {"Query": {"name": "edge", "steps": [
                    {"N": {"Var": "a"}},
                    {"AddE": {
                        "label": schema.next_fragment_edge,
                        "to": {"Var": "b"},
                        "properties": []
                    }},
                    {"Count": null}
                ], "condition": null}}
            ],
            "returns": ["edge"]
        }
    })
}

/// 创建 RELATED_TO 边（v2 AddE 对象格式）。
pub fn create_related_to_edge(
    schema: &HelixSchema,
    doc_id: &str,
    other_id: &str,
    weight: f64,
) -> Value {
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "doc", "steps": [
                    {"NWhere": {"Eq": [schema.id_property, {"String": doc_id}]}}
                ], "condition": null}},
                {"Query": {"name": "other", "steps": [
                    {"NWhere": {"Eq": [schema.id_property, {"String": other_id}]}}
                ], "condition": null}},
                {"Query": {"name": "edge", "steps": [
                    {"N": {"Var": "doc"}},
                    {"AddE": {
                        "label": schema.related_edge,
                        "to": {"Var": "other"},
                        "properties": [["weight", prop_f64(weight)]]
                    }},
                    {"Count": null}
                ], "condition": null}}
            ],
            "returns": ["edge"]
        }
    })
}

/// 级联删除文档及其所有分块和关联边。
pub fn delete_document_cascade(schema: &HelixSchema, doc_id: &str) -> Value {
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "doc", "steps": [
                    {"NWhere": {"Eq": [schema.id_property, {"String": doc_id}]}}
                ], "condition": null}},
                {"Query": {"name": "chunks", "steps": [
                    {"N": {"Var": "doc"}},
                    {"Out": schema.contains_edge}
                ], "condition": null}},
                {"Query": {"name": "dropped_chunks", "steps": [
                    {"N": {"Var": "chunks"}},
                    {"Drop": null},
                    {"Count": null}
                ], "condition": null}},
                {"Query": {"name": "dropped_doc", "steps": [
                    {"N": {"Var": "doc"}},
                    {"Drop": null},
                    {"Count": null}
                ], "condition": null}}
            ],
            "returns": ["dropped_doc"]
        }
    })
}

/// 通过 ID 删除单个 Chunk 节点。
pub fn delete_chunk_by_id(schema: &HelixSchema, chunk_id: &str) -> Value {
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "dropped", "steps": [
                    {"NWhere": {"Eq": [schema.id_property, {"String": chunk_id}]}},
                    {"Drop": null},
                    {"Count": null}
                ], "condition": null}}
            ],
            "returns": ["dropped"]
        }
    })
}

/// 更新 Chunk 节点的 embedding 属性（v2 SetProperty 格式）。
///
/// SetProperty 在 v2 中仍为 `[prop_name, PropertyValue]` 二元数组，
/// 但值须包装为 `{"Value": {...}}` 格式。
pub fn update_chunk_embedding(schema: &HelixSchema, chunk_id: &str, embedding: &[f32]) -> Value {
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "updated", "steps": [
                    {"NWhere": {"Eq": [schema.id_property, {"String": chunk_id}]}},
                    {"SetProperty": [schema.fragment_vector_property, prop_f32_array(embedding)]},
                    {"Count": null}
                ], "condition": null}}
            ],
            "returns": ["updated"]
        }
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// Read 操作
// ═══════════════════════════════════════════════════════════════════════════

/// 向量 ANN 搜索分块（v2 VectorSearchNodes 对象格式）。
pub fn vector_search_chunks(
    schema: &HelixSchema,
    query_vec: &[f32],
    top_k: u32,
    filters: Option<&SearchFilters>,
) -> Value {
    let steps = build_vector_search_steps(schema, query_vec, top_k, filters);
    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "results", "steps": steps, "condition": null}}
            ],
            "returns": ["results"]
        }
    })
}

/// 构建向量搜索步骤列表。
fn build_vector_search_steps(
    schema: &HelixSchema,
    query_vec: &[f32],
    top_k: u32,
    filters: Option<&SearchFilters>,
) -> Vec<Value> {
    let mut steps: Vec<Value> = vec![json!({
        "VectorSearchNodes": {
            "label": schema.fragment_node_label,
            "property": schema.fragment_vector_property,
            "query_vector": {"Value": {"F32Array": vec_f32_to_f64(query_vec)}},
            "k": {"Literal": top_k}
        }
    })];

    if let Some(filter_step) = filter_step(filters) {
        steps.push(filter_step);
    }

    steps.push(json!({"Project": [
        {"source": "$id", "alias": "chunk_id"},
        {"source": "document_id", "alias": "document_id"},
        {"source": schema.fragment_text_property, "alias": "text"},
        {"source": "$distance", "alias": "score"}
    ]}));

    steps
}

/// 全文搜索分块（v2 TextSearchNodes 对象格式）。
pub fn text_search_chunks(
    schema: &HelixSchema,
    query_text: &str,
    top_k: u32,
    filters: Option<&SearchFilters>,
) -> Value {
    let mut steps: Vec<Value> = vec![json!({
        "TextSearchNodes": {
            "label": schema.fragment_node_label,
            "property": schema.fragment_text_property,
            "query_text": {"Value": {"String": query_text}},
            "k": {"Literal": top_k}
        }
    })];

    if let Some(filter_step) = filter_step(filters) {
        steps.push(filter_step);
    }

    steps.push(json!({"Project": [
        {"source": "$id", "alias": "chunk_id"},
        {"source": "document_id", "alias": "document_id"},
        {"source": schema.fragment_text_property, "alias": "text"},
        {"source": "$distance", "alias": "score"}
    ]}));

    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "results", "steps": steps, "condition": null}}
            ],
            "returns": ["results"]
        }
    })
}

/// 按 ID 获取文档。
pub fn get_document_by_id(schema: &HelixSchema, doc_id: &str) -> Value {
    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "doc", "steps": [
                    {"NWhere": {"Eq": [schema.id_property, {"String": doc_id}]}},
                    {"Project": [
                        {"source": "$id", "alias": "id"},
                        {"source": "title", "alias": "title"},
                        {"source": "source_path", "alias": "source_path"},
                        {"source": schema.content_text_property, "alias": "content"},
                        {"source": "metadata", "alias": "metadata"}
                    ]}
                ], "condition": null}}
            ],
            "returns": ["doc"]
        }
    })
}

/// 获取文档的所有分块。
pub fn get_document_chunks(schema: &HelixSchema, doc_id: &str) -> Value {
    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "doc", "steps": [
                    {"NWhere": {"Eq": [schema.id_property, {"String": doc_id}]}}
                ], "condition": null}},
                {"Query": {"name": "chunks", "steps": [
                    {"N": {"Var": "doc"}},
                    {"Out": schema.contains_edge},
                    {"OrderBy": ["sequence_index", "Asc"]},
                    {"Project": [
                        {"source": "$id", "alias": "chunk_id"},
                        {"source": schema.fragment_text_property, "alias": "text"},
                        {"source": "document_id", "alias": "document_id"},
                        {"source": "sequence_index", "alias": "sequence_index"},
                        {"source": "page_number", "alias": "page_number"}
                    ]}
                ], "condition": null}}
            ],
            "returns": ["chunks"]
        }
    })
}

/// 分页列出文档（v2: NWithLabel → NWhere + $label 等式）。
pub fn list_documents(schema: &HelixSchema, offset: usize, limit: usize) -> Value {
    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "docs", "steps": [
                    {"NWhere": {"Eq": ["$label", {"String": schema.content_node_label}]}},
                    {"Skip": offset},
                    {"Limit": limit},
                    {"Project": [
                        {"source": "$id", "alias": "id"},
                        {"source": "title", "alias": "title"},
                        {"source": "source_path", "alias": "source_path"},
                        {"source": "metadata", "alias": "metadata"}
                    ]}
                ], "condition": null}}
            ],
            "returns": ["docs"]
        }
    })
}

/// 统计节点数量（v2: NWithLabel → NWhere + $label 等式）。
pub fn count_nodes(_schema: &HelixSchema, label: &str) -> Value {
    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "count", "steps": [
                    {"NWhere": {"Eq": ["$label", {"String": label}]}},
                    {"Count": null}
                ], "condition": null}}
            ],
            "returns": ["count"]
        }
    })
}

/// 图遍历：从起始节点沿指定边类型做 BFS。
pub fn traverse_graph(
    schema: &HelixSchema,
    start_node_id: &str,
    edge_label: &str,
    direction: &str, // "Out", "In", "Both"
    max_depth: u32,
) -> Value {
    let dir_step: Value = match direction {
        "In" => json!({"In": edge_label}),
        "Both" => json!({"Both": edge_label}),
        _ => json!({"Out": edge_label}),
    };

    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "start", "steps": [
                    {"NWhere": {"Eq": [schema.id_property, {"String": start_node_id}]}}
                ], "condition": null}},
                {"Query": {"name": "traversed", "steps": [
                    {"N": {"Var": "start"}},
                    {"Repeat": {
                        "traversal": {"steps": [dir_step]},
                        "max_depth": max_depth,
                        "emit": "All"
                    }},
                    "Dedup",
                    {"Project": [
                        {"source": "$id", "alias": "node_id"},
                        {"source": "$label", "alias": "label"},
                        {"source": "$distance", "alias": "distance"}
                    ]}
                ], "condition": null}}
            ],
            "returns": ["traversed"]
        }
    })
}

/// 图扩展：从分块 ID 出发，找父文档，再做关联扩展。
///
/// 分块 ID 是 HelixDB 内部 `$id`（自增整数，以数字字符串形式传入）。
/// 使用 `N: {"Ids": [id1, id2, ...]}` 一步选中所有分块节点，
/// 然后沿 CONTAINS 入边找到各自的父文档，再做图扩展。
pub fn expand_from_chunks(
    schema: &HelixSchema,
    chunk_ids: &[String],
    edge_labels: &[String],
    max_depth: u32,
) -> Value {
    // 将字符串 ID 解析为 i64
    let chunk_i64_ids: Vec<i64> = chunk_ids
        .iter()
        .filter_map(|cid| cid.parse::<i64>().ok())
        .collect();

    // 选中所有分块 → In CONTAINS 找父文档 → 去重
    let mut steps: Vec<Value> = vec![
        json!({"N": {"Ids": chunk_i64_ids}}),
        json!({"In": schema.contains_edge}),
        json!({"Dedup": null}),
    ];

    // 在父文档之间做图扩展
    if max_depth > 0 && !edge_labels.is_empty() {
        let repeat_branches: Vec<Value> = edge_labels
            .iter()
            .map(|label| json!({"steps": [{"Both": label}]}))
            .collect();

        steps.push(json!({"Repeat": {
            "traversal": {"steps": [
                {"Union": repeat_branches}
            ]},
            "max_depth": max_depth,
            "emit": "All"
        }}));
        steps.push(json!({"Dedup": null}));
    }

    steps.push(json!({"Project": [
        {"source": "$id", "alias": "document_id"},
        {"source": "title", "alias": "title"},
        {"source": "source_path", "alias": "source_path"},
        {"source": schema.content_text_property, "alias": "content"},
        {"source": "$distance", "alias": "distance"}
    ]}));

    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "expanded", "steps": steps, "condition": null}}
            ],
            "returns": ["expanded"]
        }
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// CombinedSearch 查询
// ═══════════════════════════════════════════════════════════════════════════

/// 构建组合搜索的 single readBatch JSON。
///
/// 包含 2 个独立 `varAs`，各自完成「检索 + 图扩展」管道：
/// 1. `vector_path` — 向量搜索 → 找父文档 → 图扩展
/// 2. `text_path` — 全文搜索 → 找父文档 → 图扩展
///
/// 每条管道自包含，不跨 varAs 引用。
pub fn combined_search_query(schema: &HelixSchema, query: &CombinedQuery) -> Value {
    let graph_depth = query.graph_expansion_depth;
    let related = &schema.related_edge;
    let belongs = &schema.belongs_to_edge;

    // ── vector_path ──
    let mut vector_steps: Vec<Value> = vec![json!({
        "VectorSearchNodes": {
            "label": schema.fragment_node_label,
            "property": schema.fragment_vector_property,
            "query_vector": {"Value": {"F32Array": vec_f32_to_f64(&query.query_vector)}},
            "k": {"Literal": query.vector_top_k as u32}
        }
    })];

    if let Some(ref f) = query.filters {
        if let Some(filter_step) = filter_step(Some(f)) {
            vector_steps.push(filter_step);
        }
    }

    // 投影命中分块
    vector_steps.push(json!({"Project": [
        {"source": "$id", "alias": "chunk_id"},
        {"source": "document_id", "alias": "document_id"},
        {"source": schema.fragment_text_property, "alias": "text"},
        {"source": "$distance", "alias": "score"}
    ]}));

    // In CONTAINS → 找父文档 → 图扩展
    add_graph_expansion_steps(&mut vector_steps, schema, graph_depth, related, belongs);

    // ── text_path ──
    let mut text_steps: Vec<Value> = vec![json!({
        "TextSearchNodes": {
            "label": schema.fragment_node_label,
            "property": schema.fragment_text_property,
            "query_text": {"Value": {"String": query.query_text}},
            "k": {"Literal": query.text_top_k as u32}
        }
    })];

    if let Some(ref f) = query.filters {
        if let Some(filter_step) = filter_step(Some(f)) {
            text_steps.push(filter_step);
        }
    }

    // 投影命中分块
    text_steps.push(json!({"Project": [
        {"source": "$id", "alias": "chunk_id"},
        {"source": "document_id", "alias": "document_id"},
        {"source": schema.fragment_text_property, "alias": "text"},
        {"source": "$distance", "alias": "score"}
    ]}));

    // In CONTAINS → 找父文档 → 图扩展
    add_graph_expansion_steps(&mut text_steps, schema, graph_depth, related, belongs);

    json!({
        "request_type": "read",
        "query_name": "combined_search",
        "query": {
            "queries": [
                {"Query": {"name": "vector_path", "steps": vector_steps, "condition": null}},
                {"Query": {"name": "text_path", "steps": text_steps, "condition": null}}
            ],
            "returns": ["vector_path", "text_path"]
        }
    })
}

/// 向管道追加「In CONTAINS → 去重 → Repeat 图扩展 → 去重 → 投影文档字段」步骤。
fn add_graph_expansion_steps(
    steps: &mut Vec<Value>,
    schema: &HelixSchema,
    graph_depth: u32,
    related_edge: &str,
    belongs_to_edge: &str,
) {
    // In CONTAINS: 从命中分块沿入边找到父文档
    steps.push(json!({"In": schema.contains_edge}));
    steps.push(json!({"Dedup": null}));

    // 如果 depth > 0，做多跳图扩展
    if graph_depth > 0 {
        steps.push(json!({"Repeat": {
            "traversal": {"steps": [
                {"Union": [
                    {"steps": [{"Both": related_edge}]},
                    {"steps": [{"Both": belongs_to_edge}]}
                ]}
            ]},
            "max_depth": graph_depth,
            "emit": "All"
        }}));
        steps.push(json!({"Dedup": null}));
    }

    // 投影文档字段（包含 graph_distance 用于区分图扩展结果）
    steps.push(json!({"Project": [
        {"source": "$id", "alias": "document_id"},
        {"source": "title", "alias": "title"},
        {"source": schema.content_text_property, "alias": "content"},
        {"source": "source_path", "alias": "source_path"},
        {"source": "$distance", "alias": "graph_distance"}
    ]}));
}

// ═══════════════════════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::combined_search::RrfConfig;

    fn test_schema() -> HelixSchema {
        HelixSchema::default()
    }

    #[test]
    fn build_vector_search_query() {
        let q = vector_search_chunks(&test_schema(), &[0.1, 0.2, 0.3], 10, None);
        assert_eq!(q["request_type"], "read");
        let queries = q["query"]["queries"].as_array().unwrap();
        assert_eq!(queries.len(), 1);
        let steps = queries[0]["Query"]["steps"].as_array().unwrap();
        // 第一步应为 VectorSearchNodes（v2 对象格式）
        let first_step = steps[0].as_object().unwrap();
        assert!(first_step.contains_key("VectorSearchNodes"));
        let vsn = first_step["VectorSearchNodes"].as_object().unwrap();
        assert!(vsn.contains_key("label"));
        assert!(vsn.contains_key("property"));
        assert!(vsn.contains_key("query_vector"));
        assert!(vsn.contains_key("k"));
        // 最后一步应为 Project
        let last = steps.last().unwrap();
        assert!(last.as_object().unwrap().contains_key("Project"));
    }

    #[test]
    fn build_text_search_query() {
        let q = text_search_chunks(&test_schema(), "test query", 5, None);
        assert_eq!(q["request_type"], "read");
        let steps = q["query"]["queries"][0]["Query"]["steps"]
            .as_array()
            .unwrap();
        let first_step = steps[0].as_object().unwrap();
        assert!(first_step.contains_key("TextSearchNodes"));
        let tsn = first_step["TextSearchNodes"].as_object().unwrap();
        assert!(tsn.contains_key("label"));
        assert!(tsn.contains_key("query_text"));
    }

    #[test]
    fn build_combined_search_query_no_graph() {
        let cq = CombinedQuery {
            query_text: "test".into(),
            query_vector: vec![0.1, 0.2],
            vector_top_k: 10,
            text_top_k: 10,
            graph_expansion_depth: 0,
            graph_edge_types: vec![],
            fusion: RrfConfig::default(),
            filters: None,
        };
        let q = combined_search_query(&test_schema(), &cq);
        assert_eq!(q["request_type"], "read");
        assert_eq!(q["query_name"], "combined_search");
        let queries = q["query"]["queries"].as_array().unwrap();
        assert_eq!(queries.len(), 2);
        assert_eq!(queries[0]["Query"]["name"], "vector_path");
        assert_eq!(queries[1]["Query"]["name"], "text_path");

        // vector_path 应包含 VectorSearchNodes（v2 对象）→ Project → In → Dedup → Project
        let vsteps = queries[0]["Query"]["steps"].as_array().unwrap();
        let first = vsteps[0].as_object().unwrap();
        assert!(first.contains_key("VectorSearchNodes"));
        // 最后一步应为 Project（文档字段投影）
        let vlast = vsteps.last().unwrap();
        assert!(vlast.as_object().unwrap().contains_key("Project"));

        // text_path 同理
        let tsteps = queries[1]["Query"]["steps"].as_array().unwrap();
        let first_t = tsteps[0].as_object().unwrap();
        assert!(first_t.contains_key("TextSearchNodes"));
    }

    #[test]
    fn build_combined_search_query_with_graph() {
        let cq = CombinedQuery {
            query_text: "graph query".into(),
            query_vector: vec![0.5; 4],
            vector_top_k: 15,
            text_top_k: 15,
            graph_expansion_depth: 2,
            graph_edge_types: vec![],
            fusion: RrfConfig::default(),
            filters: None,
        };
        let q = combined_search_query(&test_schema(), &cq);
        let vsteps = q["query"]["queries"][0]["Query"]["steps"]
            .as_array()
            .unwrap();

        // 应包含 Repeat 步骤（图扩展）
        let has_repeat = vsteps
            .iter()
            .any(|s| s.as_object().map_or(false, |o| o.contains_key("Repeat")));
        assert!(has_repeat, "graph_depth > 0 时应包含 Repeat 步骤");
    }

    #[test]
    fn build_document_crud_queries() {
        let s = test_schema();

        // get
        let q = get_document_by_id(&s, "doc-1");
        assert_eq!(q["request_type"], "read");

        // list — 验证使用 NWhere + $label 格式
        let q = list_documents(&s, 0, 10);
        assert_eq!(q["request_type"], "read");
        let steps = q["query"]["queries"][0]["Query"]["steps"]
            .as_array()
            .unwrap();
        let first = steps[0].as_object().unwrap();
        // list 现在使用 NWhere 而不是 NWithLabel
        assert!(first.contains_key("NWhere"));

        // delete cascade
        let q = delete_document_cascade(&s, "doc-1");
        assert_eq!(q["request_type"], "write");
    }

    #[test]
    fn add_n_uses_v2_object_format() {
        let s = test_schema();
        let doc = Document {
            id: "test-1".into(),
            kb_id: None,
            title: "Test".into(),
            source_path: "/tmp/test.txt".into(),
            content: "hello".into(),
            metadata: Default::default(),
        };
        let q = create_document_node(&s, &doc, "{}", &[0.1, 0.2]);
        assert_eq!(q["request_type"], "write");

        let steps = q["query"]["queries"][0]["Query"]["steps"]
            .as_array()
            .unwrap();
        let addn = &steps[0]["AddN"];
        // v2 AddN 是对象，包含 label 和 properties
        assert!(addn.is_object(), "AddN 应为 v2 对象格式");
        let obj = addn.as_object().unwrap();
        assert!(obj.contains_key("label"), "AddN 应包含 label");
        assert!(obj.contains_key("properties"), "AddN 应包含 properties");
        assert_eq!(obj["label"], s.content_node_label);

        // properties 是 [[k, v], ...] 数组
        let props = obj["properties"].as_array().unwrap();
        assert!(!props.is_empty());
        // 每个属性是 [k, PropertyValue] 二元组
        for prop in props {
            let pair = prop.as_array().unwrap();
            assert_eq!(pair.len(), 2);
            assert!(pair[0].is_string());
            // PropertyValue 应包含 Value 包装
            assert!(pair[1].as_object().unwrap().contains_key("Value"));
        }
    }

    #[test]
    fn add_e_uses_v2_object_format() {
        let s = test_schema();
        let q = create_contains_edge(&s, "doc-1", "chunk-1");
        assert_eq!(q["request_type"], "write");

        // 第三个 query 是 edge
        let queries = q["query"]["queries"].as_array().unwrap();
        let edge_query = &queries[2];
        let steps = edge_query["Query"]["steps"].as_array().unwrap();
        // 第二步应该是 AddE
        let adde = &steps[1]["AddE"];
        assert!(adde.is_object(), "AddE 应为 v2 对象格式");
        let obj = adde.as_object().unwrap();
        assert!(obj.contains_key("label"), "AddE 应包含 label");
        assert!(obj.contains_key("to"), "AddE 应包含 to");
        assert!(obj.contains_key("properties"), "AddE 应包含 properties");
        assert_eq!(obj["label"], s.contains_edge);
        assert_eq!(obj["to"]["Var"], "chunk");
    }

    #[test]
    fn vec_f32_to_f64_conversion() {
        let input = [1.0f32, 0.5, -0.25];
        let output = vec_f32_to_f64(&input);
        assert_eq!(output.len(), 3);
        assert!((output[0] - 1.0).abs() < 1e-10);
        assert!((output[1] - 0.5).abs() < 1e-10);
        assert!((output[2] - (-0.25)).abs() < 1e-10);
    }

    #[test]
    fn prop_value_wrappers() {
        // prop_str
        let p = prop_str("hello");
        assert_eq!(p["Value"]["String"], "hello");

        // prop_f32_array
        let p = prop_f32_array(&[1.0, 2.0]);
        let arr = p["Value"]["F32Array"].as_array().unwrap();
        assert_eq!(arr.len(), 2);

        // prop_f64
        let p = prop_f64(3.14);
        assert!((p["Value"]["F64"].as_f64().unwrap() - 3.14).abs() < 1e-10);

        // prop_i64
        let p = prop_i64(42);
        assert_eq!(p["Value"]["I64"], 42);
    }

    #[test]
    fn filter_with_document_ids() {
        let filters = SearchFilters {
            document_ids: Some(vec!["doc-a".into(), "doc-b".into()]),
            file_types: None,
        };
        let step = filter_step(Some(&filters));
        assert!(step.is_some());
        let step_val = step.unwrap();
        assert!(step_val.as_object().unwrap().contains_key("Where"));
    }

    #[test]
    fn filter_none_returns_none() {
        assert!(filter_step(None).is_none());
    }

    #[test]
    fn filter_empty_returns_none() {
        assert!(filter_step(Some(&SearchFilters::default())).is_none());
    }

    #[test]
    fn count_nodes_uses_nwhere() {
        let q = count_nodes(&test_schema(), "Document");
        let steps = q["query"]["queries"][0]["Query"]["steps"]
            .as_array()
            .unwrap();
        let first = steps[0].as_object().unwrap();
        assert!(
            first.contains_key("NWhere"),
            "count_nodes 应使用 NWhere 而非 NWithLabel"
        );
    }
}
