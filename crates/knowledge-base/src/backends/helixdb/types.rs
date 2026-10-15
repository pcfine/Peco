//! HelixDB Schema 配置类型。
//!
//! 定义知识图谱的节点标签、边标签和索引目标，
//! 使 `HelixDbBackend` 能适应不同的 AI Agent 场景。

use crate::error::KnowledgeError;

// ---------------------------------------------------------------------------
// IndexType
// ---------------------------------------------------------------------------

/// HelixDB 支持的索引类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexType {
    /// 相等索引。
    Equality,
    /// 带唯一性约束的相等索引。
    UniqueEquality,
    /// 升序范围索引。
    Range,
    /// 降序范围索引。
    RangeDesc,
    /// 向量 ANN 索引。
    Vector,
    /// 全文 BM25 索引。
    Text,
}

// ---------------------------------------------------------------------------
// HelixIndexSpec
// ---------------------------------------------------------------------------

/// 额外索引规格 — 用于 `HelixSchema::extra_indexes`。
///
/// 注意：`fragment_vector_property`、`fragment_text_property`、
/// `content_vector_property` 和 `content_text_property` 的核心索引
/// 由 `HelixSchema` 内置字段自动创建，不需要在这里重复声明。
#[derive(Debug, Clone)]
pub struct HelixIndexSpec {
    pub index_type: IndexType,
    pub node_label: String,
    pub property: String,
}

// ---------------------------------------------------------------------------
// HelixSchema
// ---------------------------------------------------------------------------

/// HelixDB 知识图谱的 Schema 配置。
///
/// 每个 AI Agent 场景定义自己的节点类型、边类型和索引目标。
/// 一个 `HelixDbBackend` 绑定一个 `HelixSchema`。
///
/// # 示例
///
/// ## 文档 RAG（默认）
///
/// ```ignore
/// HelixSchema::default()
/// ```
///
/// ## 代码知识库
///
/// ```ignore
/// HelixSchema {
///     content_node_label: "Module".into(),
///     fragment_node_label: "Function".into(),
///     fragment_vector_property: "signature_embedding".into(),
///     fragment_text_property: "docstring".into(),
///     contains_edge: "DEFINES".into(),
///     related_edge: "CALLS".into(),
///     ..HelixSchema::default()
/// }
/// ```
#[derive(Debug, Clone)]
pub struct HelixSchema {
    /// 内容节点的标签名（默认 `"Document"`）。
    pub content_node_label: String,
    /// 片段节点的标签名（默认 `"Chunk"`）。
    pub fragment_node_label: String,

    /// 内容节点上的向量索引属性名（默认 `"embedding"`）。
    pub content_vector_property: String,
    /// 片段节点上的向量索引属性名（CombinedSearch 主索引，默认 `"embedding"`）。
    pub fragment_vector_property: String,
    /// 片段节点上的全文索引属性名（默认 `"text"`）。
    pub fragment_text_property: String,
    /// 内容节点上的全文索引属性名（默认 `"content"`）。
    pub content_text_property: String,

    /// 内容 → 片段的包含边标签（默认 `"CONTAINS"`）。
    pub contains_edge: String,
    /// 片段 → 下一个片段的顺序边标签（默认 `"NEXT_CHUNK"`）。
    pub next_fragment_edge: String,
    /// 内容 ↔ 内容的相关边标签（默认 `"RELATED_TO"`）。
    pub related_edge: String,
    /// 内容 → 分类/主题的归属边标签（默认 `"BELONGS_TO"`）。
    pub belongs_to_edge: String,

    /// 用于文档标识的 ID 属性名（默认 `"$id"`，使用 HelixDB 内置 ID）。
    ///
    /// 当设置为自定义属性（如 `"doc_id"`）时，需要该属性上建有相等索引。
    pub id_property: String,

    /// 额外索引列表（除了上述内置索引之外的自定义索引）。
    pub extra_indexes: Vec<HelixIndexSpec>,

