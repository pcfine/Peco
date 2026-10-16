//! HelixDB JSON 查询构建器。
//!
//! 所有查询函数接受 `&HelixSchema` 参数，从 schema 配置中读取
//! 节点标签、属性名和边标签，避免硬编码。
//!
//! 查询格式参考 HelixDB 文档的 JSON AST 规范（v2 API）：
//! `POST /v1/query` 接受 `{"request_type": "...", "query": {...}}` 格式。

use std::collections::HashMap;

use serde_json::{Value, json};

use super::types::HelixSchema;
use crate::error::KnowledgeError;
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

// ── 隔离谓词 ──────────────────────────────────────────────────────────────

/// `$label` 谓词 —— 节点身份由 id 与 label 共同确定，缺 label 会跨 label 误选。
fn eq_label(label: &str) -> Value {
    json!({"Eq": ["$label", {"String": label}]})
}

/// `ns` 谓词 —— 隔离边界。schema 未绑定命名空间时不产谓词。
fn eq_ns(ns: Option<&str>) -> Option<Value> {
    ns.map(|n| json!({"Eq": ["ns", {"String": n}]}))
}

/// 字符串属性的等值谓词：`id_property` 或任意字符串属性名。
fn eq_id(id_property: &str, id: &str) -> Value {
    json!({"Eq": [id_property, {"String": id}]})
}

/// 合取谓词：单个直接返回该谓词，多个包 `And`（HelixDB 不接受空 `And`，调用方至少给一个）。
fn and_all(mut predicates: Vec<Value>) -> Value {
    match predicates.len() {
        1 => predicates.swap_remove(0),
        _ => json!({"And": predicates}),
    }
}

/// 按 `$label` + `ns` + id 选中节点。
fn node_by_id(schema: &HelixSchema, label: &str, id: &str) -> Value {
    let mut predicates = vec![eq_label(label)];
    predicates.extend(eq_ns(schema.ns.as_deref()));
    predicates.push(eq_id(&schema.id_property, id));
    and_all(predicates)
}

/// 按 `$label` + `ns` 选中一整类节点。
fn nodes_by_label(schema: &HelixSchema, label: &str) -> Value {
    let mut predicates = vec![eq_label(label)];
    predicates.extend(eq_ns(schema.ns.as_deref()));
    and_all(predicates)
}

/// 图谱（实体）节点按 id 选中 —— label 与写侧同源于 `schema.graph_node_label`。
fn graph_node_by_id(schema: &HelixSchema, id: &str) -> Value {
    node_by_id(schema, &schema.graph_node_label, id)
}

/// 按 `ns` + id 选中节点，**不**限定 `$label`。
///
/// 供边写入的端点选点使用：端点 id 由调用方给出，其 label（实体、文档、分块……）
/// 这里判不出来。带上 `$label` 一旦与实际端点不符就会选空该端，`AddE` 随即因无源
/// 顶点报错 —— 事实谓词恰与 `schema.related_edge` 同名时即踩中。
pub(crate) fn node_by_ns_and_id(schema: &HelixSchema, id: &str) -> Value {
    let mut predicates: Vec<Value> = eq_ns(schema.ns.as_deref()).into_iter().collect();
    predicates.push(eq_id(&schema.id_property, id));
    and_all(predicates)
}

/// `In` 之后的 `ns` 过滤步骤：`In` 得到的父节点集不继承命中行的过滤条件。
fn ns_where_step(schema: &HelixSchema) -> Option<Value> {
    eq_ns(schema.ns.as_deref()).map(|predicate| json!({"Where": predicate}))
}

// ── 属性构建器 ────────────────────────────────────────────────────────────

/// 构建文档属性数组（v2 AddN `properties` 格式）。
///
/// 返回 `[["key", PropertyValue], ...]` 的元组数组。
/// `ns` 是隔离键，随节点落库（schema 未绑定命名空间时不写）。
fn document_props(
    doc: &Document,
    metadata_json: &str,
    embedding: &[f32],
    ns: Option<&str>,
) -> Value {
    let mut props: Vec<Value> = vec![
        json!(["id", prop_str(&doc.id)]),
        json!(["title", prop_str(&doc.title)]),
        json!(["source_path", prop_str(&doc.source_path)]),
        json!(["content", prop_str(&doc.content)]),
        json!(["metadata", prop_str(metadata_json)]),
    ];

    if let Some(ns) = ns {
        props.push(json!(["ns", prop_str(ns)]));
    }

    // embedding 是可选的 —— 非 Full 存储模式下传入空向量，此时必须整体省略
    // 该属性。空 F32Array 会被向量索引按 0 维处理并拒绝
    // （`Invalid vector dimension: expected N, got 0`），而缺失属性只是不入索引。
    if !embedding.is_empty() {
        props.push(json!(["embedding", prop_f32_array(embedding)]));
    }

    Value::Array(props)
}

/// 构建分块属性数组（v2 AddN `properties` 格式）。
fn chunk_props(chunk: &Chunk, ns: Option<&str>) -> Value {
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
    ];

    if let Some(ns) = ns {
        props.push(json!(["ns", prop_str(ns)]));
    }

    // embedding 是可选的 — 非 Full 存储模式（graph_only / text_only）下传入空
    // 向量，此时必须整体省略该属性。空 F32Array 会被向量索引按 0 维处理并拒绝
    // （`Invalid vector dimension: expected N, got 0`），而缺失属性只是不入索引。
    if !chunk.embedding.is_empty() {
        props.push(json!(["embedding", prop_f32_array(&chunk.embedding)]));
    }

    // page_number 是可选的 — 只在 Some 时加入
    if let Some(pn) = chunk.page_number {
        props.push(json!(["page_number", prop_i64(pn as i64)]));
    }

    Value::Array(props)
}

// ── 搜索过滤 Where 步骤 ──────────────────────────────────────────────────

/// 构建搜索管道的过滤步骤：`ns` 边界谓词与调用方过滤谓词合取。
///
/// `ns` 与过滤谓词都没有时不产步骤 —— 搜索构造器默认返回全量命中。
/// `file_types` 在 HelixDB 上无对应 AST 原语（镜像不支持 `IsIn`），且全仓库无
/// 生产者：静默丢弃它会让调用方拿到未过滤的结果，因此显式失败。
fn filter_step(
    schema: &HelixSchema,
    filters: Option<&SearchFilters>,
) -> Result<Option<Value>, KnowledgeError> {
    let mut predicates: Vec<Value> = eq_ns(schema.ns.as_deref()).into_iter().collect();

    if let Some(f) = filters {
        if let Some(ref doc_ids) = f.document_ids {
            if doc_ids.is_empty() {
                return Err(KnowledgeError::InvalidInput(
                    "document_ids 为空时无法构造过滤谓词".into(),
                ));
            }
            let mut id_predicates: Vec<Value> =
                doc_ids.iter().map(|id| eq_id("document_id", id)).collect();
            predicates.push(match id_predicates.len() {
                1 => id_predicates.swap_remove(0),
                _ => json!({"Or": id_predicates}),
            });
        }

        if f.file_types.is_some() {
            return Err(KnowledgeError::InvalidInput(
                "HelixDB 后端不支持 file_types 过滤".into(),
            ));
        }
    }

    Ok(match predicates.len() {
        0 => None,
        _ => Some(json!({"Where": and_all(predicates)})),
    })
}

/// 把过滤步骤追加进搜索管道，把 [`filter_step`] 的错误上抛给调用方。
fn push_filter_step(
    steps: &mut Vec<Value>,
    schema: &HelixSchema,
    filters: Option<&SearchFilters>,
) -> Result<(), KnowledgeError> {
    if let Some(step) = filter_step(schema, filters)? {
        steps.push(step);
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Write 操作
// ═══════════════════════════════════════════════════════════════════════════

/// 创建 Document 节点（v2 AddN 对象格式）。
///
/// 写响应投影的内部 `$id` 目前**无人消费**（`HelixDbBackend::store` 丢弃写入
/// 返回值，节点身份一律由调用方传入的 `Document.id` 决定）。保留原样，不做
/// 语义改写 —— 改它需要先确认没有外部观察者依赖该响应。
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
                        "properties": document_props(
                            doc,
                            metadata_json,
                            embedding,
                            schema.ns.as_deref()
                        )
                    }},
                    {"Project": [{"source": "$id", "alias": "id"}]}
                ], "condition": null}}
            ],
            "returns": ["doc"]
        }
    })
}

/// 创建 Chunk 节点（v2 AddN 对象格式）。
///
/// 同 [`create_document_node`]：写响应的 `$id` 投影无人消费，保留原样。
pub fn create_chunk_node(schema: &HelixSchema, chunk: &Chunk) -> Value {
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "chunk", "steps": [
                    {"AddN": {
                        "label": schema.fragment_node_label,
                        "properties": chunk_props(chunk, schema.ns.as_deref())
                    }},
                    {"Project": [{"source": "$id", "alias": "id"}]}
                ], "condition": null}}
            ],
            "returns": ["chunk"]
        }
    })
}

/// 构建节点属性数组（v2 AddN `properties` 格式）。
///
/// `id_property` 承载节点 ID；`properties` 中的其余条目原样写入，
/// 与 `id_property` 同名的键忽略 —— ID 是节点身份，不允许被普通属性覆盖。
/// `ns` 同理只认 schema：隔离键不能由调用方伪造，也不能重复写入。
fn node_props(schema: &HelixSchema, node_id: &str, properties: &HashMap<String, String>) -> Value {
    let ns = schema.ns.as_deref();
    let mut props: Vec<Value> = vec![json!([schema.id_property, prop_str(node_id)])];

    for (key, value) in properties {
        if key == &schema.id_property || (key.as_str() == "ns" && ns.is_some()) {
            continue;
        }
        props.push(json!([key, prop_str(value)]));
    }

    if let Some(ns) = ns {
        props.push(json!(["ns", prop_str(ns)]));
    }

    Value::Array(props)
}

