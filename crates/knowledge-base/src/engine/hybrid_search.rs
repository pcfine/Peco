use std::sync::Arc;

use crate::config::EmbeddingModelConfig;
use crate::engine::cross_validation::{CrossValidation, validate_signals};
use crate::engine::fusion::rrf_fuse;
use crate::engine::query_analysis::{QueryAnalyzer, query_adjusted_weights};
use crate::engine::query_router::{BackendCapabilities, QueryRouter};
use crate::engine::score_calibration::{PathCalibration, calibrate_path};
use crate::error::KnowledgeError;
use crate::traits::combined_search::RrfConfig;
use crate::traits::*;
use crate::types::*;
use tracing::{info, warn};

/// 向量搜索命中结果进入 RRF 融合的最低余弦相似度。
///
/// 向量相似度低于此阈值的分块在排序前被丢弃。
/// 这可以防止基于排名的 RRF 公式为与查询语义无关的文档
/// 赋予有意义的分数。
///
/// 0.55 是 BGE 中文嵌入模型的推荐阈值：
/// * ≥ 0.55 — 有意义的相关性（保留）
/// * < 0.55 — 噪声或弱主题重叠（丢弃）
///
/// # 阈值选择依据
///
/// BGE 嵌入空间中，不相关文档对的余弦相似度噪声底噪：
/// * BGE-small-zh (512d): 噪声底噪约 0.20–0.50
/// * BGE-M3 / BGE-large-zh (1024d): 噪声底噪约 0.15–0.40
///
/// 0.55 高于两种模型的噪声区间上界，能有效过滤假阳性，
/// 同时保留真正相关的文档对（相关文档相似度通常 ≥ 0.65）。
///
/// 设计原则：宁缺毋滥 — 优先降低假阳性，可接受少量假阴性。
///
/// 注意：Phase 2 中，当启用自适应管道时，此常量作为
/// `PathCalibration::adaptive_threshold` 的下限。
/// 每个路径的实际阈值会根据其分数分布动态上调。
const MIN_VECTOR_SCORE: f32 = 0.55;

// ---------------------------------------------------------------------------
// AdaptiveFusionConfig
// ---------------------------------------------------------------------------

/// 根据 Layer 2–3 的信号校准和交叉验证结果自适应确定的
/// RRF 融合参数。
#[derive(Debug, Clone)]
pub struct AdaptiveFusionConfig {
    /// RRF k 参数（平滑常数）。
    pub k: f32,
    /// 自适应最小 RRF 分数 — 低于此值的结果在融合后被截断。
    ///
    /// 根据信号一致性动态设置：
    /// * StrongAgreement → 0.002（宽松：两条路径一致）
    /// * WeakAgreement → 0.003（适中）
    /// * SinglePath → 0.005（严格：单路径需要更强的信号）
    /// * NoSignal → N/A（在融合前已短路）
    pub min_rrf_score: f32,
    /// 多路径融合是否在额外的确认步骤后应用后验过滤器。
    pub require_cross_validation: bool,
}

impl Default for AdaptiveFusionConfig {
    fn default() -> Self {
        Self {
            k: 60.0,
            min_rrf_score: 0.003,
            require_cross_validation: false,
        }
    }
}

/// 将 Layer 2–3 的信号校准和交叉验证结果映射为
/// 自适应融合配置。
///
/// # 信号矩阵 → 融合参数
///
/// | 交叉验证 | RRF min_score | 原理 |
/// |---|---|---|
/// | StrongAgreement | 0.002 | 两条独立路径收敛 → 降低阈值 |
/// | WeakAgreement | 0.003 | 一些重叠 → 标准阈值 |
/// | SinglePath | 0.005 | 仅一条路径 → 需要更强的独立信号 |
/// | NoSignal | 短路 | 在融合前返回空结果 |
pub fn adaptive_fusion_config(
    cv: CrossValidation,
    _calibrations: &[PathCalibration],
) -> AdaptiveFusionConfig {
    match cv {
        CrossValidation::StrongAgreement => AdaptiveFusionConfig {
            k: 60.0,
            min_rrf_score: 0.002,
            require_cross_validation: true,
        },
        CrossValidation::WeakAgreement => AdaptiveFusionConfig {
            k: 60.0,
            min_rrf_score: 0.003,
            require_cross_validation: true,
        },
        CrossValidation::SinglePath => AdaptiveFusionConfig {
            k: 60.0,
            min_rrf_score: 0.005,
            require_cross_validation: false,
        },
        CrossValidation::NoSignal => AdaptiveFusionConfig {
            k: 60.0,
            min_rrf_score: 0.010, // 不应到达 — 调用方应已短路。
            require_cross_validation: false,
        },
    }
}

/// 将 Layer 2–3 交叉验证结果映射为 [`ConfidenceLevel`]。
fn confidence_from_cv(cv: CrossValidation) -> ConfidenceLevel {
    match cv {
        CrossValidation::StrongAgreement => ConfidenceLevel::High,
        CrossValidation::WeakAgreement => ConfidenceLevel::High,
        CrossValidation::SinglePath => ConfidenceLevel::Medium,
        CrossValidation::NoSignal => ConfidenceLevel::None,
    }
}

// ---------------------------------------------------------------------------
// HybridSearchEngine
// ---------------------------------------------------------------------------

