use std::collections::HashMap;

use async_trait::async_trait;

use crate::error::KnowledgeError;

// ---------------------------------------------------------------------------
// 边类型
// ---------------------------------------------------------------------------

/// 知识图谱中的有向边。
#[derive(Debug, Clone)]
pub struct KnowledgeEdge {
    pub source_id: String,
    pub target_id: String,
    pub edge_type: EdgeType,
    /// 权重 0.0–1.0。
    pub weight: f32,
    pub properties: HashMap<String, String>,
}

/// 预定义和自定义的边类型标签。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EdgeType {
    Custom(String),
    // ── 预定义 ──
    Contains,  // 文档 → 分块
    RelatedTo, // 文档 ↔ 文档
    Mentions,  // 分块 → 实体
    BelongsTo, // 文档 → 主题
    NextChunk, // 分块 → 分块（顺序）
}

impl EdgeType {
    /// 返回存储后端中使用的规范标签字符串。
    pub fn as_label(&self) -> &str {
        match self {
            Self::Contains => "CONTAINS",
            Self::RelatedTo => "RELATED_TO",
            Self::Mentions => "MENTIONS",
            Self::BelongsTo => "BELONGS_TO",
            Self::NextChunk => "NEXT_CHUNK",
            Self::Custom(s) => s.as_str(),
        }
    }
}

// ---------------------------------------------------------------------------
// 遍历
// ---------------------------------------------------------------------------

/// 图遍历的方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraversalDirection {
    Outgoing,
    Incoming,
    Both,
}

/// 图遍历中的一步（节点 + 到达该节点的边）。
#[derive(Debug, Clone)]
pub struct TraversalStep {
    pub node: GraphNode,
    /// 对于起始节点为 `None`。
    pub via_edge: Option<EdgeType>,
}

/// 知识图谱中的一个节点。
#[derive(Debug, Clone)]
pub struct GraphNode {
    pub id: String,
    pub labels: Vec<String>,
    pub properties: HashMap<String, String>,
    /// 从起始节点出发的跳数距离。
    pub distance: u32,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// 图存储抽象 — 负责知识图谱的遍历和关系管理。
#[async_trait]
pub trait GraphStore: Send + Sync {
    /// 创建单条边。
    async fn add_edge(&self, edge: KnowledgeEdge) -> Result<(), KnowledgeError>;

    /// 批量创建边。
    async fn add_edges(&self, edges: &[KnowledgeEdge]) -> Result<(), KnowledgeError>;

    /// 移除与某个节点相连的所有边。
    ///
    /// **契约不完整，调用方不得依赖其字面语义**（三个后端的实现互不相同）：
    /// - HelixDB 后端是**显式空操作** —— 图侧清理由 `DocumentStore::delete` 的
    ///   `delete_document_cascade` 承担；
    /// - 内存后端只清理**以 `node_id` 为端点**的边，分块级边（`NEXT_CHUNK` 等）
    ///   不在清理范围内，会留下悬空边。
    ///
    /// 需要「确实删掉两个实体之间的边」时用 [`Self::remove_edges_between`]。
    async fn remove_node_edges(&self, node_id: &str) -> Result<(), KnowledgeError>;

    /// 读取两个实体之间指定谓词的边（供存在性检查与审计快照）。
    ///
    /// `edge_type` 为 `None` 时匹配**两个方向**上的全部边；为 `Some` 时只匹配
    /// `source_id → target_id` 一个方向，且按 `edge_type` 精确匹配（谓词无任何
    /// 规范化，大小写敏感）。
    ///
    /// 返回的 `KnowledgeEdge` 必须携带真实 `weight`。
    async fn edges_between(
        &self,
        source_id: &str,
        target_id: &str,
        edge_type: Option<&EdgeType>,
    ) -> Result<Vec<KnowledgeEdge>, KnowledgeError>;

    /// 删除两个实体之间指定谓词的边，返回**删除前观测到的匹配边数**。
    ///
    /// **方向**：只删 `source_id → target_id`（有向），不删反向边 —— 与内存后端
    /// 的 `retain` 语义一致；`edge_type` 为 `None` 时两个方向都删。
    ///
    /// **返回值是下界，不是精确删除数**：HelixDB 不报告删除条数（`DropEdgeLabeled`
    /// 后的 `Count` 数的是流经的源节点数，边不存在时仍返回 1），因此这里取「删前
    /// 读到的条数」。后置条件是「返回后 `(source, target, edge_type)` 已无匹配边」，
    /// 但「读-删」之间存在并发窗口（后台记忆提取会并发写同一个图），实际删除数可能
    /// 多于返回值。调用方不得把它当作精确删除数做账。
    ///
    /// `Ok(0)` 表示没有匹配的边。
    async fn remove_edges_between(
        &self,
        source_id: &str,
        target_id: &str,
        edge_type: Option<&EdgeType>,
    ) -> Result<usize, KnowledgeError>;

    /// 删除单个节点，返回**删除前观测到的关联边数**。
    ///
    /// `cascade == false` 且该节点仍有边时返回 `KnowledgeError::InvalidInput`；
    /// 为空才继续删除。`cascade == true` 时删除节点并级联带走其双向全部关联边。
    ///
    /// 返回值同样是下界，理由见 [`Self::remove_edges_between`]。
    async fn remove_node(&self, node_id: &str, cascade: bool) -> Result<usize, KnowledgeError>;

    /// 从起始节点沿指定边类型进行 BFS 遍历。
    async fn traverse(
        &self,
        start_node: &str,
        edge_types: &[EdgeType],
        direction: TraversalDirection,
        max_depth: u32,
    ) -> Result<Vec<TraversalStep>, KnowledgeError>;

    /// 查找两个节点之间的最短路径。
    async fn shortest_path(
        &self,
        from: &str,
        to: &str,
        edge_types: &[EdgeType],
        max_depth: u32,
    ) -> Result<Option<Vec<TraversalStep>>, KnowledgeError>;

    /// 从多个分块 ID 批量扩展（搜索后图增强）。
    ///
    /// 语义：给定一组分块 ID，沿着 CONTAINS（入向）查找父文档，
    /// 然后通过 RELATED_TO / BELONGS_TO 查找相关内容。
    async fn expand(
        &self,
        start_chunk_ids: &[String],
        edge_types: &[EdgeType],
        max_depth: u32,
    ) -> Result<Vec<GraphNode>, KnowledgeError>;

    /// 插入或更新一个节点。
    ///
    /// 默认实现为空操作，适用于不显式存储节点元数据的后端。
    async fn upsert_node(&self, _node: GraphNode) -> Result<(), KnowledgeError> {
        Ok(())
    }

    /// 按 ID 获取节点。
    ///
    /// 返回 `None` 表示节点不存在或此后端不支持节点存储。
    async fn get_node(&self, _node_id: &str) -> Result<Option<GraphNode>, KnowledgeError> {
        Ok(None)
    }

    /// 检查节点是否存在。
    async fn node_exists(&self, node_id: &str) -> Result<bool, KnowledgeError> {
        Ok(self.get_node(node_id).await?.is_some())
    }
}
