use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 类型别名
// ---------------------------------------------------------------------------

/// 文档 ID — 通过 SHA-256 前缀进行内容寻址。
pub type DocumentId = String;

// ---------------------------------------------------------------------------
// 文档与分块
// ---------------------------------------------------------------------------

/// 一条已摄入的知识文档。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: DocumentId,
    /// 所属知识库 ID（None 表示全局文档）。
    pub kb_id: Option<String>,
    pub title: String,
    /// 原始文件路径或 URL。
    pub source_path: String,
    /// 全文内容。
    pub content: String,
    pub metadata: DocumentMetadata,
}

/// 文档级元数据。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocumentMetadata {
    pub author: Option<String>,
    /// ISO 8601 格式。
    pub created_at: Option<String>,
    /// 例如 "pdf"、"md"、"txt"。
    pub file_type: Option<String>,
    pub page_count: Option<u32>,
    pub language: Option<String>,
    /// 扩展字段。
    #[serde(flatten)]
    pub extra: HashMap<String, String>,
}

/// `list` 返回的轻量级文档摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSummary {
    pub id: DocumentId,
    pub title: String,
    pub source_path: String,
    pub chunk_count: usize,
    pub file_type: Option<String>,
}

impl From<&Document> for DocumentSummary {
    fn from(doc: &Document) -> Self {
        Self {
            id: doc.id.clone(),
            title: doc.title.clone(),
            source_path: doc.source_path.clone(),
            chunk_count: 0, // 由存储层填充
            file_type: doc.metadata.file_type.clone(),
        }
    }
}

/// 文本分块 — 最小的检索单元。
///
/// 分块 ID 是确定性计算的，因此相同内容的重新摄入是幂等的。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub document_id: DocumentId,
    pub text: String,
    /// 在文档中的位置（从 0 开始）。
    pub sequence_index: u32,
    /// 来源页码（用于 PDF 等分页格式）。
    pub page_number: Option<u32>,
    /// 语义嵌入向量 — 由摄入管道填充。
    pub embedding: Vec<f32>,
    pub metadata: ChunkMetadata,
}

/// 每个分块的元数据。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChunkMetadata {
    /// 分块在父文档中的起始字节偏移。
    pub start_char: Option<usize>,
    /// 分块在父文档中的结束字节偏移。
    pub end_char: Option<usize>,
    /// 层级标题路径，例如 "第1章 > 第1.2节"。
    pub heading_path: Option<String>,
}

// ---------------------------------------------------------------------------
// 实体（知识图谱）
// ---------------------------------------------------------------------------

/// 由 LLM 从文本中提取的概念节点（第二阶段知识图谱）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// 确定性 ID：`entity:{类型}:{标准化名称哈希}`。
    pub id: String,
    pub name: String,
    /// 例如 "Person"、"Technology"、"Organization"。
    pub entity_type: String,
    /// 提取该实体的来源分块。
    pub source_chunk_id: String,
    /// 提取置信度 0.0–1.0。
    pub confidence: f32,
    pub properties: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// 知识事实（结构化三元组）
// ---------------------------------------------------------------------------

/// 知识事实 — 结构化三元组，用于存储离散知识。
///
/// 适合表示对话中提取的用户偏好、事件、关系等。
/// 每条 Fact 映射为图谱中的 Entity→Entity 边。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    /// 确定性 ID：`fact:{sha256(subject+predicate+object)[:8]}`
    pub id: String,
    /// 主体实体名称（如 "用户"、"张三"、"技术部"）
    pub subject: String,
    /// 谓词 / 关系类型（如 "prefers"、"works_for"、"has_skill"）
    pub predicate: String,
    /// 客体实体名称或字面量值
    pub object: String,
    /// 置信度 0.0–1.0
    pub confidence: f32,
    /// 来源标识（对话轮次 ID、文档 ID 等）
    #[serde(default)]
    pub source: String,
    /// 时间戳 ISO 8601
    #[serde(default)]
    pub created_at: String,
    /// 扩展属性
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

impl Fact {
    /// 计算确定性 Fact ID。
    pub fn compute_id(subject: &str, predicate: &str, object: &str) -> String {
        use sha2::Digest;
        let input = format!("{subject}|{predicate}|{object}");
        let hash = sha2::Sha256::digest(input.as_bytes());
        format!("fact:{}", hex::encode(&hash[..8]))
    }

    /// 创建新的 Fact，自动生成 ID。
    pub fn new(
        subject: impl Into<String>,
        predicate: impl Into<String>,
        object: impl Into<String>,
        confidence: f32,
    ) -> Self {
        let subject = subject.into();
        let predicate = predicate.into();
        let object = object.into();
        let id = Self::compute_id(&subject, &predicate, &object);
        Self {
            id,
            subject,
            predicate,
            object,
            confidence,
            source: String::new(),
            created_at: String::new(),
            metadata: HashMap::new(),
        }
    }
}

