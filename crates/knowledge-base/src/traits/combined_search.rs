use async_trait::async_trait;

use crate::error::KnowledgeError;
use crate::traits::graph_store::EdgeType;
use crate::types::{SearchFilters, SearchResult};

// ---------------------------------------------------------------------------
// RRF 配置
// ---------------------------------------------------------------------------

/// 倒数排名融合（Reciprocal Rank Fusion）参数。
///
/// 公式：`score(d) = Σ weight_i / (k + rank_i(d))`
#[derive(Debug, Clone)]
pub struct RrfConfig {
    /// 平滑常数（推荐值：来自 RRF 论文的 60）。
    pub k: f32,
    /// 最小融合 RRF 分数 — 低于此值的结果将被剪枝。
    ///
    /// 设置为 0.003，作为多路径融合后的最终质量门控。
    /// RRF 分数量级参考：
    /// * 双路径 top-1：约 0.016（通过）
    /// * 单路径 top-1：约 0.008（通过）
    /// * 单路径弱信号 top-5：约 0.0015（过滤）
    /// * 纯噪声：约 0.0（过滤）
    pub min_score: f32,
}

impl Default for RrfConfig {
    fn default() -> Self {
        Self {
            k: 60.0,
            // 0.003 过滤单路径弱信号和纯噪声，同时保留所有合法的双路径
            // 以及强单路径结果。Phase 2 将引入自适应 min_score。
            min_score: 0.003,
        }
    }
}

// ---------------------------------------------------------------------------
// CombinedQuery
// ---------------------------------------------------------------------------

/// 完整的多策略检索规格说明。
///
/// 对于 HelixDB 等后端，这会转换为**单个**复合查询
///（一次 HTTP 往返）。对于更简单的后端，引擎会将其分解为
/// 单独的 `VectorIndex` / `FullTextIndex` / `GraphStore` 调用。
#[derive(Debug, Clone)]
pub struct CombinedQuery {
    pub query_text: String,
    /// 预先计算的查询嵌入向量。
    pub query_vector: Vec<f32>,
    /// 过采样的向量 top-k（通常为 `top_k * 3`）。
    pub vector_top_k: usize,
    /// 全文 top-k。
    pub text_top_k: usize,
    /// 图扩展深度（0 = 禁用）。
    pub graph_expansion_depth: u32,
    pub graph_edge_types: Vec<EdgeType>,
    pub fusion: RrfConfig,
    pub filters: Option<SearchFilters>,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// 后端原生的组合搜索能力（可选）。
///
/// 只有能通过单次调用执行多策略查询的后端（HelixDB、pgvector+AGE）
/// 才实现此 trait。当不存在时，`HybridSearchEngine` 会退回到组合
/// 各个单独的 trait 调用。
#[async_trait]
pub trait CombinedSearch: Send + Sync {
    /// 执行组合查询，返回融合并排序后的结果。
    ///
    /// 后端负责：多路径检索 → RRF 融合 → 去重 → 排序 → top-k 截断。
    async fn combined_search(
        &self,
        query: &CombinedQuery,
    ) -> Result<Vec<SearchResult>, KnowledgeError>;
}