/// 创建节点（v2 AddN 对象格式），供 `GraphStore::upsert_node` 使用。
///
/// HelixDB 的每个节点只有一个 label，一律取 `schema.graph_node_label`：
/// 与读侧（按 id 选点、遍历起点、边两端）同源，否则同 id 的节点写读错位，
/// `node_exists` 守卫漏选后重复建点。图谱节点只此一类，文档/分块各有专用构造器。
///
/// 写响应的 `$id` 投影同样无人消费（`upsert_node` 丢弃写入返回值），保留原样。
pub fn create_node(
    schema: &HelixSchema,
    node_id: &str,
    properties: &HashMap<String, String>,
) -> Value {
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "node", "steps": [
                    {"AddN": {
                        "label": schema.graph_node_label,
                        "properties": node_props(schema, node_id, properties)
                    }},
                    {"Project": [{"source": "$id", "alias": "id"}]}
                ], "condition": null}}
            ],
            "returns": ["node"]
        }
    })
}

/// 创建 Document → Chunk 的 CONTAINS 边（v2 AddE 对象格式）。
pub fn create_contains_edge(schema: &HelixSchema, doc_id: &str, chunk_id: &str) -> Value {
    let doc_match = node_by_id(schema, &schema.content_node_label, doc_id);
    let chunk_match = node_by_id(schema, &schema.fragment_node_label, chunk_id);
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "doc", "steps": [
                    {"NWhere": doc_match}
                ], "condition": null}},
                {"Query": {"name": "chunk", "steps": [
                    {"NWhere": chunk_match}
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
    let a_match = node_by_id(schema, &schema.fragment_node_label, chunk_a_id);
    let b_match = node_by_id(schema, &schema.fragment_node_label, chunk_b_id);
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "a", "steps": [
                    {"NWhere": a_match}
                ], "condition": null}},
                {"Query": {"name": "b", "steps": [
                    {"NWhere": b_match}
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

/// 创建一条带 weight 的边，两端按 `ns` + id 选点（与 label 无关）。
///
/// 端点 id 由调用方语义决定其 label（实体、文档……），本构造器无从判断，
/// 因此不能加 `$label` 谓词 —— 加错会选空端点，`AddE` 随即因无源顶点报错。
pub(crate) fn create_labeled_edge(
    schema: &HelixSchema,
    from_id: &str,
    to_id: &str,
    label: &str,
    weight: f64,
) -> Value {
    let from_match = node_by_ns_and_id(schema, from_id);
    let to_match = node_by_ns_and_id(schema, to_id);
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "src", "steps": [
                    {"NWhere": from_match}
                ], "condition": null}},
                {"Query": {"name": "tgt", "steps": [
                    {"NWhere": to_match}
                ], "condition": null}},
                {"Query": {"name": "edge", "steps": [
                    {"N": {"Var": "src"}},
                    {"AddE": {
                        "label": label,
                        "to": {"Var": "tgt"},
                        "properties": [["weight", prop_f64(weight)]]
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
    from_id: &str,
    to_id: &str,
    weight: f64,
) -> Value {
    create_labeled_edge(schema, from_id, to_id, &schema.related_edge, weight)
}

/// 级联删除文档及其所有分块和关联边。
pub fn delete_document_cascade(schema: &HelixSchema, doc_id: &str) -> Value {
    let doc_match = node_by_id(schema, &schema.content_node_label, doc_id);
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "doc", "steps": [
                    {"NWhere": doc_match}
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
    let chunk_match = node_by_id(schema, &schema.fragment_node_label, chunk_id);
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "dropped", "steps": [
                    {"NWhere": chunk_match},
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
    let chunk_match = node_by_id(schema, &schema.fragment_node_label, chunk_id);
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "updated", "steps": [
                    {"NWhere": chunk_match},
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
) -> Result<Value, KnowledgeError> {
    let steps = build_vector_search_steps(schema, query_vec, top_k, filters)?;
    Ok(json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "results", "steps": steps, "condition": null}}
            ],
            "returns": ["results"]
        }
    }))
}

/// 构建向量搜索步骤列表。
///
/// 向量检索不认过滤条件，命中集必须在检索步骤之后按 `ns` 与调用方过滤谓词收窄
/// 一次，否则跨库命中会直接进入结果。
fn build_vector_search_steps(
    schema: &HelixSchema,
    query_vec: &[f32],
    top_k: u32,
    filters: Option<&SearchFilters>,
) -> Result<Vec<Value>, KnowledgeError> {
    let mut steps: Vec<Value> = vec![json!({
        "VectorSearchNodes": {
            "label": schema.fragment_node_label,
            "property": schema.fragment_vector_property,
            "query_vector": {"Value": {"F32Array": vec_f32_to_f64(query_vec)}},
            "k": {"Literal": top_k}
        }
    })];

    push_filter_step(&mut steps, schema, filters)?;

    // `chunk_id` 这里是**内部** `$id`，刻意不取 `id_property`：它只在引擎内部
    // 流转，交给 `GraphStore::expand` 做 `N: {"Ids": [...]}` 定位，而 `Ids` 只
    // 认内部自增 id（内容哈希串会被 `expand_from_chunks` 解析时丢弃）。
    // `document_id` 则是分块节点上的属性，本来就是内容哈希口径。
    steps.push(json!({"Project": [
        {"source": "$id", "alias": "chunk_id"},
        {"source": "document_id", "alias": "document_id"},
        {"source": schema.fragment_text_property, "alias": "text"},
        {"source": "$distance", "alias": "score"}
    ]}));

    Ok(steps)
}

/// 全文搜索分块（v2 TextSearchNodes 对象格式）。
///
/// 命中集同样在检索步骤之后按 `ns` 与过滤谓词收窄。
pub fn text_search_chunks(
    schema: &HelixSchema,
    query_text: &str,
    top_k: u32,
    filters: Option<&SearchFilters>,
) -> Result<Value, KnowledgeError> {
    let mut steps: Vec<Value> = vec![json!({
        "TextSearchNodes": {
            "label": schema.fragment_node_label,
            "property": schema.fragment_text_property,
            "query_text": {"Value": {"String": query_text}},
            "k": {"Literal": top_k}
        }
    })];

    push_filter_step(&mut steps, schema, filters)?;

    // `chunk_id` 同为内部 `$id`，理由见 `build_vector_search_steps`。
    steps.push(json!({"Project": [
        {"source": "$id", "alias": "chunk_id"},
        {"source": "document_id", "alias": "document_id"},
        {"source": schema.fragment_text_property, "alias": "text"},
        {"source": "$distance", "alias": "score"}
    ]}));

    Ok(json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "results", "steps": steps, "condition": null}}
            ],
            "returns": ["results"]
        }
    }))
}

/// 按 ID 获取文档。
///
/// 投影的 `id` 取 `schema.id_property`（内容哈希）而非内置 `$id`，与
/// `delete_document_cascade` / `delete_chunk_by_id` 的匹配口径保持一致 ——
/// 否则读回来的 `Document.id` 是内部自增整数，回传给删除接口必然 `NotFound`。
pub fn get_document_by_id(schema: &HelixSchema, doc_id: &str) -> Value {
    let doc_match = node_by_id(schema, &schema.content_node_label, doc_id);
    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "doc", "steps": [
                    {"NWhere": doc_match},
                    {"Project": [
                        {"source": schema.id_property, "alias": "id"},
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
///
/// 投影的 `chunk_id` 取 `schema.id_property`（内容哈希）而非内置 `$id`。
/// 调用方（`IngestionPipeline::delete_document`）把这些 id 原样交给
/// `delete_chunk_by_id`，后者按 `id_property` 匹配 —— 投影内部 `$id` 会让
/// 向量/全文条目的清理静默落空。
///
/// 注意不要与 `vector_search_chunks` / `text_search_chunks` 的 `chunk_id` 混淆：
/// 那两处的 id 供 `GraphStore::expand` 做 `N: {"Ids": [...]}` 定位，**必须**
/// 是内部 `$id`。
pub fn get_document_chunks(schema: &HelixSchema, doc_id: &str) -> Value {
    let doc_match = node_by_id(schema, &schema.content_node_label, doc_id);
    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "doc", "steps": [
                    {"NWhere": doc_match}
                ], "condition": null}},
                {"Query": {"name": "chunks", "steps": [
                    {"N": {"Var": "doc"}},
                    {"Out": schema.contains_edge},
                    {"OrderBy": ["sequence_index", "Asc"]},
                    {"Project": [
                        {"source": schema.id_property, "alias": "chunk_id"},
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
///
/// 委托 [`list_documents_filtered`] 且 `source = None`。
pub fn list_documents(schema: &HelixSchema, offset: usize, limit: usize) -> Value {
    list_documents_filtered(schema, offset, limit, None)
}

/// 分页列出文档，可选按 `source_path` **精确**过滤（E2）。
///
/// 投影的 `id` 取 `schema.id_property`（内容哈希）而非 HelixDB 内置 `$id`，
/// 与 `get_document_by_id` / `delete_document_cascade` 的匹配口径保持一致 ——
/// 否则列表返回的 ID 无法回传给删除接口。
///
/// 首步骤按 `$label`（绑定命名空间时再加 `ns`）选文档，`source = Some(s)` 追加
/// `source_path` 等式。过滤在 `Skip`/`Limit` **之前**下推 ⇒ `offset` 是「过滤后的偏移」。
pub fn list_documents_filtered(
    schema: &HelixSchema,
    offset: usize,
    limit: usize,
    source: Option<&str>,
) -> Value {
    let mut predicates = vec![eq_label(&schema.content_node_label)];
    predicates.extend(eq_ns(schema.ns.as_deref()));
    if let Some(s) = source {
        predicates.push(eq_id("source_path", s));
    }
    let node_match = and_all(predicates);

    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "docs", "steps": [
                    {"NWhere": node_match},
                    {"Skip": offset},
                    {"Limit": limit},
                    {"Project": [
                        {"source": schema.id_property, "alias": "id"},
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

/// 全量扫描文档（E4）：取回至多 `limit` 条 `Document`（**含正文 + metadata**）。
///
/// 一次只读查询：`NWhere($label[, ns])` → `Limit` → `Project(id,title,source_path,content,metadata)`。
/// **不做**分页 `Skip`，投影含 `content`（同 `get_document_by_id` 的正文属性口径）——
/// 这是「中文全文索引不可用」的替代机制所需的取数原语。
pub fn list_documents_with_content(schema: &HelixSchema, limit: usize) -> Value {
    let doc_match = nodes_by_label(schema, &schema.content_node_label);
    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "docs", "steps": [
                    {"NWhere": doc_match},
                    {"Limit": limit},
                    {"Project": [
                        {"source": schema.id_property, "alias": "id"},
                        {"source": "title", "alias": "title"},
                        {"source": "source_path", "alias": "source_path"},
                        {"source": schema.content_text_property, "alias": "content"},
                        {"source": "metadata", "alias": "metadata"}
                    ]}
                ], "condition": null}}
            ],
            "returns": ["docs"]
        }
    })
}

/// 子图快照：一次 read batch 内并发取回某 label 的**节点集**与**边集**。
///
/// 这是「一次往返拿回 (nodes, edges)」的唯一构造 —— 任何拆成两条独立查询的
/// 方案都会让「节点集」与「边集」来自不同快照，无法保证子图闭合。
///
/// 形状（只读实测）：
/// 1. `nodes` —— `NWhere{$label=<label>[, ns]} → Project($id, id_property, name, entity_type)`：
///    同时投影内部 `$id`（关联键）与稳定 `id_property`（对外身份）；
/// 2. `edges` —— `NWhere{$label=<label>[, ns]} → OutE:null → EdgeProperties:null`：
///    从**全部命中节点**扇出（`NWhere` 多命中的既定语义），返回每条出边的
///    `$label` 与端点内部 `$id`。
///
/// `EdgeProperties` 是终结步骤且不能在边流上 `Project`，故边的「标签 + 端点稳定
/// id」必须靠调用方按内部 `$id` 关联两份结果 —— 与 `labeled_edge_batch` 同一技巧。
/// 只发 `OutE` 一条：子图闭合要求两端皆该 label 的节点，而 `OutE` 从全部该 label
/// 节点扇出即已覆盖全部两端命中的边，`InE` 冗余。
pub fn list_subgraph(schema: &HelixSchema, label: &str) -> Value {
    let node_match = nodes_by_label(schema, label);
    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "nodes", "steps": [
                    {"NWhere": node_match},
                    {"Project": [
                        {"source": "$id", "alias": "internal_id"},
                        {"source": schema.id_property, "alias": "node_id"},
                        {"source": "name", "alias": "name"},
                        {"source": "entity_type", "alias": "entity_type"}
                    ]}
                ], "condition": null}},
                {"Query": {"name": "edges", "steps": [
                    {"NWhere": node_match},
                    {"OutE": null},
                    {"EdgeProperties": null}
                ], "condition": null}}
            ],
            "returns": ["nodes", "edges"]
        }
    })
}