/// 计算确定性实体节点 ID。
///
/// 格式：`entity:{entity_type}:{sha256(normalized_name)[:8]}`
pub fn compute_entity_id(entity_name: &str, entity_type: &str) -> String {
    use sha2::Digest;
    let normalized = entity_name.trim().to_lowercase();
    let input = format!("{entity_type}:{normalized}");
    let hash = sha2::Sha256::digest(input.as_bytes());
    format!("entity:{}:{}", entity_type, hex::encode(&hash[..8]))
}

// ---------------------------------------------------------------------------
// 搜索类型
// ---------------------------------------------------------------------------

/// 发送到知识模块的搜索请求。
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub top_k: usize,
    pub strategy: SearchStrategy,
    pub filters: Option<SearchFilters>,
    /// 最低置信度阈值 — 低于此值的结果将被丢弃。
    /// Phase 1 预留字段（暂不参与过滤逻辑），Phase 2 启用。
    pub min_confidence: Option<ConfidenceLevel>,
}

/// 要组合哪些检索策略。
#[derive(Debug, Clone)]
pub enum SearchStrategy {
    VectorOnly,
    TextOnly,
    GraphOnly {
        start_node_ids: Vec<String>,
    },
    Hybrid {
        vector_weight: f32,
        text_weight: f32,
    },
    FullHybrid {
        vector_weight: f32,
        text_weight: f32,
        graph_weight: f32,
        graph_expansion_depth: u32,
    },
    /// 让 QueryRouter 自动选择（第二阶段+）。
    Auto,
}

impl Default for SearchStrategy {
    fn default() -> Self {
        Self::FullHybrid {
            vector_weight: 0.4,
            text_weight: 0.4,
            graph_weight: 0.2,
            graph_expansion_depth: 1,
        }
    }
}

/// 检索结果置信度 — 基于多路径信号一致性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfidenceLevel {
    /// 多路径强一致 — 最高置信度。
    High,
    /// 双路径一致或单路径强信号。
    Medium,
    /// 仅单路径弱信号 — 可能为噪声。
    Low,
    /// 无有意义信号 — 用于空结果场景。
    None,
}

/// 返回给调用者的单条搜索结果。
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub document_id: DocumentId,
    pub title: String,
    /// 相关文本片段 — 可能来自多个分块。
    pub snippet: String,
    /// 融合后的相关性分数 0.0–1.0。
    pub score: f32,
    pub source_path: String,
    /// 哪些检索器为此结果做出了贡献。
    pub match_sources: Vec<MatchSource>,
    /// 结果置信度 — 基于信号一致性评估。
    pub confidence: ConfidenceLevel,
    /// 诊断信息（Phase 1 暂为 None，Phase 2 启用）。
    pub diagnostic: Option<String>,
}

/// 结果来自哪个检索来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchSource {
    Vector,
    Text,
    Graph,
}

/// 应用于向量和全文搜索的可选过滤器。
///
/// 主题过滤在引擎层处理（图遍历），不包含在此处。
#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    /// 限定搜索特定知识库（None 表示所有知识库）。
    pub kb_id: Option<String>,
    pub document_ids: Option<Vec<String>>,
    pub file_types: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// 存储模式
// ---------------------------------------------------------------------------

/// 控制文档摄入时使用哪些存储后端的模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StorageMode {
    /// 全量存储：向量 + 全文 + 图（默认，兼容现有行为）
    #[default]
    Full,
    /// 仅向量语义搜索
    VectorOnly,
    /// 仅全文关键词索引
    TextOnly,
    /// 仅图谱结构边（CONTAINS + NEXT_CHUNK），不嵌入
    GraphOnly,
    /// 向量 + 全文（无图）
    VectorAndText,
    /// 向量 + 图
    VectorAndGraph,
    /// 全文 + 图
    TextAndGraph,
}

/// 返回该模式是否需要分块。
pub fn mode_requires_chunks(mode: &StorageMode) -> bool {
    matches!(
        mode,
        StorageMode::Full
            | StorageMode::VectorOnly
            | StorageMode::TextOnly
            | StorageMode::VectorAndText
            | StorageMode::VectorAndGraph
            | StorageMode::TextAndGraph
    )
}

/// 返回该模式是否需要嵌入。
pub fn mode_requires_embed(mode: &StorageMode) -> bool {
    matches!(
        mode,
        StorageMode::Full | StorageMode::VectorOnly | StorageMode::VectorAndText | StorageMode::VectorAndGraph
    )
}

/// 返回该模式是否需要向量索引更新。
pub fn mode_requires_vector(mode: &StorageMode) -> bool {
    matches!(
        mode,
        StorageMode::Full | StorageMode::VectorOnly | StorageMode::VectorAndText | StorageMode::VectorAndGraph
    )
}

