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
    async fn remove_node_edges(&self, node_id: &str) -> Result<(), KnowledgeError>;

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