/// 按 ID 获取节点（v2 投影格式）。
///
/// 投影 `id_property` 而非内置 `$id`，与 `upsert_node` 写入的 ID 口径一致。
/// 仅投影 `name` —— HelixDB 的投影需要显式列名，无法在此投影未知的属性集合。
///
/// `$label` 取 `schema.graph_node_label`：与写侧同源，否则 id 相同也会读不到。
pub fn get_node_by_id(schema: &HelixSchema, node_id: &str) -> Value {
    let node_match = graph_node_by_id(schema, node_id);
    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "node", "steps": [
                    {"NWhere": node_match},
                    {"Limit": 1},
                    {"Project": [
                        {"source": schema.id_property, "alias": "id"},
                        {"source": "$label", "alias": "label"},
                        {"source": "name", "alias": "name"}
                    ]}
                ], "condition": null}}
            ],
            "returns": ["node"]
        }
    })
}

/// 按 ID 统计节点数量 —— `node_exists` 的存在性探针。
///
/// 用 Count 而非投影，避免为了判断存在而取回节点属性。
///
/// `$label` 同 [`get_node_by_id`]，取 `schema.graph_node_label`。
pub fn count_node_by_id(schema: &HelixSchema, node_id: &str) -> Value {
    let node_match = graph_node_by_id(schema, node_id);
    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "count", "steps": [
                    {"NWhere": node_match},
                    {"Count": null}
                ], "condition": null}}
            ],
            "returns": ["count"]
        }
    })
}

/// 统计节点数量（v2: NWithLabel → NWhere + $label 等式），只数当前命名空间。
pub fn count_nodes(schema: &HelixSchema, label: &str) -> Value {
    let node_match = nodes_by_label(schema, label);
    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "count", "steps": [
                    {"NWhere": node_match},
                    {"Count": null}
                ], "condition": null}}
            ],
            "returns": ["count"]
        }
    })
}

/// 图遍历：从起始节点做 BFS，**按跳数分层**返回（`d1`..`dN`）。
///
/// `edge_label` 为 `None` 时发出通配遍历（`{"Out": null}` 形态），沿**全部**
/// 边走出，不管边标签是什么。这是唯一能覆盖 `EdgeType::Custom` 任意谓词标签的
/// 形式 —— 自定义边的标签由谓词文本决定，枚举不出来。
///
/// 注意空串**不是**通配：HelixDB 把 `""` 当作字面标签匹配，只会得到空结果。
///
/// 结果投影 `schema.id_property`（稳定身份）而非内置 `$id` —— 否则调用方拿到的
/// 是内部自增整数，无法与 `compute_entity_id` 的内容哈希对齐。`name` 一并投影，
/// 供 `query_entity_facts` 展示实体名。
///
/// ## 为什么分层
///
/// HelixDB 的 `$distance` 是**搜索命中专用**的虚拟字段，遍历结果不返回它；
/// `Path` / `SimplePath` 也只回扁平 id 列表，拿不到逐跳结构。因此精确跳数只能在
/// 客户端推：第 k 层 = `max_depth = k` 的累积遍历结果，节点**首次出现的层号**就是
/// 它到起点的最短跳数。所有层装在**同一个 read batch** 里，不增加 HTTP 往返次数，
/// 代价是同批 N 条子查询（N = `max_depth`，实际通常 2~3）。
///
/// 起点按 `$label`（`schema.graph_node_label`）+ `ns` + id 选中，避免同 id 的
/// 跨库节点被一起遍历。
///
/// 起点内联在每条层查询里（不再单发 `start` 查询）；`emit: "All"` 会把起点自己
/// 也放进每一层，由解析侧按 `start_node_id` 过滤。
pub fn traverse_graph(
    schema: &HelixSchema,
    start_node_id: &str,
    edge_label: Option<&str>,
    direction: &str, // "Out", "In", "Both"
    max_depth: u32,
) -> Value {
    let label = match edge_label {
        Some(l) => json!(l),
        None => Value::Null,
    };
    let dir_step: Value = match direction {
        "In" => json!({"In": label}),
        "Both" => json!({"Both": label}),
        _ => json!({"Out": label}),
    };

    let start_match = graph_node_by_id(schema, start_node_id);
    let mut queries = Vec::with_capacity(max_depth as usize);
    let mut returns = Vec::with_capacity(max_depth as usize);
    for depth in 1..=max_depth {
        let name = depth_query_name(depth);
        returns.push(name.clone());
        queries.push(json!({"Query": {"name": name, "steps": [
            {"NWhere": start_match.clone()},
            {"Repeat": {
                "traversal": {"steps": [dir_step.clone()]},
                "max_depth": depth,
                "emit": "All"
            }},
            "Dedup",
            {"Project": [
                {"source": schema.id_property, "alias": "node_id"},
                {"source": "$label", "alias": "label"},
                {"source": "name", "alias": "name"}
            ]}
        ], "condition": null}}));
    }

    json!({
        "request_type": "read",
        "query": {"queries": queries, "returns": returns}
    })
}

/// 分层遍历查询的返回键名：第 `depth` 层叫 `d{depth}`（`d1`、`d2`……）。
///
/// 构造侧（[`traverse_graph`]）与解析侧（后端 `parse_traversed_nodes`）共用同一
/// 函数，避免两处字面量各自漂移。
pub fn depth_query_name(depth: u32) -> String {
    format!("d{depth}")
}

/// 起始节点的直连边 + 邻接点身份（把边标签与稳定节点 id 一起取回）。
///
/// HelixDB 不允许在边流上做 `Project`（`project() requires node state`），而
/// `EdgeProperties` 又是终结步骤 —— 没有任何单条查询能同时给出「边标签」和
/// 「邻接点稳定 id + name」。因此这里在一次 read batch 里发两条查询，各自覆盖
/// 同一批直连边，再由调用方按内部 `$id` 关联：
///
/// 1. `edges` —— `N(start) → {edge_step}(null) → EdgeProperties`，返回每条边的
///    `$label` 与端点内部 `$id`；
/// 2. `nodes` —— `N(start) → {edge_step}(null) → {node_step}(null) → Project`，
///    返回邻接点的内部 `$id` + `schema.id_property`（稳定 id）+ `name`。
///
/// 关联键是 `edges` 的端点内部 id（`$to`/`$from`）== `nodes` 的 `internal_id`，
/// 与返回行序无关，不依赖 HelixDB 的行序稳定性。
///
/// `edge_step` 只接受 `"OutE"` / `"InE"`，`node_step` 对应 `"OutN"` / `"InN"`：
/// 邻接点分别是目标端点（`OutE→OutN`，`$to`）与源端点（`InE→InN`，`$from`）。
pub fn adjacent_labeled_nodes(
    schema: &HelixSchema,
    start_node_id: &str,
    edge_step: &str,
    node_step: &str,
) -> Value {
    labeled_edge_batch(schema, start_node_id, edge_step, node_step, None)
}