    /// 行级命名空间（`{owner_scope}::{kb_name}`）。
    ///
    /// `Some` ⇒ 每条查询都带 `ns` 谓词 / 每个节点都写 `ns` 属性，隔离才成立；
    /// `None`（`Default`）⇒ 不产 `ns` 谓词，仅测试与本地连通性检查走这条路。
    pub ns: Option<String>,
    /// 图谱（实体）节点的 label。
    ///
    /// 写侧（`upsert_node`）与读侧（按 id 选点、遍历起点、边两端）必须同源，
    /// 否则 id 相同的节点会因 label 不一致而被漏选，出现重复 `AddN`。
    pub graph_node_label: String,
}

impl Default for HelixSchema {
    fn default() -> Self {
        Self {
            content_node_label: "Document".into(),
            fragment_node_label: "Chunk".into(),
            content_vector_property: "embedding".into(),
            fragment_vector_property: "embedding".into(),
            fragment_text_property: "text".into(),
            content_text_property: "content".into(),
            contains_edge: "CONTAINS".into(),
            next_fragment_edge: "NEXT_CHUNK".into(),
            related_edge: "RELATED_TO".into(),
            belongs_to_edge: "BELONGS_TO".into(),
            id_property: "id".into(),
            extra_indexes: vec![],
            ns: None,
            graph_node_label: crate::types::ENTITY_NODE_LABEL.into(),
        }
    }
}

impl HelixSchema {
    /// 构造绑定单个知识库命名空间的 schema。
    ///
    /// 命名空间键的拼装与校验统一走 [`crate::types::namespace_for`]：
    /// 隔离维度由单一实现决定，schema 与实体 id 口径不会各拼一套而漂移。
    pub fn for_namespace(scope: &str, kb_name: &str) -> Result<Self, KnowledgeError> {
        Ok(Self {
            ns: Some(crate::types::namespace_for(scope, kb_name)?),
            ..Self::default()
        })
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_schema_is_document_rag() {
        let s = HelixSchema::default();
        assert_eq!(s.content_node_label, "Document");
        assert_eq!(s.fragment_node_label, "Chunk");
        assert_eq!(s.contains_edge, "CONTAINS");
        assert_eq!(s.related_edge, "RELATED_TO");
        assert!(s.extra_indexes.is_empty());
        assert!(s.ns.is_none(), "默认 schema 不绑定命名空间");
        assert_eq!(s.graph_node_label, "Entity");
    }

    #[test]
    fn for_namespace_scopes_to_scope_and_kb() {
        let s = HelixSchema::for_namespace(" user-1 ", "工作记录").unwrap();
        assert_eq!(s.ns.as_deref(), Some("user-1::工作记录"));
        assert_eq!(s.graph_node_label, "Entity", "其余字段沿用默认");

        for blank in ["", "   ", "\t"] {
            let err = HelixSchema::for_namespace(blank, "工作记录").unwrap_err();
            assert!(matches!(err, KnowledgeError::InvalidInput(_)), "{err:?}");
        }
    }

    #[test]
    fn custom_schema_code_knowledge() {
        let s = HelixSchema {
            content_node_label: "Module".into(),
            fragment_node_label: "Function".into(),
            fragment_vector_property: "signature_embedding".into(),
            fragment_text_property: "docstring".into(),
            contains_edge: "DEFINES".into(),
            related_edge: "CALLS".into(),
            next_fragment_edge: String::new(),
            ..HelixSchema::default()
        };
        assert_eq!(s.fragment_node_label, "Function");
        assert_eq!(s.contains_edge, "DEFINES");
        assert_eq!(s.related_edge, "CALLS");
        assert!(s.next_fragment_edge.is_empty());
    }

    #[test]
    fn extra_indexes() {
        let mut s = HelixSchema::default();
        s.extra_indexes.push(HelixIndexSpec {
            index_type: IndexType::UniqueEquality,
            node_label: "Document".into(),
            property: "title".into(),
        });
        assert_eq!(s.extra_indexes.len(), 1);
        assert_eq!(s.extra_indexes[0].index_type, IndexType::UniqueEquality);
    }
}