/// 混合搜索引擎 — 编排多路径检索 + RRF 融合。
///
/// ## 检索路径（自动选择）
///
/// 1. **CombinedSearch 快速路径**（例如 HelixDB）：如果设置了 `combined_searcher`，
///    引擎将请求打包为 `CombinedQuery` 并完全委托给后端 —
///    一次调用，一个结果集。
///
/// 2. **Trait 组合回退**（LanceDB、InMemory）：独立调用每个可用的
///    trait（`VectorIndex`、`FullTextIndex`、`GraphStore`），然后在引擎层
///    融合结果。
///
/// 单条检索路径的失败按尽力而为降级处理 — 一条检索器失败
/// 不会导致整个搜索失败。
///
/// ## 自适应管道（Layer 1–4）
///
/// 当通过 [`with_query_analyzer`] 设置查询分析器时，引擎启用完整的
/// 五层自适应检索管道：
///
/// 1. **Layer 1 — 查询分析**：分类意图、调整每条路径的权重。
/// 2. **Layer 2 — 分数校准**：每条路径计算自适应阈值。
/// 3. **Layer 3 — 交叉验证**：检查跨路径的一致性。
/// 4. **Layer 4 — 自适应融合**：根据信号质量调整 RRF 参数。
///
/// 当未设置查询分析器时，引擎使用 Phase 1 的简单固定阈值行为
/// （向后兼容）。
pub struct HybridSearchEngine {
    doc_store: Arc<dyn DocumentStore>,
    vector_index: Option<Arc<dyn VectorIndex>>,
    graph_store: Option<Arc<dyn GraphStore>>,
    fulltext_index: Option<Arc<dyn FullTextIndex>>,
    embedding: Arc<dyn EmbeddingEngine>,
    combined_searcher: Option<Arc<dyn CombinedSearch>>,
    query_router: Option<Box<dyn QueryRouter>>,
    /// Layer 1 查询分析器 — 当设置时启用自适应管道。
    query_analyzer: Option<Box<dyn QueryAnalyzer>>,
    /// 嵌入模型配置 — 为 Layer 2 校准提供噪声底噪。
    model_config: EmbeddingModelConfig,
}

impl HybridSearchEngine {
    /// 构建一个新的引擎。
    ///
    /// # Panics
    ///
    /// 如果提供了 `vector_index` 且其 `ndims()` 与 `embedding.ndims()` 不匹配，
    /// 此构造函数会 panic — 维度不匹配是配置错误，最好及早发现。
    pub fn new(
        doc_store: Arc<dyn DocumentStore>,
        vector_index: Option<Arc<dyn VectorIndex>>,
        graph_store: Option<Arc<dyn GraphStore>>,
        fulltext_index: Option<Arc<dyn FullTextIndex>>,
        embedding: Arc<dyn EmbeddingEngine>,
    ) -> Self {
        if let Some(ref vi) = vector_index {
            let vi_dims = vi.ndims();
            let ee_dims = embedding.ndims();
            // ndims 为 0 表示"接受任意维度"（InMemory 后端）。
            if vi_dims != 0 && ee_dims != 0 {
                assert_eq!(
                    vi_dims, ee_dims,
                    "VectorIndex 维度 ({vi_dims}) != EmbeddingEngine 维度 ({ee_dims})"
                );
            }
        }
        Self {
            doc_store,
            vector_index,
            graph_store,
            fulltext_index,
            embedding,
            combined_searcher: None,
            query_router: None,
            query_analyzer: None,
            model_config: EmbeddingModelConfig::default(),
        }
    }

    /// 注入后端原生的组合搜索器以使用快速路径。
    pub fn with_combined_search(mut self, searcher: Arc<dyn CombinedSearch>) -> Self {
        self.combined_searcher = Some(searcher);
        self
    }

    /// 注入查询路由器以支持自动策略模式。
    pub fn with_query_router(mut self, router: Box<dyn QueryRouter>) -> Self {
        self.query_router = Some(router);
        self
    }

    /// 注入查询分析器以启用自适应 Layer 1–4 管道。
    ///
    /// 未设置时，引擎回退到 Phase 1 的固定阈值行为。
    pub fn with_query_analyzer(mut self, analyzer: Box<dyn QueryAnalyzer>) -> Self {
        self.query_analyzer = Some(analyzer);
        self
    }

    /// 设置嵌入模型配置（用于 Layer 2 噪声底噪）。
    ///
    /// 如果未调用，默认使用 [`EmbeddingModelConfig::bge_large_zh`]。
    pub fn with_model_config(mut self, config: EmbeddingModelConfig) -> Self {
        self.model_config = config;
        self
    }

    /// 如果配置了查询分析器，则返回 true。
    pub fn has_adaptive_pipeline(&self) -> bool {
        self.query_analyzer.is_some()
    }