/// [`adjacent_labeled_nodes`] 的通用形态：可选边标签的「边流 + 节点流」双查询。
///
/// `edge_label` 为 `None` 时走通配（`{edge_step: null}`），覆盖全部标签；
/// 为 `Some(l)` 时只取该标签的边。**标签是精确字符串匹配，无任何规范化** ——
/// 谓词边（`EdgeType::Custom`）的标签就是谓词原文，大小写敏感。
///
/// 供 `GraphStore::edges_between` 做存在性检查：调用方按
/// 「`edges.$to`/`$from` == `nodes.internal_id`」关联两个查询，再按
/// `nodes.node_id`（稳定身份）筛选出目标对端。
pub fn edges_between_nodes(
    schema: &HelixSchema,
    start_node_id: &str,
    edge_step: &str,
    node_step: &str,
    edge_label: Option<&str>,
) -> Value {
    labeled_edge_batch(schema, start_node_id, edge_step, node_step, edge_label)
}

/// 构建「起点 → 邻接边 + 邻接点身份」的双查询（v2 read batch）。
///
/// 形状与 [`adjacent_labeled_nodes`] 的历史实现逐字节一致，两者共用一份，
/// 避免新增构造器时把已验证的查询形状抄歪。
fn labeled_edge_batch(
    schema: &HelixSchema,
    start_node_id: &str,
    edge_step: &str,
    node_step: &str,
    edge_label: Option<&str>,
) -> Value {
    let start_match = graph_node_by_id(schema, start_node_id);
    let label = match edge_label {
        Some(l) => json!(l),
        None => Value::Null,
    };
    json!({
        "request_type": "read",
        "query": {
            "queries": [
                {"Query": {"name": "start", "steps": [
                    {"NWhere": start_match}
                ], "condition": null}},
                {"Query": {"name": "edges", "steps": [
                    {"N": {"Var": "start"}},
                    {edge_step: label.clone()},
                    {"EdgeProperties": null}
                ], "condition": null}},
                {"Query": {"name": "nodes", "steps": [
                    {"N": {"Var": "start"}},
                    {edge_step: label},
                    {node_step: null},
                    {"Project": [
                        {"source": "$id", "alias": "internal_id"},
                        {"source": schema.id_property, "alias": "node_id"},
                        {"source": "name", "alias": "name"}
                    ]}
                ], "condition": null}}
            ],
            "returns": ["edges", "nodes"]
        }
    })
}

// ── 图删除 ────────────────────────────────────────────────────────────────

/// 删除 `from_id` 沿 `label` 指向 `to_id` 的一条边（同一标签的并行边**全部**删除）。
///
/// 这是 HelixDB 上唯一「单轮往返、且无需预先知道任何内部 id」的删边形式：
/// `s` / `t` 都由 `NWhere` 按 `$label`（`schema.graph_node_label`）+ `ns` + 稳定内容
/// 哈希 id 选中，与 `compute_entity_id` 直接对接。
///
/// ## 为什么不用写响应的 `Count`
///
/// `DropEdgeLabeled` 作用在**节点状态**上 —— 它从流经的节点上摘掉匹配的边，再把
/// **节点**继续往下传。因此其后的 `Count` 数的是**流经的源节点数**（源存在 = 1，
/// 源不存在 = 0），与删除的边数完全无关；边已不存在时重放同一请求仍返回 1。
/// 删除数只能由调用方「删前先读」得出。
///
/// ## 方向性
///
/// 只匹配**出边**（`OutE` 语义）。删反向边必须把 `from_id` / `to_id` 对调再发一次。
///
/// ## payload 形状
///
/// `DropEdgeLabeled` 的 `to` 必须是 `{"Var": ...}` 节点引用枚举，裸值会 400；
/// 且它**不能**作为源步骤（`Invalid source step`），前面必须先是 `N(Var)`。
/// 目标不存在时是静默 no-op（不会退化成通配误删）。
pub fn drop_edge_between(schema: &HelixSchema, from_id: &str, to_id: &str, label: &str) -> Value {
    let from_match = graph_node_by_id(schema, from_id);
    let to_match = graph_node_by_id(schema, to_id);
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "s", "steps": [
                    {"NWhere": from_match}
                ], "condition": null}},
                {"Query": {"name": "t", "steps": [
                    {"NWhere": to_match}
                ], "condition": null}},
                {"Query": {"name": "d", "steps": [
                    {"N": {"Var": "s"}},
                    {"DropEdgeLabeled": {"label": label, "to": {"Var": "t"}}},
                    {"Count": null}
                ], "condition": null}}
            ],
            "returns": ["d"]
        }
    })
}

/// 删除单个节点（`NWhere($label + ns + id) → Drop`）。
///
/// HelixDB 的 `Drop` **自动级联删除该节点双向的全部关联边**，无需先删边；响应
/// `count` 恒为 `0`（节点被删后流为空），因此删除数同样只能由调用方「删前先读」
/// 得出 —— 见 [`drop_edge_between`] 的论证。
///
/// `NWhere` 命中同 id 的多个重复顶点时会**全部**删除（`AddN` 无 MERGE 语义，
/// 同 id 多顶点是已知现象）。
pub fn remove_node_by_id(schema: &HelixSchema, node_id: &str) -> Value {
    let node_match = graph_node_by_id(schema, node_id);
    json!({
        "request_type": "write",
        "query": {
            "queries": [
                {"Query": {"name": "d", "steps": [
                    {"NWhere": node_match},
                    {"Drop": null},
                    {"Count": null}
                ], "condition": null}}
            ],
            "returns": ["d"]
        }
    })
}