/// 返回该模式是否需要全文索引。
pub fn mode_requires_text(mode: &StorageMode) -> bool {
    matches!(
        mode,
        StorageMode::Full | StorageMode::TextOnly | StorageMode::VectorAndText | StorageMode::TextAndGraph
    )
}

/// 返回该模式是否需要图谱结构边。
pub fn mode_requires_graph(mode: &StorageMode) -> bool {
    matches!(
        mode,
        StorageMode::Full | StorageMode::GraphOnly | StorageMode::VectorAndGraph | StorageMode::TextAndGraph
    )
}

// ---------------------------------------------------------------------------
// 存储统计
// ---------------------------------------------------------------------------

/// 知识存储的聚合统计信息。
#[derive(Debug, Clone, Default)]
pub struct StoreStats {
    pub document_count: usize,
    pub chunk_count: usize,
    pub total_bytes: u64,
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_strategy_default_is_full_hybrid() {
        let s = SearchStrategy::default();
        assert!(matches!(
            s,
            SearchStrategy::FullHybrid {
                vector_weight: 0.4,
                text_weight: 0.4,
                graph_weight: 0.2,
                graph_expansion_depth: 1,
            }
        ));
    }

    #[test]
    fn document_summary_from_document() {
        let doc = Document {
            id: "abc".into(),
            kb_id: None,
            title: "Test".into(),
            source_path: "/tmp/test.md".into(),
            content: "hello world".into(),
            metadata: DocumentMetadata {
                file_type: Some("md".into()),
                ..Default::default()
            },
        };
        let summary = DocumentSummary::from(&doc);
        assert_eq!(summary.id, "abc");
        assert_eq!(summary.title, "Test");
        assert_eq!(summary.file_type.as_deref(), Some("md"));
    }

    #[test]
    fn search_filters_default() {
        let f = SearchFilters::default();
        assert!(f.kb_id.is_none());
        assert!(f.document_ids.is_none());
        assert!(f.file_types.is_none());
    }

    #[test]
    fn storage_mode_full_requires_all() {
        let m = StorageMode::Full;
        assert!(mode_requires_chunks(&m));
        assert!(mode_requires_embed(&m));
        assert!(mode_requires_vector(&m));
        assert!(mode_requires_text(&m));
        assert!(mode_requires_graph(&m));
    }

    #[test]
    fn storage_mode_graph_only_requires_no_embed() {
        let m = StorageMode::GraphOnly;
        assert!(!mode_requires_chunks(&m));
        assert!(!mode_requires_embed(&m));
        assert!(!mode_requires_vector(&m));
        assert!(!mode_requires_text(&m));
        assert!(mode_requires_graph(&m));
    }

    #[test]
    fn storage_mode_vector_only() {
        let m = StorageMode::VectorOnly;
        assert!(mode_requires_chunks(&m));
        assert!(mode_requires_embed(&m));
        assert!(mode_requires_vector(&m));
        assert!(!mode_requires_text(&m));
        assert!(!mode_requires_graph(&m));
    }

    #[test]
    fn storage_mode_default_is_full() {
        assert_eq!(StorageMode::default(), StorageMode::Full);
    }

    #[test]
    fn fact_compute_id_deterministic() {
        let id1 = Fact::compute_id("用户", "prefers", "Rust");
        let id2 = Fact::compute_id("用户", "prefers", "Rust");
        assert_eq!(id1, id2);
        assert!(id1.starts_with("fact:"));
    }

    #[test]
    fn fact_compute_id_different_for_different_input() {
        let id1 = Fact::compute_id("用户", "prefers", "Rust");
        let id2 = Fact::compute_id("用户", "prefers", "Go");
        assert_ne!(id1, id2);
    }

    #[test]
    fn compute_entity_id_deterministic() {
        let id1 = compute_entity_id("张三", "Person");
        let id2 = compute_entity_id("张三", "Person");
        assert_eq!(id1, id2);
        assert!(id1.starts_with("entity:Person:"));
    }

    #[test]
    fn compute_entity_id_case_insensitive() {
        let id1 = compute_entity_id("Alice", "Person");
        let id2 = compute_entity_id("alice", "Person");
        assert_eq!(id1, id2, "entity ID should be case-insensitive");
    }

    #[test]
    fn fact_new_auto_generates_id() {
        let fact = Fact::new("用户", "has_skill", "Rust", 0.9);
        assert!(fact.id.starts_with("fact:"));
        assert_eq!(fact.subject, "用户");
        assert_eq!(fact.predicate, "has_skill");
        assert_eq!(fact.object, "Rust");
        assert_eq!(fact.confidence, 0.9);
    }
}