    /// 返回此引擎检测到的能力。
    pub fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            has_vector: self.vector_index.is_some() || self.combined_searcher.is_some(),
            has_fulltext: self.fulltext_index.is_some() || self.combined_searcher.is_some(),
            has_graph: self.graph_store.is_some() || self.combined_searcher.is_some(),
        }
    }

    // ------------------------------------------------------------------
    // search
    // ------------------------------------------------------------------

    /// 使用配置的策略（或自动路由）执行搜索。
    pub async fn search(
        &self,
        request: &SearchRequest,
    ) -> Result<Vec<SearchResult>, KnowledgeError> {
        let strategy = match &request.strategy {
            SearchStrategy::Auto => {
                if let Some(ref router) = self.query_router {
                    router.route(&request.query, &self.capabilities())
                } else {
                    SearchStrategy::default()
                }
            }
            s => s.clone(),
        };

        info!(
            strategy = ?strategy,
            query = %request.query,
            adaptive = self.has_adaptive_pipeline(),
            "混合搜索"
        );

        // ── 快速路径：CombinedSearch ──
        if let Some(ref cs) = self.combined_searcher {
            return self.combined_fast_path(cs, request, &strategy).await;
        }

        // ── 回退：组合各个 trait ──
        if self.has_adaptive_pipeline() {
            self.compose_search_adaptive(request, &strategy).await
        } else {
            self.compose_search_phase1(request, &strategy).await
        }
    }

    async fn combined_fast_path(
        &self,
        cs: &Arc<dyn CombinedSearch>,
        request: &SearchRequest,
        strategy: &SearchStrategy,
    ) -> Result<Vec<SearchResult>, KnowledgeError> {
        let query_vec = self
            .embedding
            .embed_query(&request.query)
            .await
            .map_err(|e| KnowledgeError::EmbeddingError(format!("查询嵌入失败: {e}")))?;

        let (vec_top_k, text_top_k, graph_depth, graph_edge_types, _fusion) =
            decompose_strategy(strategy, request.top_k);

        let combined = CombinedQuery {
            query_text: request.query.clone(),
            query_vector: query_vec,
            vector_top_k: vec_top_k,
            text_top_k,
            graph_expansion_depth: graph_depth,
            graph_edge_types,
            fusion: _fusion,
            filters: request.filters.clone(),
        };

        cs.combined_search(&combined).await
    }

    // ------------------------------------------------------------------
    // Phase 1 compose_search（向后兼容）
    // ------------------------------------------------------------------

    /// Phase 1 搜索：固定阈值 + 简单单路径弱信号检查。
    ///
    /// 当未通过 [`with_query_analyzer`] 设置查询分析器时使用。
    async fn compose_search_phase1(
        &self,
        request: &SearchRequest,
        strategy: &SearchStrategy,
    ) -> Result<Vec<SearchResult>, KnowledgeError> {
        let (vec_top_k, text_top_k, graph_depth, graph_edge_types, _fusion) =
            decompose_strategy(strategy, request.top_k);

        let mut ranked_lists: Vec<(f32, Vec<(String, f32)>)> = Vec::new();
        let mut any_success = false;

        // ── 向量搜索 ──
        if should_use_vector(strategy)
            && let Some(ref vi) = self.vector_index
        {
            match self.embedding.embed_query(&request.query).await {
                Ok(query_vec) => match vi
                    .search(&query_vec, vec_top_k, request.filters.as_ref())
                    .await
                {
                    Ok(hits) => {
                        let items = dedup_by_doc_id(
                            hits.into_iter()
                                .filter(|h| h.score >= MIN_VECTOR_SCORE)
                                .map(|h| (h.document_id, h.score))
                                .collect(),
                        );
                        if !items.is_empty() {
                            ranked_lists.push((vector_weight(strategy), items));
                            any_success = true;
                        }
                    }
                    Err(e) => warn!("向量搜索失败: {e}"),
                },
                Err(e) => warn!("嵌入失败: {e}"),
            }
        }

        // ── 全文搜索 ──
        if should_use_text(strategy)
            && let Some(ref ft) = self.fulltext_index
        {
            match ft
                .search(&request.query, text_top_k, request.filters.as_ref())
                .await
            {
                Ok(hits) => {
                    let items = dedup_by_doc_id(
                        hits.into_iter().map(|h| (h.document_id, h.score)).collect(),
                    );
                    if !items.is_empty() {
                        ranked_lists.push((text_weight(strategy), items));
                        any_success = true;
                    }
                }
                Err(e) => warn!("全文搜索失败: {e}"),
            }
        }

        // ── 图扩展 ──
        if should_use_graph(strategy)
            && let Some(ref gs) = self.graph_store
        {
            let top_chunk_ids: Vec<String> =
                self.collect_top_chunk_ids(request, strategy, 20).await;

            if !top_chunk_ids.is_empty() {
                match gs
                    .expand(&top_chunk_ids, &graph_edge_types, graph_depth)
                    .await
                {
                    Ok(nodes) => {
                        let items = dedup_by_doc_id(
                            nodes
                                .into_iter()
                                .map(|n| {
                                    let score = 1.0 / (1.0 + n.distance as f32);
                                    (n.id, score)
                                })
                                .collect(),
                        );
                        if !items.is_empty() {
                            ranked_lists.push((graph_weight(strategy), items));
                            any_success = true;
                        }
                    }
                    Err(e) => warn!("图扩展失败: {e}"),
                }
            }
        }

        if !any_success {
            return Ok(Vec::new());
        }

        // ── Phase 1 简单信号检查 ──
        if ranked_lists.len() == 1 {
            let all_weak = ranked_lists[0].1.iter().all(|(_, s)| *s < 0.6);
            if all_weak {
                info!(
                    query = %request.query,
                    path_count = ranked_lists.len(),
                    "单路径弱信号 — 判定为噪声，返回空结果"
                );
                return Ok(Vec::new());
            }
        }

        // ── RRF 融合 ──
        let refs: Vec<(f32, &[(String, f32)])> = ranked_lists
            .iter()
            .map(|(w, v)| (*w, v.as_slice()))
            .collect();
        let fused = rrf_fuse(&refs, &RrfConfig::default());

        // ── 用文档内容丰富结果 ──
        let mut results = Vec::new();
        for (doc_id, score) in fused.into_iter().take(request.top_k) {
            if let Ok(Some(doc)) = self.doc_store.get(&doc_id).await {
                let snippet = doc.content.chars().take(500).collect();
                results.push(SearchResult {
                    document_id: doc_id,
                    title: doc.title,
                    snippet,
                    score,
                    source_path: doc.source_path,
                    match_sources: Vec::new(),
                    confidence: ConfidenceLevel::Medium,
                    diagnostic: None,
                });
            }
        }

        Ok(results)
    }

    // ------------------------------------------------------------------
    // Phase 2 compose_search_adaptive（Layer 1–4 管道）
    // ------------------------------------------------------------------

    /// 自适应搜索：完整的 Layer 1–4 管道。
    ///
    /// 1. **Layer 1** — 查询分析 → 调整每条路径的权重。
    /// 2. **检索** — 使用 5 倍过采样并行执行每条路径。
    /// 3. **Layer 2** — 校准每条路径 → 自适应阈值 + 信号标志。
    /// 4. **Layer 3** — 跨路径交叉验证。
    /// 5. **Layer 4** — 自适应 RRF 融合 + 置信度标签。
    async fn compose_search_adaptive(
        &self,
        request: &SearchRequest,
        strategy: &SearchStrategy,
    ) -> Result<Vec<SearchResult>, KnowledgeError> {
        let oversample_factor = 5;

        // ── Layer 1: 查询分析 ──
        let analysis = self
            .query_analyzer
            .as_ref()
            .map(|a| a.analyze(&request.query));

        let (vec_weight, txt_weight, grph_weight) = if let Some(ref a) = analysis {
            query_adjusted_weights(a, strategy)
        } else {
            (
                vector_weight(strategy),
                text_weight(strategy),
                graph_weight(strategy),
            )
        };

        info!(
            query = %request.query,
            vec_w = vec_weight,
            txt_w = txt_weight,
            grph_w = grph_weight,
            intent = ?analysis.as_ref().map(|a| a.intent),
            "Layer 1: 权重已调整"
        );

        let vec_top_k = if should_use_vector(strategy) && vec_weight > 0.0 {
            request.top_k * oversample_factor
        } else {
            0
        };
        let text_top_k = if should_use_text(strategy) && txt_weight > 0.0 {
            request.top_k * oversample_factor
        } else {
            0
        };

        // ── 并行检索 + 收集原始命中用于校准 ──
        let mut vector_hits: Vec<(String, f32)> = Vec::new();
        let mut text_hits: Vec<(String, f32)> = Vec::new();
        let mut graph_hits: Vec<(String, f32)> = Vec::new();

        // 向量搜索。
        if vec_top_k > 0
            && let Some(ref vi) = self.vector_index
        {
            match self.embedding.embed_query(&request.query).await {
                Ok(query_vec) => {
                    match vi
                        .search(&query_vec, vec_top_k, request.filters.as_ref())
                        .await
                    {
                        Ok(hits) => {
                            vector_hits = dedup_by_doc_id(
                                hits.into_iter().map(|h| (h.document_id, h.score)).collect(),
                            );
                        }
                        Err(e) => warn!("向量搜索失败: {e}"),
                    }
                }
                Err(e) => warn!("嵌入失败: {e}"),
            }
        }

        // 全文搜索。
        if text_top_k > 0
            && let Some(ref ft) = self.fulltext_index
        {
            match ft
                .search(&request.query, text_top_k, request.filters.as_ref())
                .await
            {
                Ok(hits) => {
                    text_hits = dedup_by_doc_id(
                        hits.into_iter().map(|h| (h.document_id, h.score)).collect(),
                    );
                }
                Err(e) => warn!("全文搜索失败: {e}"),
            }
        }

        // 图扩展。
        if should_use_graph(strategy)
            && grph_weight > 0.0
            && let Some(ref gs) = self.graph_store
        {
            let top_chunk_ids: Vec<String> =
                self.collect_top_chunk_ids(request, strategy, 20).await;
            if !top_chunk_ids.is_empty() {
                let graph_edge_types = match strategy {
                    SearchStrategy::FullHybrid { .. } => {
                        vec![EdgeType::Contains, EdgeType::RelatedTo, EdgeType::BelongsTo]
                    }
                    SearchStrategy::GraphOnly { .. } => vec![EdgeType::Contains],
                    _ => vec![],
                };
                let graph_depth = match strategy {
                    SearchStrategy::FullHybrid {
                        graph_expansion_depth,
                        ..
                    } => *graph_expansion_depth,
                    SearchStrategy::GraphOnly { .. } => 2,
                    _ => 0,
                };
                match gs
                    .expand(&top_chunk_ids, &graph_edge_types, graph_depth)
                    .await
                {
                    Ok(nodes) => {
                        graph_hits = dedup_by_doc_id(
                            nodes
                                .into_iter()
                                .map(|n| {
                                    let score = 1.0 / (1.0 + n.distance as f32);
                                    (n.id, score)
                                })
                                .collect(),
                        );
                    }
                    Err(e) => warn!("图扩展失败: {e}"),
                }
            }
        }

        // ── Layer 2: 路径校准 ──
        //
        // 每条路径使用不同的 min_score 下限：
        // * 向量：使用模型噪声底噪（BGE-large-zh = 0.55）— 余弦相似度
        //   在噪声区间内（0.15–0.40）可能产生假阳性。
        // * 文本：使用 0.0 — BM25 重叠分数（matched/total）天然
        //   有界于 [0, 1]，部分匹配（如 0.5）就是有意义的信号。
        // * 图谱：使用 0.0 — 距离衍生分数，无固定噪声底噪。
        let vec_cal = calibrate_path("vector", &vector_hits, self.model_config.min_vector_score);
        let txt_cal = calibrate_path("fulltext", &text_hits, 0.0);
        let grph_cal = calibrate_path("graph", &graph_hits, 0.0);

        let calibrations = vec![vec_cal.clone(), txt_cal.clone(), grph_cal.clone()];

        info!(
            query = %request.query,
            vec_signal = vec_cal.has_signal,
            txt_signal = txt_cal.has_signal,
            grph_signal = grph_cal.has_signal,
            vec_threshold = vec_cal.adaptive_threshold,
            txt_threshold = txt_cal.adaptive_threshold,
            "Layer 2: 路径校准完成"
        );

        // ── Layer 3: 交叉验证 ──
        let path_doc_refs: Vec<&[(String, f32)]> = vec![
            vector_hits.as_slice(),
            text_hits.as_slice(),
            graph_hits.as_slice(),
        ];
        let cv = validate_signals(&calibrations, &path_doc_refs);

        info!(
            query = %request.query,
            cross_validation = ?cv,
            "Layer 3: 交叉验证"
        );

        if cv == CrossValidation::NoSignal {
            info!(
                query = %request.query,
                "Layer 3: NoSignal — 返回空结果"
            );
            return Ok(Vec::new());
        }

        // ── 按自适应阈值过滤每条路径 ──
        let mut ranked_lists: Vec<(f32, Vec<(String, f32)>)> = Vec::new();

        if vec_cal.has_signal && !vector_hits.is_empty() {
            let filtered: Vec<(String, f32)> = vector_hits
                .iter()
                .filter(|(_, s)| *s >= vec_cal.adaptive_threshold)
                .cloned()
                .collect();
            if !filtered.is_empty() {
                ranked_lists.push((vec_weight, filtered));
            }
        }

        if txt_cal.has_signal && !text_hits.is_empty() {
            let filtered: Vec<(String, f32)> = text_hits
                .iter()
                .filter(|(_, s)| *s >= txt_cal.adaptive_threshold)
                .cloned()
                .collect();
            if !filtered.is_empty() {
                ranked_lists.push((txt_weight, filtered));
            }
        }

        if grph_cal.has_signal && !graph_hits.is_empty() {
            let filtered: Vec<(String, f32)> = graph_hits
                .iter()
                .filter(|(_, s)| *s >= grph_cal.adaptive_threshold)
                .cloned()
                .collect();
            if !filtered.is_empty() {
                ranked_lists.push((grph_weight, filtered));
            }
        }

        if ranked_lists.is_empty() {
            return Ok(Vec::new());
        }

        // ── Layer 4: 自适应 RRF 融合 ──
        let fusion_cfg = adaptive_fusion_config(cv, &calibrations);
        let rrf_config = RrfConfig {
            k: fusion_cfg.k,
            min_score: fusion_cfg.min_rrf_score,
        };

        let refs: Vec<(f32, &[(String, f32)])> = ranked_lists
            .iter()
            .map(|(w, v)| (*w, v.as_slice()))
            .collect();
        let fused = rrf_fuse(&refs, &rrf_config);

        // ── 置信度 ──
        let base_confidence = confidence_from_cv(cv);

        // ── 构建结果 ──
        let mut results = Vec::new();
        for (doc_id, score) in fused.into_iter().take(request.top_k) {
            if let Ok(Some(doc)) = self.doc_store.get(&doc_id).await {
                let snippet = doc.content.chars().take(500).collect();
                let diagnostic = Some(format!(
                    "cv={cv:?} vec_sig={} txt_sig={} grph_sig={}",
                    vec_cal.has_signal, txt_cal.has_signal, grph_cal.has_signal,
                ));
                results.push(SearchResult {
                    document_id: doc_id,
                    title: doc.title,
                    snippet,
                    score,
                    source_path: doc.source_path,
                    match_sources: Vec::new(),
                    confidence: base_confidence,
                    diagnostic,
                });
            }
        }

        Ok(results)
    }

    /// 从向量 + 文本搜索中收集 top 分块 ID 以作为图扩展的种子。
    async fn collect_top_chunk_ids(
        &self,
        request: &SearchRequest,
        strategy: &SearchStrategy,
        top_k: usize,
    ) -> Vec<String> {
        let mut ids = Vec::new();

        if should_use_vector(strategy)
            && let Some(ref vi) = self.vector_index
            && let Ok(query_vec) = self.embedding.embed_query(&request.query).await
            && let Ok(hits) = vi.search(&query_vec, top_k, request.filters.as_ref()).await
        {
            ids.extend(hits.into_iter().map(|h| h.chunk_id));
        }

        if should_use_text(strategy)
            && let Some(ref ft) = self.fulltext_index
            && let Ok(hits) = ft
                .search(&request.query, top_k, request.filters.as_ref())
                .await
        {
            ids.extend(hits.into_iter().map(|h| h.chunk_id));
        }

        ids
    }
}