/// 图扩展：从分块 ID 出发，找父文档，再做关联扩展。
///
/// 分块 ID 是 HelixDB 内部 `$id`（自增整数，以数字字符串形式传入）。
/// 使用 `N: {"Ids": [id1, id2, ...]}` 一步选中所有分块节点，
/// 然后沿 CONTAINS 入边找到各自的父文档，再做图扩展。
///
/// `In` 之后的流不继承命中行的过滤条件，父文档集必须再按 `ns` 滤一次。
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
    ];
    steps.extend(ns_where_step(schema));
    steps.push(json!({"Dedup": null}));

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

    // 投影的 `document_id` 取 `schema.id_property`（内容哈希）而非内置 `$id`：
    // 该字段会经 `SearchResult.document_id` 流到工具面，再被 `get_document_by_id`
    // / `delete_document_cascade` 按 `id_property` 匹配。投影内部 `$id` 会让
    // 「搜索命中 → 删除」这条链路报 `Document not found: '<内部自增 id>'`。
    steps.push(json!({"Project": [
        {"source": schema.id_property, "alias": "document_id"},
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
pub fn combined_search_query(
    schema: &HelixSchema,
    query: &CombinedQuery,
) -> Result<Value, KnowledgeError> {
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

    push_filter_step(&mut vector_steps, schema, query.filters.as_ref())?;

    // 投影命中分块（`chunk_id` 仅作「这一行是命中行」的判别位，取内部 `$id`
    // 与本路径其余分块 id 口径一致）
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

    push_filter_step(&mut text_steps, schema, query.filters.as_ref())?;

    // 投影命中分块（同上，`chunk_id` 仅作判别位）
    text_steps.push(json!({"Project": [
        {"source": "$id", "alias": "chunk_id"},
        {"source": "document_id", "alias": "document_id"},
        {"source": schema.fragment_text_property, "alias": "text"},
        {"source": "$distance", "alias": "score"}
    ]}));

    // In CONTAINS → 找父文档 → 图扩展
    add_graph_expansion_steps(&mut text_steps, schema, graph_depth, related, belongs);

    Ok(json!({
        "request_type": "read",
        "query_name": "combined_search",
        "query": {
            "queries": [
                {"Query": {"name": "vector_path", "steps": vector_steps, "condition": null}},
                {"Query": {"name": "text_path", "steps": text_steps, "condition": null}}
            ],
            "returns": ["vector_path", "text_path"]
        }
    }))
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
    // `In` 之后的流不继承命中行的过滤条件，父文档集必须再按 `ns` 滤一次
    steps.extend(ns_where_step(schema));
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

    // 投影文档字段（包含 graph_distance 用于区分图扩展结果）。
    // `document_id` 同样取 `schema.id_property`：与命中行（chunk 行投影的
    // `document_id` 属性）口径一致，才能在与命中行一起进 RRF 融合、再被
    // `get_document_by_id` 取回时不落空。
    steps.push(json!({"Project": [
        {"source": schema.id_property, "alias": "document_id"},
        {"source": "title", "alias": "title"},
        {"source": schema.content_text_property, "alias": "content"},
        {"source": "source_path", "alias": "source_path"},
        {"source": "$distance", "alias": "graph_distance"}
    ]}));
}

// ═══════════════════════════════════════════════════════════════════════════
// 构造器名册
// ═══════════════════════════════════════════════════════════════════════════

/// 产 AST（`Value`）的查询构造器 —— 每个都必须带 `ns` 隔离谓词或 `ns` 节点属性。
///
/// 单测据此逐构造器断言，并与源码里的 `^pub fn` 双向对齐：新增、改名或漏登记
/// 构造器都会让 T1 变红。
#[cfg(test)]
pub const QUERY_CONSTRUCTORS: &[&str] = &[
    "create_document_node",
    "create_chunk_node",
    "create_node",
    "create_contains_edge",
    "create_next_chunk_edge",
    "create_related_to_edge",
    "delete_document_cascade",
    "delete_chunk_by_id",
    "update_chunk_embedding",
    "vector_search_chunks",
    "text_search_chunks",
    "get_document_by_id",
    "get_document_chunks",
    "list_documents",
    "list_documents_filtered",
    "list_documents_with_content",
    "list_subgraph",
    "get_node_by_id",
    "count_node_by_id",
    "count_nodes",
    "traverse_graph",
    "adjacent_labeled_nodes",
    "edges_between_nodes",
    "drop_edge_between",
    "remove_node_by_id",
    "expand_from_chunks",
    "combined_search_query",
];

/// 不产 AST 的公开函数。
#[cfg(test)]
pub const NON_QUERY_CONSTRUCTORS: &[&str] = &["depth_query_name"];

/// 允许不带 `ns` 的函数及理由 —— 恰好等于 [`NON_QUERY_CONSTRUCTORS`]。
#[cfg(test)]
pub const NO_NS_REASON: &[(&str, &str)] = &[(
    "depth_query_name",
    "只返回层名 `d{n}` 字符串，不产查询、不触库",
)];

// ═══════════════════════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::combined_search::RrfConfig;
    use crate::types::ChunkMetadata;

    fn test_schema() -> HelixSchema {
        HelixSchema::default()
    }

    #[test]
    fn build_vector_search_query() {
        let q = vector_search_chunks(&test_schema(), &[0.1, 0.2, 0.3], 10, None).unwrap();
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
        let q = text_search_chunks(&test_schema(), "test query", 5, None).unwrap();
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
        let q = combined_search_query(&test_schema(), &cq).unwrap();
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
        let q = combined_search_query(&test_schema(), &cq).unwrap();
        let vsteps = q["query"]["queries"][0]["Query"]["steps"]
            .as_array()
            .unwrap();

        // 应包含 Repeat 步骤（图扩展）
        let has_repeat = vsteps
            .iter()
            .any(|s| s.as_object().is_some_and(|o| o.contains_key("Repeat")));
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

    /// 结构边（`CONTAINS` / `NEXT_CHUNK`）两端 label 不同，端点选点必须与 label 无关：
    /// 带上 `$label` 会把另一端的节点选空，`AddE` 随即报「requires at least one
    /// source vertex」。
    #[test]
    fn node_by_ns_and_id_ignores_label() {
        assert_eq!(
            node_by_ns_and_id(&scoped_schema(), "doc-1"),
            json!({"And": [
                {"Eq": ["ns", {"String": SENTINEL_NS}]},
                {"Eq": ["id", {"String": "doc-1"}]}
            ]})
        );

        // 未绑定命名空间时退化为单谓词，与隔离前的形状一致。
        assert_eq!(
            node_by_ns_and_id(&test_schema(), "doc-1"),
            json!({"Eq": ["id", {"String": "doc-1"}]})
        );
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
        let p = prop_f64(3.5);
        assert!((p["Value"]["F64"].as_f64().unwrap() - 3.5).abs() < 1e-10);

        // prop_i64
        let p = prop_i64(42);
        assert_eq!(p["Value"]["I64"], 42);
    }

    #[test]
    fn filter_with_document_ids() {
        let filters = SearchFilters {
            kb_id: None,
            document_ids: Some(vec!["doc-a".into(), "doc-b".into()]),
            file_types: None,
        };
        let step = filter_step(&test_schema(), Some(&filters))
            .expect("多元素只是 Or，不是错误")
            .expect("有过滤谓词就应有 Where");
        assert_eq!(
            step,
            json!({"Where": {"Or": [
                {"Eq": ["document_id", {"String": "doc-a"}]},
                {"Eq": ["document_id", {"String": "doc-b"}]}
            ]}})
        );
    }

    #[test]
    fn filter_none_returns_none() {
        assert!(filter_step(&test_schema(), None).unwrap().is_none());
    }

    #[test]
    fn filter_empty_returns_none() {
        let filters = SearchFilters::default();
        assert!(
            filter_step(&test_schema(), Some(&filters))
                .unwrap()
                .is_none()
        );
    }

    /// ④ 空 embedding（graph_only / text_only 存储模式）必须整体省略该属性 ——
    /// 写入空 F32Array 会被向量索引按 0 维拒绝。
    #[test]
    fn empty_embedding_is_omitted_from_props() {
        let s = test_schema();

        // 分块：空 embedding → 无 embedding 属性
        let mut c = Chunk {
            id: "doc-0001-0000-abcd".into(),
            document_id: "doc-0001".into(),
            text: "无向量的分块".into(),
            sequence_index: 0,
            page_number: Some(3),
            embedding: Vec::new(),
            metadata: ChunkMetadata::default(),
        };
        let q = create_chunk_node(&s, &c);
        let props = q["query"]["queries"][0]["Query"]["steps"][0]["AddN"]["properties"]
            .as_array()
            .unwrap()
            .clone();
        assert!(
            !props.iter().any(|p| p[0] == "embedding"),
            "空 embedding 不应写入 embedding 属性，实际 props: {props:?}"
        );
        // 其余可选属性不受影响
        assert!(props.iter().any(|p| p[0] == "page_number"));

        // 分块：非空 embedding → 照常写入
        c.embedding = vec![0.1, 0.2, 0.3];
        let q = create_chunk_node(&s, &c);
        let props = q["query"]["queries"][0]["Query"]["steps"][0]["AddN"]["properties"]
            .as_array()
            .unwrap()
            .clone();
        let emb = props
            .iter()
            .find(|p| p[0] == "embedding")
            .expect("应写入 embedding");
        assert_eq!(emb[1]["Value"]["F32Array"].as_array().unwrap().len(), 3);

        // 文档：同理
        let doc = Document {
            id: "doc-0001".into(),
            kb_id: None,
            title: "无向量文档".into(),
            source_path: "/tmp/test.txt".into(),
            content: "hello".into(),
            metadata: Default::default(),
        };
        let q = create_document_node(&s, &doc, "{}", &[]);
        let props = q["query"]["queries"][0]["Query"]["steps"][0]["AddN"]["properties"]
            .as_array()
            .unwrap()
            .clone();
        assert!(
            !props.iter().any(|p| p[0] == "embedding"),
            "空 embedding 不应写入 embedding 属性，实际 props: {props:?}"
        );
    }

    /// ① v2 `list_documents_filtered(Some)`：首步骤 `NWhere.And` =
    /// `[Eq:$label, Eq:source_path]`，随后 `Skip`/`Limit`/`Project`，`Project[0].source=="id"`。
    #[test]
    fn list_documents_filtered_with_source_uses_and_pushdown() {
        let s = test_schema();
        let q = list_documents_filtered(&s, 5, 10, Some("ppa_profile"));
        let steps = q["query"]["queries"][0]["Query"]["steps"]
            .as_array()
            .unwrap();

        let and = steps[0]["NWhere"]["And"]
            .as_array()
            .expect("Some(source) ⇒ NWhere.And");
        assert_eq!(and.len(), 2, "And 恰两条等式");
        assert_eq!(
            and[0],
            json!({"Eq": ["$label", {"String": s.content_node_label}]})
        );
        assert_eq!(
            and[1],
            json!({"Eq": ["source_path", {"String": "ppa_profile"}]})
        );

        // 过滤在 Skip/Limit 之前下推
        assert_eq!(steps[1]["Skip"], 5);
        assert_eq!(steps[2]["Limit"], 10);
        let project = steps[3]["Project"].as_array().unwrap();
        assert_eq!(project[0]["source"], "id");
        assert_eq!(project[0]["alias"], "id");
    }

    /// ② v2 回归：`list_documents`（无 source）首步骤为 `NWhere.Eq:$label`，
    /// **无** `And` 包裹，且与 `list_documents_filtered(.., None)` 逐字相同。
    #[test]
    fn list_documents_without_source_has_no_and() {
        let s = test_schema();
        let q = list_documents(&s, 0, 10);
        let steps = q["query"]["queries"][0]["Query"]["steps"]
            .as_array()
            .unwrap();

        let nwhere = &steps[0]["NWhere"];
        assert!(
            nwhere.get("And").is_none(),
            "无 source 不得出现 And：{nwhere:?}"
        );
        assert_eq!(nwhere["Eq"], json!(["$label", {"String": "Document"}]));

        assert_eq!(
            q,
            list_documents_filtered(&s, 0, 10, None),
            "list_documents 必须逐字委托 filtered(None)"
        );
    }

    /// ③ v2 `list_documents_with_content`：`NWhere.Eq:$label` → `Limit` →
    /// `Project` 含 `content`/`metadata`，`Project[0].source=="id"`，无 `Skip`。
    #[test]
    fn list_documents_with_content_projects_content() {
        let s = test_schema();
        let q = list_documents_with_content(&s, 2001);
        let steps = q["query"]["queries"][0]["Query"]["steps"]
            .as_array()
            .unwrap();

        assert_eq!(
            steps[0]["NWhere"],
            json!({"Eq": ["$label", {"String": s.content_node_label}]})
        );
        assert_eq!(steps[1]["Limit"], 2001);
        assert!(
            steps.iter().all(|st| st.get("Skip").is_none()),
            "扫描不分页，不得含 Skip"
        );

        let project = steps[2]["Project"].as_array().unwrap();
        assert_eq!(project[0]["source"], "id");
        assert_eq!(project[0]["alias"], "id");
        let aliases: Vec<&str> = project
            .iter()
            .map(|p| p["alias"].as_str().unwrap())
            .collect();
        assert!(aliases.contains(&"content"), "必须投影正文：{aliases:?}");
        assert!(
            aliases.contains(&"metadata"),
            "必须投影 metadata：{aliases:?}"
        );
        assert_eq!(
            project.iter().find(|p| p["alias"] == "content").unwrap()["source"],
            s.content_text_property
        );
    }

    /// ③ 列表返回的 `id` 必须与 get / delete 的匹配口径一致（`id_property`），
    /// 否则「列表 → 删除」链路断裂。
    #[test]
    fn list_documents_projects_schema_id_property() {
        let s = test_schema();
        let q = list_documents(&s, 0, 10);
        let steps = q["query"]["queries"][0]["Query"]["steps"]
            .as_array()
            .unwrap();
        let project = steps
            .iter()
            .find_map(|st| st.as_object().and_then(|o| o.get("Project")))
            .expect("list_documents 应包含 Project 步骤");
        assert_eq!(
            project[0]["source"], "id",
            "列表应投影 schema.id_property 而非内置 $id"
        );
        assert_eq!(project[0]["alias"], "id");

        // 自定义 id_property 时跟随 schema
        let custom = HelixSchema {
            id_property: "doc_id".into(),
            ..HelixSchema::default()
        };
        let q = list_documents(&custom, 0, 10);
        let project = q["query"]["queries"][0]["Query"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|st| st.as_object().and_then(|o| o.get("Project")))
            .unwrap()
            .clone();
        assert_eq!(project[0]["source"], "doc_id");
    }

    /// ① `upsert_node` 的建节点查询：v2 AddN 对象格式、label 取自 schema、
    /// `id_property` 承载节点 ID。
    #[test]
    fn create_node_uses_v2_addn_with_id_property() {
        let s = test_schema();
        let props = HashMap::from([("name".to_string(), "小C".to_string())]);
        let q = create_node(&s, "entity:Entity:abcd1234", &props);
        assert_eq!(q["request_type"], "write");

        let addn = &q["query"]["queries"][0]["Query"]["steps"][0]["AddN"];
        assert!(addn.is_object(), "AddN 应为 v2 对象格式");
        assert_eq!(addn["label"], "Entity");

        let pairs = addn["properties"].as_array().unwrap();
        let id_prop = pairs
            .iter()
            .find(|p| p[0] == s.id_property)
            .expect("节点必须写入 id_property");
        assert_eq!(id_prop[1]["Value"]["String"], "entity:Entity:abcd1234");
        let name = pairs
            .iter()
            .find(|p| p[0] == "name")
            .expect("节点必须写入 name");
        assert_eq!(name[1]["Value"]["String"], "小C");
    }

    /// ① label 与读侧同源：`create_node` 恒取 `schema.graph_node_label`。
    ///
    /// 把 label 写死成常量（如 `"Entity"`）会让自定义 label 的 schema 写读错位，
    /// 本断言正是该回归的守卫。
    #[test]
    fn create_node_label_comes_from_schema() {
        let s = HelixSchema {
            graph_node_label: "Person".into(),
            ..HelixSchema::default()
        };
        let q = create_node(&s, "entity:Person:abcd1234", &HashMap::new());
        let addn = &q["query"]["queries"][0]["Query"]["steps"][0]["AddN"];
        assert_eq!(addn["label"], "Person");
    }

    /// ① 属性里出现与 `id_property` 同名的键时，不允许覆盖节点身份。
    #[test]
    fn create_node_id_property_cannot_be_overridden() {
        let s = test_schema();
        let props = HashMap::from([
            ("id".to_string(), "伪造的-id".to_string()),
            ("name".to_string(), "小C".to_string()),
        ]);
        let q = create_node(&s, "entity:Entity:abcd1234", &props);
        let pairs = q["query"]["queries"][0]["Query"]["steps"][0]["AddN"]["properties"]
            .as_array()
            .unwrap()
            .clone();

        let id_pairs: Vec<_> = pairs.iter().filter(|p| p[0] == s.id_property).collect();
        assert_eq!(id_pairs.len(), 1, "id_property 只应出现一次");
        assert_eq!(id_pairs[0][1]["Value"]["String"], "entity:Entity:abcd1234");
    }

    /// ① `get_node` / `node_exists` 的读查询按 `id_property` 匹配。
    #[test]
    fn get_and_count_node_match_by_id_property() {
        let s = test_schema();

        let q = get_node_by_id(&s, "entity:Entity:abcd1234");
        assert_eq!(q["request_type"], "read");
        let steps = q["query"]["queries"][0]["Query"]["steps"]
            .as_array()
            .unwrap();
        assert_eq!(
            steps[0]["NWhere"],
            json!({"And": [
                {"Eq": ["$label", {"String": "Entity"}]},
                {"Eq": ["id", {"String": "entity:Entity:abcd1234"}]}
            ]}),
            "应按 id_property 匹配而非内置 $id，且 label 与写侧同源"
        );
        let project = steps
            .iter()
            .find_map(|st| st.as_object().and_then(|o| o.get("Project")))
            .expect("get_node_by_id 应包含 Project 步骤");
        assert_eq!(project[0]["source"], "id");

        let q = count_node_by_id(&s, "entity:Entity:abcd1234");
        let steps = q["query"]["queries"][0]["Query"]["steps"]
            .as_array()
            .unwrap();
        assert_eq!(
            steps[0]["NWhere"],
            json!({"And": [
                {"Eq": ["$label", {"String": "Entity"}]},
                {"Eq": ["id", {"String": "entity:Entity:abcd1234"}]}
            ]})
        );
        assert!(
            steps
                .iter()
                .any(|st| st.as_object().is_some_and(|o| o.contains_key("Count"))),
            "node_exists 应用 Count 而非投影"
        );
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

    /// ⑦ 分层遍历批次形状：`returns` 为 `d1..dN`，第 k 条的 `max_depth` 恰为 k。
    #[test]
    fn traverse_graph_is_layered_by_depth() {
        let s = HelixSchema::default();
        let q = traverse_graph(&s, "entity:Entity:abc", None, "Both", 3);

        assert_eq!(q["request_type"], "read");
        assert_eq!(q["query"]["returns"], json!(["d1", "d2", "d3"]));

        let queries = q["query"]["queries"].as_array().unwrap();
        assert_eq!(queries.len(), 3, "层数 = max_depth");
        for (idx, depth) in (1..=3u32).enumerate() {
            let query = &queries[idx];
            assert_eq!(query["Query"]["name"], depth_query_name(depth));
            assert_eq!(
                query["Query"]["steps"][0],
                json!({"NWhere": {"And": [
                    {"Eq": ["$label", {"String": "Entity"}]},
                    {"Eq": ["id", {"String": "entity:Entity:abc"}]}
                ]}})
            );
            assert_eq!(query["Query"]["steps"][1]["Repeat"]["max_depth"], depth);
        }
    }

    /// `list_subgraph` 的 AST 形状 —— 一个 read batch、两个子查询、闭包所需的两份投影。
    ///
    /// 与只读实测逐字比对：`nodes` = `NWhere{$label} → Project($id, id, name, entity_type)`；
    /// `edges` = `NWhere{$label} → OutE:null → EdgeProperties:null`。形状漂移会先在这里炸。
    #[test]
    fn list_subgraph_shape() {
        let s = HelixSchema::default();
        let q = list_subgraph(&s, "Entity");

        assert_eq!(q["request_type"], "read");
        // 一个 batch（不是两次往返）
        let batch = q["query"]["queries"].as_array().unwrap();
        assert_eq!(batch.len(), 2, "一个 batch 内并发 nodes + edges");
        assert_eq!(q["query"]["returns"], json!(["nodes", "edges"]));

        // nodes：NWhere{$label=Entity} → Project($id, id_property, name, entity_type)
        let nodes = &batch[0]["Query"];
        assert_eq!(nodes["name"], "nodes");
        let nsteps = nodes["steps"].as_array().unwrap();
        assert_eq!(
            nsteps[0],
            json!({"NWhere": {"Eq": ["$label", {"String": "Entity"}]}})
        );
        // 身份字段名硬编码为字面量 "id"（而非 `s.id_property`）：实测形状
        // 就是 `id`，且 `id_property` 默认即 "id" —— 用同一表达式断言会恒真、锁不住
        // 「稳定 id(=id) ≠ 内部 $id」这条最易混的口径。改动 schema 默认即在此炸。
        assert_eq!(
            nsteps[1],
            json!({"Project": [
                {"source": "$id", "alias": "internal_id"},
                {"source": "id", "alias": "node_id"},
                {"source": "name", "alias": "name"},
                {"source": "entity_type", "alias": "entity_type"}
            ]}),
            "同时投影内部 $id（join 键）与稳定身份字段 id（对外身份）"
        );

        // edges：NWhere{$label=Entity} → OutE:null → EdgeProperties:null
        let edges = &batch[1]["Query"];
        assert_eq!(edges["name"], "edges");
        let esteps = edges["steps"].as_array().unwrap();
        assert_eq!(
            esteps.len(),
            3,
            "OutE 通配 + EdgeProperties 终结，边流上无 Project"
        );
        assert_eq!(
            esteps[0],
            json!({"NWhere": {"Eq": ["$label", {"String": "Entity"}]}})
        );
        assert_eq!(esteps[1], json!({"OutE": null}), "null 才是通配，空串不是");
        assert_eq!(esteps[2], json!({"EdgeProperties": null}));
    }
    // ── T1：逐构造器隔离断言 ────────────────────────────────────────────────

    /// T1 的哨兵命名空间：非空，且与任何默认值都不同。
    const SENTINEL_NS: &str = "sentinel::ns";

    fn scoped_schema() -> HelixSchema {
        HelixSchema {
            ns: Some(SENTINEL_NS.into()),
            ..HelixSchema::default()
        }
    }

    fn sample_doc() -> Document {
        Document {
            id: "doc-1".into(),
            kb_id: None,
            title: "标题".into(),
            source_path: "ppa_profile".into(),
            content: "正文".into(),
            metadata: Default::default(),
        }
    }

    fn sample_chunk() -> Chunk {
        Chunk {
            id: "chunk-1".into(),
            document_id: "doc-1".into(),
            text: "分块".into(),
            sequence_index: 0,
            page_number: None,
            embedding: vec![0.1],
            metadata: ChunkMetadata::default(),
        }
    }

    fn sample_combined_query() -> CombinedQuery {
        CombinedQuery {
            query_text: "q".into(),
            query_vector: vec![0.1, 0.2],
            vector_top_k: 5,
            text_top_k: 5,
            graph_expansion_depth: 1,
            graph_edge_types: vec![],
            fusion: RrfConfig::default(),
            filters: None,
        }
    }

    /// `ns` 应当落在 AST 的哪一类位置上。
    #[derive(Clone, Copy)]
    enum NsPlace {
        /// 每个 `NWhere` 的谓词都要含 `ns`（按 id / 按 label 选点）。
        NWhere,
        /// 这些标记步骤之后要出现带 `ns` 的 `Where`。
        After(&'static [&'static str]),
        /// `AddN` 的节点属性里要含 `ns`（写入类）。
        Property,
    }

    struct Fixture {
        name: &'static str,
        /// 每个元素是该构造器的一种输入形态（两分支的构造器给出两条）。
        build: fn(&HelixSchema) -> Vec<Value>,
        ns_place: NsPlace,
        /// 期望出现在 `Eq $label` 里的节点标签；空 ⇒ 该构造器不按 `$label` 选点。
        labels: &'static [&'static str],
    }

    /// 逐构造器 fixture —— [`fixtures_cover_every_constructor`] 保证与名册一一对应。
    fn fixtures() -> Vec<Fixture> {
        vec![
            Fixture {
                name: "create_document_node",
                build: |s| vec![create_document_node(s, &sample_doc(), "{}", &[0.1, 0.2])],
                ns_place: NsPlace::Property,
                labels: &[],
            },
            Fixture {
                name: "create_chunk_node",
                build: |s| vec![create_chunk_node(s, &sample_chunk())],
                ns_place: NsPlace::Property,
                labels: &[],
            },
            Fixture {
                name: "create_node",
                build: |s| {
                    vec![create_node(
                        s,
                        "entity:Entity:abc",
                        &HashMap::from([("name".to_string(), "小C".to_string())]),
                    )]
                },
                ns_place: NsPlace::Property,
                labels: &[],
            },
            Fixture {
                name: "create_contains_edge",
                build: |s| vec![create_contains_edge(s, "doc-1", "chunk-1")],
                ns_place: NsPlace::NWhere,
                labels: &["Document", "Chunk"],
            },
            Fixture {
                name: "create_next_chunk_edge",
                build: |s| vec![create_next_chunk_edge(s, "chunk-1", "chunk-2")],
                ns_place: NsPlace::NWhere,
                labels: &["Chunk"],
            },
            Fixture {
                name: "create_related_to_edge",
                build: |s| vec![create_related_to_edge(s, "doc-1", "doc-2", 0.5)],
                ns_place: NsPlace::NWhere,
                labels: &[],
            },
            Fixture {
                name: "delete_document_cascade",
                build: |s| vec![delete_document_cascade(s, "doc-1")],
                ns_place: NsPlace::NWhere,
                labels: &["Document"],
            },
            Fixture {
                name: "delete_chunk_by_id",
                build: |s| vec![delete_chunk_by_id(s, "chunk-1")],
                ns_place: NsPlace::NWhere,
                labels: &["Chunk"],
            },
            Fixture {
                name: "update_chunk_embedding",
                build: |s| vec![update_chunk_embedding(s, "chunk-1", &[0.1])],
                ns_place: NsPlace::NWhere,
                labels: &["Chunk"],
            },
            Fixture {
                name: "vector_search_chunks",
                build: |s| vec![vector_search_chunks(s, &[0.1, 0.2], 5, None).unwrap()],
                ns_place: NsPlace::After(&["VectorSearchNodes"]),
                labels: &[],
            },
            Fixture {
                name: "text_search_chunks",
                build: |s| vec![text_search_chunks(s, "q", 5, None).unwrap()],
                ns_place: NsPlace::After(&["TextSearchNodes"]),
                labels: &[],
            },
            Fixture {
                name: "get_document_by_id",
                build: |s| vec![get_document_by_id(s, "doc-1")],
                ns_place: NsPlace::NWhere,
                labels: &["Document"],
            },
            Fixture {
                name: "get_document_chunks",
                build: |s| vec![get_document_chunks(s, "doc-1")],
                ns_place: NsPlace::NWhere,
                labels: &["Document"],
            },
            Fixture {
                name: "list_documents",
                build: |s| vec![list_documents(s, 0, 10)],
                ns_place: NsPlace::NWhere,
                labels: &["Document"],
            },
            Fixture {
                name: "list_documents_filtered",
                build: |s| {
                    vec![
                        list_documents_filtered(s, 0, 10, None),
                        list_documents_filtered(s, 0, 10, Some("ppa_profile")),
                    ]
                },
                ns_place: NsPlace::NWhere,
                labels: &["Document"],
            },
            Fixture {
                name: "list_documents_with_content",
                build: |s| vec![list_documents_with_content(s, 100)],
                ns_place: NsPlace::NWhere,
                labels: &["Document"],
            },
            Fixture {
                name: "list_subgraph",
                build: |s| vec![list_subgraph(s, "Entity")],
                ns_place: NsPlace::NWhere,
                labels: &["Entity"],
            },
            Fixture {
                name: "get_node_by_id",
                build: |s| vec![get_node_by_id(s, "entity:Entity:abc")],
                ns_place: NsPlace::NWhere,
                labels: &["Entity"],
            },
            Fixture {
                name: "count_node_by_id",
                build: |s| vec![count_node_by_id(s, "entity:Entity:abc")],
                ns_place: NsPlace::NWhere,
                labels: &["Entity"],
            },
            Fixture {
                name: "count_nodes",
                build: |s| vec![count_nodes(s, "Document")],
                ns_place: NsPlace::NWhere,
                labels: &["Document"],
            },
            Fixture {
                name: "traverse_graph",
                build: |s| vec![traverse_graph(s, "entity:Entity:abc", None, "Both", 2)],
                ns_place: NsPlace::NWhere,
                labels: &["Entity"],
            },
            Fixture {
                name: "adjacent_labeled_nodes",
                build: |s| {
                    vec![adjacent_labeled_nodes(
                        s,
                        "entity:Entity:abc",
                        "OutE",
                        "OutN",
                    )]
                },
                ns_place: NsPlace::NWhere,
                labels: &["Entity"],
            },
            Fixture {
                name: "edges_between_nodes",
                build: |s| {
                    vec![edges_between_nodes(
                        s,
                        "entity:Entity:abc",
                        "OutE",
                        "OutN",
                        None,
                    )]
                },
                ns_place: NsPlace::NWhere,
                labels: &["Entity"],
            },
            Fixture {
                name: "drop_edge_between",
                build: |s| {
                    vec![drop_edge_between(
                        s,
                        "entity:Entity:a",
                        "entity:Entity:b",
                        "朋友",
                    )]
                },
                ns_place: NsPlace::NWhere,
                labels: &["Entity"],
            },
            Fixture {
                name: "remove_node_by_id",
                build: |s| vec![remove_node_by_id(s, "entity:Entity:a")],
                ns_place: NsPlace::NWhere,
                labels: &["Entity"],
            },
            Fixture {
                name: "expand_from_chunks",
                build: |s| vec![expand_from_chunks(s, &["7".into()], &["rel".into()], 1)],
                ns_place: NsPlace::After(&["In"]),
                labels: &[],
            },
            Fixture {
                name: "combined_search_query",
                build: |s| vec![combined_search_query(s, &sample_combined_query()).unwrap()],
                ns_place: NsPlace::After(&["VectorSearchNodes", "TextSearchNodes", "In"]),
                labels: &[],
            },
        ]
    }

    /// 谓词本身（`Eq` 直连或 `And` 元素）是否就是哨兵 `ns` 等式。
    fn predicate_has_ns(predicate: &Value) -> bool {
        let is_ns_eq = predicate
            .get("Eq")
            .and_then(Value::as_array)
            .is_some_and(|eq| {
                eq.len() == 2 && eq[0] == "ns" && eq[1] == json!({"String": SENTINEL_NS})
            });
        if is_ns_eq {
            return true;
        }
        predicate
            .get("And")
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(predicate_has_ns))
    }

    fn collect_key<'a>(value: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
        match value {
            Value::Object(map) => {
                for (k, v) in map {
                    if k == key {
                        out.push(v);
                    }
                    collect_key(v, key, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect_key(item, key, out)),
            _ => {}
        }
    }

    fn check_every_nwhere_has_ns(name: &str, ast: &Value) -> Result<(), String> {
        let mut nwheres = Vec::new();
        collect_key(ast, "NWhere", &mut nwheres);
        if nwheres.is_empty() {
            return Err(format!("{name}: AST 里没有 NWhere"));
        }
        for nwhere in nwheres {
            if !predicate_has_ns(nwhere) {
                return Err(format!("{name}: NWhere 缺 ns 谓词: {nwhere}"));
            }
        }
        Ok(())
    }

    fn check_ns_after_markers(name: &str, ast: &Value, markers: &[&str]) -> Result<(), String> {
        let mut arrays = Vec::new();
        collect_key(ast, "steps", &mut arrays);

        let mut checked = 0;
        for array in arrays {
            let steps = array
                .as_array()
                .ok_or_else(|| format!("{name}: steps 不是数组: {array}"))?;
            for (index, step) in steps.iter().enumerate() {
                if !markers.iter().any(|marker| step.get(*marker).is_some()) {
                    continue;
                }
                let has_ns_after = steps[index + 1..]
                    .iter()
                    .any(|later| later.get("Where").is_some_and(predicate_has_ns));
                if !has_ns_after {
                    return Err(format!("{name}: {markers:?} 之后缺 ns 过滤步骤: {steps:?}"));
                }
                checked += 1;
            }
        }

        if checked == 0 {
            return Err(format!("{name}: AST 里没有 {markers:?} 步骤"));
        }
        Ok(())
    }

    fn assert_ns_property(name: &str, ast: &Value) -> Result<(), String> {
        let mut add_nodes = Vec::new();
        collect_key(ast, "AddN", &mut add_nodes);
        if add_nodes.is_empty() {
            return Err(format!("{name}: AST 里没有 AddN"));
        }
        for add_node in add_nodes {
            let props = add_node
                .get("properties")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{name}: AddN 缺 properties"))?;
            let has_ns = props.iter().any(|pair| {
                pair.as_array().is_some_and(|kv| {
                    kv.len() == 2 && kv[0] == "ns" && kv[1]["Value"]["String"] == SENTINEL_NS
                })
            });
            if !has_ns {
                return Err(format!("{name}: 节点属性缺 ns: {props:?}"));
            }
        }
        Ok(())
    }

    fn assert_ns_present(name: &str, ast: &Value, place: NsPlace) -> Result<(), String> {
        match place {
            NsPlace::NWhere => check_every_nwhere_has_ns(name, ast),
            NsPlace::After(markers) => check_ns_after_markers(name, ast, markers),
            NsPlace::Property => assert_ns_property(name, ast),
        }
    }

    fn assert_labels(name: &str, ast: &Value, expected: &[&str]) -> Result<(), String> {
        let mut eqs = Vec::new();
        collect_key(ast, "Eq", &mut eqs);
        let found: Vec<&str> = eqs
            .iter()
            .filter_map(|eq| eq.as_array())
            .filter(|eq| eq.len() == 2 && eq[0] == "$label")
            .filter_map(|eq| eq[1]["String"].as_str())
            .collect();

        if expected.is_empty() {
            return if found.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "{name}: 该类构造器不按 $label 选点，却出现 {found:?}"
                ))
            };
        }
        for label in expected {
            if !found.contains(label) {
                return Err(format!("{name}: 期望 $label {expected:?}，实际 {found:?}"));
            }
        }
        Ok(())
    }

    /// `["ns", …]` 形态的谓词或属性项（`Eq` 的两元素、属性二元组共用此形状）。
    fn is_ns_term(value: &Value) -> bool {
        value
            .as_array()
            .is_some_and(|pair| pair.len() == 2 && pair[0] == "ns")
    }

    /// 剥掉 AST 里的 `ns` 谓词与 `ns` 属性 —— 等价于「源码里删掉该构造器的 ns」。
    fn strip_ns(value: &Value) -> Value {
        match value {
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .filter(|item| !is_ns_term(item))
                    .map(strip_ns)
                    .collect(),
            ),
            Value::Object(map) => Value::Object(
                map.iter()
                    .filter(|(_, v)| !is_ns_term(v))
                    .map(|(k, v)| (k.clone(), strip_ns(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// fixture 表必须与名册一一对应，否则「逐构造器断言」会漏项。
    #[test]
    fn fixtures_cover_every_constructor() {
        let mut names: Vec<&str> = fixtures().iter().map(|f| f.name).collect();
        names.sort_unstable();

        let mut roster = QUERY_CONSTRUCTORS.to_vec();
        roster.sort_unstable();

        assert_eq!(names, roster, "fixture 必须覆盖且只覆盖产 AST 的构造器");
    }

    /// T1：正向断言每个构造器都带 `ns`（位置按类别）且 `$label` 正确；
    /// 反向断言「剥掉 ns 后同一检查器必红」—— 即删任一处 `Eq ns` 都会让 T1 变红。
    #[test]
    fn scoped_constructors_carry_namespace() {
        let schema = scoped_schema();

        for fixture in fixtures() {
            let asts = (fixture.build)(&schema);
            assert!(!asts.is_empty(), "{} 没有 fixture 形态", fixture.name);
            for (index, ast) in asts.into_iter().enumerate() {
                let case = format!("{}#{index}", fixture.name);
                if let Err(e) = assert_ns_present(&case, &ast, fixture.ns_place) {
                    panic!("{e}");
                }
                if let Err(e) = assert_labels(&case, &ast, fixture.labels) {
                    panic!("{e}");
                }

                let stripped = strip_ns(&ast);
                assert_ne!(stripped, ast, "{case}: AST 里根本没有 ns 谓词/属性");
                assert!(
                    assert_ns_present(&case, &stripped, fixture.ns_place).is_err(),
                    "{case}: 剥掉 ns 后检查器仍通过 —— 该断言锁不住 ns"
                );
            }
        }
    }

    /// 名册 ↔ 源码 `^pub fn` 双向对齐：漏登记或幽灵项都会红。
    #[test]
    fn constructor_rosters_match_source() {
        assert_eq!(QUERY_CONSTRUCTORS.len(), 27, "产 AST 的构造器基数");

        let source = include_str!("queries.rs");
        let mut declared: Vec<&str> = source
            .lines()
            .filter_map(|line| line.strip_prefix("pub fn "))
            .map(|rest| rest.split('(').next().unwrap_or_default().trim())
            .collect();
        declared.sort_unstable();

        let mut registered: Vec<&str> = QUERY_CONSTRUCTORS
            .iter()
            .chain(NON_QUERY_CONSTRUCTORS)
            .copied()
            .collect();
        registered.sort_unstable();

        assert_eq!(declared, registered);
    }

    /// 豁免面必须显式且最小：只有不产 AST 的函数可以不带 `ns`。
    #[test]
    fn no_ns_exemptions_are_explicit() {
        let exempted: Vec<&str> = NO_NS_REASON.iter().map(|(name, _)| *name).collect();
        assert_eq!(exempted, NON_QUERY_CONSTRUCTORS);
        for name in QUERY_CONSTRUCTORS {
            assert!(!exempted.contains(name), "{name} 不得豁免 ns 断言");
        }
        for (name, reason) in NO_NS_REASON {
            assert!(!reason.is_empty(), "{name} 的豁免理由不能为空");
        }
    }

    /// T2：过滤谓词必须是等值/析取 —— `IsIn` 会让整条查询在镜像上 400。
    #[test]
    fn filter_step_uses_equality_not_isin() {
        let schema = test_schema();

        let single = SearchFilters {
            document_ids: Some(vec!["doc-a".into()]),
            ..SearchFilters::default()
        };
        let step = filter_step(&schema, Some(&single))
            .unwrap()
            .expect("单元素 ⇒ Eq 谓词");
        assert_eq!(
            step,
            json!({"Where": {"Eq": ["document_id", {"String": "doc-a"}]}})
        );

        let multi = SearchFilters {
            document_ids: Some(vec!["a".into(), "b".into()]),
            ..SearchFilters::default()
        };
        let step = filter_step(&schema, Some(&multi)).unwrap().unwrap();
        assert!(
            !step.to_string().contains("IsIn"),
            "产物不得含 IsIn: {step}"
        );
        assert_eq!(
            step,
            json!({"Where": {"Or": [
                {"Eq": ["document_id", {"String": "a"}]},
                {"Eq": ["document_id", {"String": "b"}]}
            ]}})
        );
    }

    /// 空的 `document_ids` 是调用方的 bug（「筛 0 个文档」应直接不查），
    /// 返回 `{"Or": []}` 会被 HelixDB 视为恒真/非法谓词，须显式失败。
    #[test]
    fn filter_step_fails_loud_on_empty_document_ids() {
        let filters = SearchFilters {
            document_ids: Some(vec![]),
            ..SearchFilters::default()
        };
        let err = filter_step(&test_schema(), Some(&filters)).unwrap_err();
        assert!(matches!(err, KnowledgeError::InvalidInput(_)), "{err:?}");
    }

    /// `file_types` 没有任何可用的 AST 原语，必须显式失败而不是静默丢弃。
    #[test]
    fn filter_step_fails_loud_on_file_types() {
        let filters = SearchFilters {
            file_types: Some(vec!["md".into()]),
            ..SearchFilters::default()
        };
        let err = filter_step(&test_schema(), Some(&filters)).unwrap_err();
        assert!(matches!(err, KnowledgeError::InvalidInput(_)), "{err:?}");
    }

    /// 搜索/图扩展的过滤步骤里，`ns` 与调用方过滤谓词合取成一个 `Where`。
    #[test]
    fn filter_step_merges_namespace_with_filters() {
        let schema = scoped_schema();

        assert_eq!(
            filter_step(&schema, None).unwrap(),
            Some(json!({"Where": {"Eq": ["ns", {"String": SENTINEL_NS}]}}))
        );

        let filters = SearchFilters {
            document_ids: Some(vec!["a".into()]),
            ..SearchFilters::default()
        };
        assert_eq!(
            filter_step(&schema, Some(&filters)).unwrap(),
            Some(json!({"Where": {"And": [
                {"Eq": ["ns", {"String": SENTINEL_NS}]},
                {"Eq": ["document_id", {"String": "a"}]}
            ]}}))
        );
    }

    /// 写入类构造器把 `ns` 落成节点属性（而非谓词），未绑定命名空间时不写。
    #[test]
    fn write_constructors_embed_namespace_property() {
        let scoped = scoped_schema();
        let plain = test_schema();

        let scoped_props =
            create_document_node(&scoped, &sample_doc(), "{}", &[])["query"]["queries"][0]["Query"]
                ["steps"][0]["AddN"]["properties"]
                .as_array()
                .unwrap()
                .clone();
        assert!(scoped_props.iter().any(|p| p[0] == "ns"));

        let plain_props =
            create_document_node(&plain, &sample_doc(), "{}", &[])["query"]["queries"][0]["Query"]
                ["steps"][0]["AddN"]["properties"]
                .as_array()
                .unwrap()
                .clone();
        assert!(
            !plain_props.iter().any(|p| p[0] == "ns"),
            "未绑定命名空间的 schema 不得写入 ns: {plain_props:?}"
        );
    }

    /// 调用方自带的 `ns` 属性不得覆盖 schema 的命名空间（隔离键不可伪造）。
    #[test]
    fn create_node_namespace_property_cannot_be_overridden() {
        let scoped = scoped_schema();
        let props = HashMap::from([("ns".to_string(), "伪造::ns".to_string())]);
        let q = create_node(&scoped, "entity:Entity:abc", &props);
        let pairs = q["query"]["queries"][0]["Query"]["steps"][0]["AddN"]["properties"]
            .as_array()
            .unwrap()
            .clone();

        let ns_pairs: Vec<_> = pairs.iter().filter(|p| p[0] == "ns").collect();
        assert_eq!(ns_pairs.len(), 1, "ns 只应出现一次: {pairs:?}");
        assert_eq!(ns_pairs[0][1]["Value"]["String"], SENTINEL_NS);
    }
}
