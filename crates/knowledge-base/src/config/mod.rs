//! 嵌入模型配置 — 每种模型微调参数的集中定义。
//!
//! 不同嵌入模型具有不同的嵌入空间特性（维度、噪声底噪、查询指令格式）。
//! 此模块为每个受支持的模型提供预设配置，并为检索引擎提供
//! 单一的真相来源。

// ---------------------------------------------------------------------------
// EmbeddingModelConfig
// ---------------------------------------------------------------------------

/// 特定嵌入模型的配置参数。
///
/// 每个变体编码了从实证评估中得出的模型特定行为：
/// * `min_vector_score` — 低于此分数的向量命中被视为噪声。
///   此值根据模型嵌入空间的噪声底噪设置。不同维度的模型
///   具有不同的不相关文档对余弦相似度分布。
/// * `query_instruction` — BGE 风格模型在嵌入查询与嵌入段落时
///   受益于任务特定的指令前缀。
#[derive(Debug, Clone)]
pub struct EmbeddingModelConfig {
    /// 人类可读的模型标识符（例如 "BGE-large-zh-v1.5"）。
    pub model_name: &'static str,
    /// 此模型产生的向量维度。
    pub ndims: usize,
    /// 向量余弦相似度的最低有效阈值。
    ///
    /// 低于此值的命中被视为噪声并丢弃。
    /// 设置值高于模型噪声底噪的上界。
    pub min_vector_score: f32,
    /// 嵌入查询时使用的指令前缀（如适用）。
    ///
    /// BGE 模型是在带有任务特定指令的数据上训练的。
    /// 为查询添加此前缀（而非段落）可产生更好的检索质量。
    /// 对于非 BGE 模型则为 `None`。
    pub query_instruction: Option<&'static str>,
    /// 是否针对中文文本进行了优化。
    pub chinese_optimized: bool,
}

impl EmbeddingModelConfig {
    // ---------------------------------------------------------------
    // 预设
    // ---------------------------------------------------------------

    /// BGE-large-zh-v1.5 — 最佳可用的中文嵌入模型（1024 维）。
    ///
    /// 噪声底噪：不相关文档对约 0.15–0.40。
    /// 相关文档对通常 ≥ 0.65。
    /// 阈值 0.55 高于噪声区间上界，同时保留了有意义的相关性。
    pub fn bge_large_zh() -> Self {
        Self {
            model_name: "BGE-large-zh-v1.5",
            ndims: 1024,
            min_vector_score: 0.55,
            query_instruction: Some("为这个句子生成表示以用于检索相关文章："),
            chinese_optimized: true,
        }
    }

    /// BGE-small-zh-v1.5 — 轻量级中文嵌入模型（512 维）。
    ///
    /// 噪声底噪：不相关文档对约 0.20–0.50。
    /// 由于维度较低，噪声分布比大型模型更广。
    /// 阈值 0.55 适用于两个 BGE 中文变体。
    pub fn bge_small_zh() -> Self {
        Self {
            model_name: "BGE-small-zh-v1.5",
            ndims: 512,
            min_vector_score: 0.55,
            query_instruction: Some("为这个句子生成表示以用于检索相关文章："),
            chinese_optimized: true,
        }
    }

    /// BGE-M3 — 顶级多语言嵌入模型（1024 维，稀疏+密集）。
    ///
    /// 当 fastembed 支持 BGE-M3 枚举变体时使用。
    /// 噪声底噪估计：不相关文档对约 0.12–0.35（待验证）。
    /// 阈值 0.50 是占位符 — 部署后根据实证噪声评估进行调整。
    pub fn bge_m3() -> Self {
        Self {
            model_name: "BGE-M3",
            ndims: 1024,
            min_vector_score: 0.50,
            query_instruction: Some("为这个句子生成表示以用于检索相关文章："),
            chinese_optimized: true,
        }
    }

    /// 通用英文嵌入模型的默认配置。
    ///
    /// 在已知模型特性之前使用保守阈值。
    pub fn default_english() -> Self {
        Self {
            model_name: "all-MiniLM-L6-v2",
            ndims: 384,
            min_vector_score: 0.50,
            query_instruction: None,
            chinese_optimized: false,
        }
    }
}

impl Default for EmbeddingModelConfig {
    /// 默认使用 BGE-large-zh-v1.5（当前最佳可用中文模型）。
    fn default() -> Self {
        Self::bge_large_zh()
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bge_large_zh_config() {
        let cfg = EmbeddingModelConfig::bge_large_zh();
        assert_eq!(cfg.ndims, 1024);
        assert!(cfg.min_vector_score > 0.4);
        assert!(cfg.chinese_optimized);
        assert!(cfg.query_instruction.is_some());
    }

    #[test]
    fn bge_small_zh_config() {
        let cfg = EmbeddingModelConfig::bge_small_zh();
        assert_eq!(cfg.ndims, 512);
        assert!(cfg.min_vector_score > 0.4);
        assert!(cfg.chinese_optimized);
    }

    #[test]
    fn bge_m3_config() {
        let cfg = EmbeddingModelConfig::bge_m3();
        assert_eq!(cfg.ndims, 1024);
        assert!(cfg.min_vector_score <= 0.55); // M3 噪声底噪更低
        assert!(cfg.chinese_optimized);
    }

    #[test]
    fn default_is_bge_large_zh() {
        let cfg = EmbeddingModelConfig::default();
        assert_eq!(cfg.ndims, 1024);
        assert!(cfg.chinese_optimized);
    }

    #[test]
    fn default_english_no_query_instruction() {
        let cfg = EmbeddingModelConfig::default_english();
        assert!(cfg.query_instruction.is_none());
        assert!(!cfg.chinese_optimized);
    }

    #[test]
    fn bge_large_overrides_small_different_ndims() {
        let large = EmbeddingModelConfig::bge_large_zh();
        let small = EmbeddingModelConfig::bge_small_zh();
        assert_ne!(large.ndims, small.ndims);
    }
}