// ---------------------------------------------------------------------------
// 策略辅助函数
// ---------------------------------------------------------------------------

/// 每个文档 ID 只保留分数最高的条目。
///
/// 同一文档的多个分块可能在 RRF 中各占一个排名槽位，人为地抬高
/// 该文档的融合分数。去重确保每个文档在每个检索路径中最多贡献一次。
///
/// 输入假定按分数降序排列（向量/文本/图后端均返回排序后的结果）。
/// 每个 `doc_id` 的第一次出现即是最佳分数，因此我们简单地保留首次出现的。
fn dedup_by_doc_id(items: Vec<(String, f32)>) -> Vec<(String, f32)> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|(id, _)| seen.insert(id.clone()))
        .collect()
}

fn should_use_vector(s: &SearchStrategy) -> bool {
    matches!(
        s,
        SearchStrategy::VectorOnly
            | SearchStrategy::Hybrid { .. }
            | SearchStrategy::FullHybrid { .. }
    )
}

fn should_use_text(s: &SearchStrategy) -> bool {
    matches!(
        s,
        SearchStrategy::TextOnly
            | SearchStrategy::Hybrid { .. }
            | SearchStrategy::FullHybrid { .. }
    )
}

fn should_use_graph(s: &SearchStrategy) -> bool {
    matches!(
        s,
        SearchStrategy::GraphOnly { .. } | SearchStrategy::FullHybrid { .. }
    )
}

