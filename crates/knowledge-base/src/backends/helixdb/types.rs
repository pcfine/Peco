//! HelixDB Schema 配置类型。
//!
//! 定义知识图谱的节点标签、边标签和索引目标，
//! 使 `HelixDbBackend` 能适应不同的 AI Agent 场景。

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
        }
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