fn vector_weight(s: &SearchStrategy) -> f32 {
    match s {
        SearchStrategy::VectorOnly => 1.0,
        SearchStrategy::Hybrid { vector_weight, .. }
        | SearchStrategy::FullHybrid { vector_weight, .. } => *vector_weight,
        _ => 0.0,
    }
}

fn text_weight(s: &SearchStrategy) -> f32 {
    match s {
        SearchStrategy::TextOnly => 1.0,
        SearchStrategy::Hybrid { text_weight, .. }
        | SearchStrategy::FullHybrid { text_weight, .. } => *text_weight,
        _ => 0.0,
    }
}

fn graph_weight(s: &SearchStrategy) -> f32 {
    match s {
        SearchStrategy::FullHybrid { graph_weight, .. } => *graph_weight,
        SearchStrategy::GraphOnly { .. } => 1.0,
        _ => 0.0,
    }
}

fn decompose_strategy(
    s: &SearchStrategy,
    top_k: usize,
) -> (usize, usize, u32, Vec<EdgeType>, RrfConfig) {
    let vec_k = if should_use_vector(s) { top_k * 3 } else { 0 };
    let txt_k = if should_use_text(s) { top_k * 3 } else { 0 };
    let (graph_depth, graph_edge_types) = match s {
        SearchStrategy::FullHybrid {
            graph_expansion_depth,
            ..
        } => {
            let et = vec![EdgeType::Contains, EdgeType::RelatedTo, EdgeType::BelongsTo];
            (*graph_expansion_depth, et)
        }
        SearchStrategy::GraphOnly { .. } => (2, vec![EdgeType::Contains]),
        _ => (0, vec![]),
    };

    (
        vec_k,
        txt_k,
        graph_depth,
        graph_edge_types,
        RrfConfig::default(),
    )
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::memory::InMemoryBackend;
    use crate::engine::query_analysis::RuleBasedAnalyzer;
    use crate::engine::query_router::RuleBasedRouter;
    use std::sync::Arc;

    /// 用于测试的最小 mock 嵌入引擎。
    struct MockEmbedding {
        ndims: usize,
    }

    #[async_trait::async_trait]
    impl EmbeddingEngine for MockEmbedding {
        fn ndims(&self) -> usize {
            self.ndims
        }

        async fn embed_query(&self, _text: &str) -> Result<Vec<f32>, KnowledgeError> {
            Ok(vec![0.1; self.ndims])
        }

        async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, KnowledgeError> {
            Ok(texts.iter().map(|_| vec![0.1; self.ndims]).collect())
        }
    }

    /// 带有可配置模拟向量的 mock 嵌入引擎，用于测试
    /// 自适应管道中的分数分布。
    struct MockEmbeddingConfigurable {
        ndims: usize,
        /// 固定返回的向量。
        vector: Vec<f32>,
    }

    impl MockEmbeddingConfigurable {
        fn new(ndims: usize, vector: Vec<f32>) -> Self {
            Self { ndims, vector }
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingEngine for MockEmbeddingConfigurable {
        fn ndims(&self) -> usize {
            self.ndims
        }

        async fn embed_query(&self, _text: &str) -> Result<Vec<f32>, KnowledgeError> {
            Ok(self.vector.clone())
        }

        async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, KnowledgeError> {
            Ok(texts.iter().map(|_| self.vector.clone()).collect())
        }
    }

    fn make_test_doc(id: &str, content: &str) -> (Document, Vec<Chunk>) {
        let doc = Document {
            id: id.into(),
            kb_id: None,
            title: format!("Doc {id}"),
            source_path: format!("/tmp/{id}.md"),
            content: content.into(),
            metadata: DocumentMetadata {
                file_type: Some("md".into()),
                ..Default::default()
            },
        };
        let chunk = Chunk {
            id: format!("{id}-0000-aaaaaaaa"),
            document_id: id.into(),
            text: content.chars().take(200).collect(),
            sequence_index: 0,
            page_number: None,
            embedding: vec![0.1],
            metadata: ChunkMetadata::default(),
        };
        (doc, vec![chunk])
    }

    // ── Phase 1 兼容性测试（无 query_analyzer） ──

    #[tokio::test]
    async fn hybrid_search_basic() {
        let backend = Arc::new(InMemoryBackend::new());
        let embedding = Arc::new(MockEmbedding { ndims: 1 });

        let (doc, chunks) = make_test_doc(
            "rust-1",
            "Rust is a systems programming language. It provides memory safety without garbage collection. It is blazingly fast.",
        );
        backend.store(doc.clone(), chunks.clone()).await.unwrap();
        backend
            .upsert(&[VectorEntry {
                id: chunks[0].id.clone(),
                document_id: doc.id.clone(),
                vector: vec![0.1],
                text: chunks[0].text.clone(),
            }])
            .await
            .unwrap();
        backend
            .index(&[FullTextEntry {
                id: chunks[0].id.clone(),
                document_id: doc.id.clone(),
                text: chunks[0].text.clone(),
            }])
            .await
            .unwrap();

        let engine = HybridSearchEngine::new(
            backend.clone() as Arc<dyn DocumentStore>,
            Some(backend.clone() as Arc<dyn VectorIndex>),
            None,
            Some(backend.clone() as Arc<dyn FullTextIndex>),
            embedding,
        )
        .with_query_router(Box::new(RuleBasedRouter::new()));

        let results = engine
            .search(&SearchRequest {
                query: "What is Rust?".into(),
                top_k: 5,
                strategy: SearchStrategy::Hybrid {
                    vector_weight: 0.5,
                    text_weight: 0.5,
                },
                filters: None,
                min_confidence: None,
            })
            .await
            .unwrap();

        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn search_with_auto_routing() {
        let backend = Arc::new(InMemoryBackend::new());
        let embedding = Arc::new(MockEmbedding { ndims: 1 });

        let (doc, chunks) = make_test_doc("rust-2", "Rust programming language guide.");
        backend.store(doc, chunks).await.unwrap();

        let engine = HybridSearchEngine::new(
            backend.clone() as Arc<dyn DocumentStore>,
            Some(backend.clone() as Arc<dyn VectorIndex>),
            None,
            Some(backend.clone() as Arc<dyn FullTextIndex>),
            embedding,
        )
        .with_query_router(Box::new(RuleBasedRouter::new()));

        let _results = engine
            .search(&SearchRequest {
                query: "what is the default port?".into(),
                top_k: 5,
                strategy: SearchStrategy::Auto,
                filters: None,
                min_confidence: None,
            })
            .await
            .unwrap();

        assert!(engine.capabilities().has_vector);
        assert!(engine.capabilities().has_fulltext);
    }

    #[test]
    fn strategy_decomposition() {
        let s = SearchStrategy::FullHybrid {
            vector_weight: 0.4,
            text_weight: 0.4,
            graph_weight: 0.2,
            graph_expansion_depth: 1,
        };
        let (v, t, g, et, _) = decompose_strategy(&s, 10);
        assert_eq!(v, 30);
        assert_eq!(t, 30);
        assert_eq!(g, 1);
        assert!(et.iter().any(|e| *e == EdgeType::Contains));

        let s = SearchStrategy::TextOnly;
        let (v, t, g, et, _) = decompose_strategy(&s, 10);
        assert_eq!(v, 0);
        assert_eq!(t, 30);
        assert_eq!(g, 0);
        assert!(et.is_empty());
    }

    // ── Phase 1 regression tests ──

    #[tokio::test]
    async fn search_relevant_query_returns_results() {
        let backend = Arc::new(InMemoryBackend::new());
        let embedding = Arc::new(MockEmbedding { ndims: 1 });

        let (doc, chunks) = make_test_doc(
            "rust-doc",
            "Rust is a systems programming language. It provides memory safety without garbage collection.",
        );
        backend.store(doc.clone(), chunks.clone()).await.unwrap();
        backend
            .upsert(&[VectorEntry {
                id: chunks[0].id.clone(),
                document_id: doc.id.clone(),
                vector: vec![0.1],
                text: chunks[0].text.clone(),
            }])
            .await
            .unwrap();
        backend
            .index(&[FullTextEntry {
                id: chunks[0].id.clone(),
                document_id: doc.id.clone(),
                text: chunks[0].text.clone(),
            }])
            .await
            .unwrap();

        let engine = HybridSearchEngine::new(
            backend.clone() as Arc<dyn DocumentStore>,
            Some(backend.clone() as Arc<dyn VectorIndex>),
            None,
            Some(backend.clone() as Arc<dyn FullTextIndex>),
            embedding,
        )
        .with_query_router(Box::new(RuleBasedRouter::new()));

        let results = engine
            .search(&SearchRequest {
                query: "Rust programming".into(),
                top_k: 5,
                strategy: SearchStrategy::Hybrid {
                    vector_weight: 0.5,
                    text_weight: 0.5,
                },
                filters: None,
                min_confidence: None,
            })
            .await
            .unwrap();

        assert!(!results.is_empty(), "相关查询应返回结果");
        for r in &results {
            assert!(matches!(r.confidence, ConfidenceLevel::Medium));
        }
    }

    #[tokio::test]
    async fn search_result_has_confidence_field() {
        let backend = Arc::new(InMemoryBackend::new());
        let embedding = Arc::new(MockEmbedding { ndims: 1 });

        let (doc, chunks) = make_test_doc("conf-test", "Systems programming with Rust.");
        backend.store(doc, chunks).await.unwrap();

        let engine = HybridSearchEngine::new(
            backend.clone() as Arc<dyn DocumentStore>,
            Some(backend.clone() as Arc<dyn VectorIndex>),
            None,
            Some(backend.clone() as Arc<dyn FullTextIndex>),
            embedding,
        )
        .with_query_router(Box::new(RuleBasedRouter::new()));

        let results = engine
            .search(&SearchRequest {
                query: "Rust".into(),
                top_k: 3,
                strategy: SearchStrategy::Hybrid {
                    vector_weight: 0.5,
                    text_weight: 0.5,
                },
                filters: None,
                min_confidence: None,
            })
            .await
            .unwrap();

        for r in &results {
            assert!(r.diagnostic.is_none());
            assert!(matches!(r.confidence, ConfidenceLevel::Medium));
        }
    }

    // ── Phase 2 自适应管道测试 ──

    #[tokio::test]
    async fn adaptive_pipeline_enabled_with_query_analyzer() {
        let backend = Arc::new(InMemoryBackend::new());
        let embedding = Arc::new(MockEmbedding { ndims: 1 });

        let (doc, chunks) = make_test_doc("adapt-1", "Rust systems programming guide.");
        backend.store(doc, chunks).await.unwrap();

        let engine = HybridSearchEngine::new(
            backend.clone() as Arc<dyn DocumentStore>,
            Some(backend.clone() as Arc<dyn VectorIndex>),
            None,
            Some(backend.clone() as Arc<dyn FullTextIndex>),
            embedding,
        )
        .with_query_router(Box::new(RuleBasedRouter::new()))
        .with_query_analyzer(Box::new(RuleBasedAnalyzer::new()));

        assert!(engine.has_adaptive_pipeline());

        let _results = engine
            .search(&SearchRequest {
                query: "Rust programming language".into(),
                top_k: 5,
                strategy: SearchStrategy::Hybrid {
                    vector_weight: 0.5,
                    text_weight: 0.5,
                },
                filters: None,
                min_confidence: None,
            })
            .await
            .unwrap();

        // 使用 MockEmbedding（所有向量相同），所有向量分数均为 1.0，
        // 因此路径校准将由于平坦分布检测到无信号。
        // 这是正确的自适应行为 — 我们无法区分 mock 数据中的信号和噪声。
        // 在真实使用中，不同的文档会产生不同的向量和不同的分数。
    }

    #[tokio::test]
    async fn adaptive_pipeline_with_configurable_embedding() {
        let backend = Arc::new(InMemoryBackend::new());
        // 使用可配置的 mock 来测试自适应管道行为。
        let embedding = Arc::new(MockEmbeddingConfigurable::new(4, vec![1.0, 0.0, 0.0, 0.0]));

        let (doc, chunks) = make_test_doc("ad-1", "分布式系统设计原理与实践指南");
        backend.store(doc.clone(), chunks.clone()).await.unwrap();
        backend
            .upsert(&[VectorEntry {
                id: chunks[0].id.clone(),
                document_id: doc.id.clone(),
                vector: chunks[0].embedding.clone(),
                text: chunks[0].text.clone(),
            }])
            .await
            .unwrap();
        backend
            .index(&[FullTextEntry {
                id: chunks[0].id.clone(),
                document_id: doc.id.clone(),
                text: chunks[0].text.clone(),
            }])
            .await
            .unwrap();

        let engine = HybridSearchEngine::new(
            backend.clone() as Arc<dyn DocumentStore>,
            Some(backend.clone() as Arc<dyn VectorIndex>),
            None,
            Some(backend.clone() as Arc<dyn FullTextIndex>),
            embedding,
        )
        .with_query_router(Box::new(RuleBasedRouter::new()))
        .with_query_analyzer(Box::new(RuleBasedAnalyzer::new()));

        let _results = engine
            .search(&SearchRequest {
                query: "分布式系统".into(),
                top_k: 5,
                strategy: SearchStrategy::Auto,
                filters: None,
                min_confidence: None,
            })
            .await
            .unwrap();

        // 调用不应 panic — 自适应管道应优雅地处理校准。
    }

    #[test]
    fn adaptive_fusion_config_mappings() {
        let cals = vec![];
        let cfg = adaptive_fusion_config(CrossValidation::StrongAgreement, &cals);
        assert_eq!(cfg.min_rrf_score, 0.002);
        assert!(cfg.require_cross_validation);

        let cfg = adaptive_fusion_config(CrossValidation::WeakAgreement, &cals);
        assert_eq!(cfg.min_rrf_score, 0.003);

        let cfg = adaptive_fusion_config(CrossValidation::SinglePath, &cals);
        assert_eq!(cfg.min_rrf_score, 0.005);
        assert!(!cfg.require_cross_validation);
    }

    #[test]
    fn confidence_mapping() {
        assert_eq!(
            confidence_from_cv(CrossValidation::StrongAgreement),
            ConfidenceLevel::High
        );
        assert_eq!(
            confidence_from_cv(CrossValidation::WeakAgreement),
            ConfidenceLevel::High
        );
        assert_eq!(
            confidence_from_cv(CrossValidation::SinglePath),
            ConfidenceLevel::Medium
        );
        assert_eq!(
            confidence_from_cv(CrossValidation::NoSignal),
            ConfidenceLevel::None
        );
    }

    #[test]
    fn default_engine_has_no_adaptive_pipeline() {
        let backend = Arc::new(InMemoryBackend::new());
        let embedding = Arc::new(MockEmbedding { ndims: 1 });
        let engine = HybridSearchEngine::new(
            backend as Arc<dyn DocumentStore>,
            None,
            None,
            None,
            embedding,
        );
        assert!(!engine.has_adaptive_pipeline());
    }

    #[test]
    fn builder_methods_chain() {
        let backend = Arc::new(InMemoryBackend::new());
        let embedding = Arc::new(MockEmbedding { ndims: 1 });
        let engine = HybridSearchEngine::new(
            backend as Arc<dyn DocumentStore>,
            None,
            None,
            None,
            embedding,
        )
        .with_query_router(Box::new(RuleBasedRouter::new()))
        .with_query_analyzer(Box::new(RuleBasedAnalyzer::new()))
        .with_model_config(EmbeddingModelConfig::bge_small_zh());

        assert!(engine.has_adaptive_pipeline());
    }
}
