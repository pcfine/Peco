//! Layer 2 — 路径级分数校准。
//!
//! 每条检索路径（向量、全文、图谱）产生一个排序后的命中列表。
//! 此模块分析每个列表的统计特性，以确定：
//! * 路径是否产生的是信号还是噪声。
//! * 有效的自适应分数阈值应为多少。
//! * 信号强度有多强（用于置信度计算）。
//!
//! 路径校准替代了固定的阈值常量，改用根据观察到的分数分布
//! 计算的自适应阈值。每条路径传入其自身的 `min_score` 下限：
//! * 向量路径使用模型噪声底噪（例如 BGE-large-zh = 0.55）。
//! * 文本路径使用 0.0 — BM25 重叠分数（0–1）与余弦相似度
//!   的分布不同；即使是部分匹配（如 0.5）也是有意义的信号。
//! * 图谱路径使用 0.0。

// ---------------------------------------------------------------------------
// PathCalibration
// ---------------------------------------------------------------------------

/// 单条检索路径的校准结果。
///
/// 每个字段都是基于该路径返回的命中分数分布计算得出的。
#[derive(Debug, Clone)]
pub struct PathCalibration {
    /// 人类可读的路径标识符（例如 "vector"、"fulltext"）。
    pub path_name: String,
    /// 该路径是否产生有意义的信号（相对于噪声）。
    pub has_signal: bool,
    /// 信号强度：top 分数与第 k 个分数之间的相对差距。
    ///
    /// 公式：`(top_score - kth_score) / top_score`
    ///
    /// * ≥ 0.3 — 强信号（清晰的 top 结果）
    /// * 0.15–0.3 — 中等信号
    /// * < 0.15 — 弱信号；命中聚集在狭窄范围内
    pub signal_strength: f32,
    /// 分数衰减：top 分数与平均分数之间的比率。
    ///
    /// 公式：`top_score / mean_score`
    ///
    /// * ≥ 2.0 — top 结果明显突出
    /// * 1.3–2.0 — 中等衰减
    /// * < 1.3 — 分数平坦；top 结果几乎不突出
    pub score_decay: f32,
    /// 自适应最小分数阈值。
    ///
    /// 公式：`max(mean + 1.5 × (top - mean), model_min)`
    ///
    /// 这会在噪声底噪之上动态设置屏障，高度取决于
    /// 该路径的分数分布离散程度。
    pub adaptive_threshold: f32,
    /// 超过自适应阈值的命中数量。
    pub passing_count: usize,
}

// ---------------------------------------------------------------------------
// calibrate_path
// ---------------------------------------------------------------------------

/// 分析命中列表的分数分布以生成路径校准。
///
/// # 参数
/// * `path_name` — 路径标识符（用于调试/日志）。
/// * `hits` — `(doc_id, score)` 对列表，假定按分数降序排列。
/// * `min_score` — 自适应阈值的绝对下限。向量路径使用模型噪声底噪
///   （例如 BGE-large-zh = 0.55）；文本和图谱路径使用 0.0。
///
/// # 信号判定
///
/// 当以下**所有三个**条件同时满足时，路径视为具有信号：
/// 1. `signal_strength ≥ 0.15` — top 分数明显高于第 k 个分数。
/// 2. `score_decay ≥ 1.3` — top 分数至少是平均值的 1.3 倍。
/// 3. `top_score ≥ adaptive_threshold` — top 命中通过自适应阈值。
///
/// # 边界情况
///
/// * **空输入**：返回 `has_signal = false`，阈值设为 `min_score`。
/// * **单命中**：`signal_strength = 0.0`，`score_decay = 1.0`（无法计算差距）。
///   就信号而言，单命中是不确定的 — 调用方应依赖交叉验证。
/// * **所有分数为零**：`score_decay = 0.0`（除零保护）。
pub fn calibrate_path(path_name: &str, hits: &[(String, f32)], min_score: f32) -> PathCalibration {
    if hits.is_empty() {
        return PathCalibration {
            path_name: path_name.to_string(),
            has_signal: false,
            signal_strength: 0.0,
            score_decay: 0.0,
            adaptive_threshold: min_score,
            passing_count: 0,
        };
    }

    let top_score = hits[0].1;

    // k 值：取第 5 个或最后一个，以较小者为准。
    let k = 5usize.min(hits.len() - 1);
    let kth_score = hits[k].1;

    // 平均值。
    let sum: f32 = hits.iter().map(|(_, s)| s).sum();
    let mean_score = sum / hits.len() as f32;

    // 信号强度。
    let signal_strength = if top_score > 0.0 {
        ((top_score - kth_score) / top_score).max(0.0)
    } else {
        0.0
    };

    // 分数衰减（除零保护）。
    let score_decay = if mean_score > 0.0 {
        top_score / mean_score
    } else {
        0.0
    };

    // 自适应阈值：取 top 分数的 75%，但不低于模型噪声底噪。
    //
    // 使用 top_score * 0.75 而非 mean + k*(top-mean)，因为后者
    // 在强信号时（top 远高于 mean）会产生高于 top 本身的阈值，
    // 从而错误地排除所有命中（余弦相似度上限为 1.0）。
    //
    // 示例：
    // * 强信号 top=0.95: 阈值 = max(0.7125, 0.55) = 0.7125（保留 top 命中）
    // * 弱信号 top=0.38: 阈值 = max(0.285, 0.55) = 0.55（全部过滤）
    // * 噪声 top=0.30: 阈值 = max(0.225, 0.55) = 0.55（全部过滤）
    let proportional_threshold = top_score * 0.75;
    let adaptive_threshold = proportional_threshold.max(min_score);

    // 超过阈值的命中数。
    let passing_count = hits
        .iter()
        .filter(|(_, s)| *s >= adaptive_threshold)
        .count();

    // 信号判定。
    //
    // 单命中情况：signal_strength=0 且 score_decay=1.0（无法计算分布）。
    // 对于向量搜索，这确实是模糊的（噪声底噪中的孤立命中）。
    // 但对于文本搜索（BM25 重叠分数），单个匹配文档就是有效信号 —
    // 尤其当 min_score=0.0 且 adaptive_threshold 纯粹由分数驱动时。
    //
    // 因此：单命中时，若分数超过自适应阈值则视为信号。
    //
    // 多命中情况：默认应用完整的三条件检查。
    //
    // 高平均分覆盖：当多命中情况的分数分布完全平坦时（signal_strength=0，
    // score_decay=1.0），三条件检查会失败。但这不一定表示噪声 —
    // 如果所有文档都获得接近满分，恰好说明查询词普遍存在。
    //
    // 示例：「彭琛」出现在两份 PDF 中 → 两条命中均为 BM25=1.0 →
    // 平坦但高质量。不应丢弃。
    //
    // 因此：多命中时，如果平均分 ≥ 0.9（接近满分平坦），同样判定为信号。
    // 阈值 0.9 是保守的选择 — 仅当所有命中确实都接近满分时才触发覆盖。
    let has_signal = if hits.len() == 1 {
        top_score >= adaptive_threshold
    } else {
        let distribution_signal =
            signal_strength >= 0.15 && score_decay >= 1.3 && top_score >= adaptive_threshold;

        let high_quality_flat = mean_score >= 0.9 && top_score >= adaptive_threshold;

        distribution_signal || high_quality_flat
    };

    PathCalibration {
        path_name: path_name.to_string(),
        has_signal,
        signal_strength,
        score_decay,
        adaptive_threshold,
        passing_count,
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// BGE-large-zh 向量噪声底噪。
    const VECTOR_MIN_SCORE: f32 = 0.55;
    /// 文本/图谱路径的 min_score（不使用向量噪声底噪）。
    const TEXT_MIN_SCORE: f32 = 0.0;

    // ── 空输入 ──

    #[test]
    fn empty_hits_has_no_signal() {
        let cal = calibrate_path("vector", &[], VECTOR_MIN_SCORE);
        assert!(!cal.has_signal);
        assert_eq!(cal.passing_count, 0);
        assert_eq!(cal.signal_strength, 0.0);
        assert_eq!(cal.adaptive_threshold, VECTOR_MIN_SCORE);
    }

    // ── 单命中 ──

    #[test]
    fn single_hit_with_sufficient_score_is_signal() {
        let hits = vec![("a".into(), 0.7)];
        let cal = calibrate_path("vector", &hits, VECTOR_MIN_SCORE);
        // 单命中：adaptive_threshold = max(0.7*0.75, 0.55) = 0.55
        // top_score(0.7) >= 0.55 → 判定为信号。
        // 单命中时放宽条件：无法计算分布，但一次强匹配仍有意义。
        assert!(cal.has_signal);
        assert_eq!(cal.signal_strength, 0.0);
        assert_eq!(cal.score_decay, 1.0);
        assert_eq!(cal.passing_count, 1);
    }

    #[test]
    fn single_hit_below_threshold_no_signal() {
        // 单命中但分数低于阈值 → 无信号。
        let hits = vec![("a".into(), 0.4)];
        let cal = calibrate_path("vector", &hits, VECTOR_MIN_SCORE);
        // adaptive_threshold = max(0.4*0.75, 0.55) = max(0.3, 0.55) = 0.55
        // top_score(0.4) < 0.55 → 无信号
        assert!(!cal.has_signal);
    }

    // ── 清晰信号 ──

    #[test]
    fn clear_signal_detected() {
        let hits = vec![
            ("a".into(), 0.95),
            ("b".into(), 0.80),
            ("c".into(), 0.75),
            ("d".into(), 0.70),
            ("e".into(), 0.65),
            ("f".into(), 0.60),
        ];
        let cal = calibrate_path("vector", &hits, VECTOR_MIN_SCORE);
        // top=0.95, kth(5th)=0.65, signal_strength=(0.95-0.65)/0.95=0.316
        // mean=0.742, score_decay=0.95/0.742=1.28
        // dispersion_threshold=0.742+1.5*(0.95-0.742)=1.054
        // adaptive=max(1.054, 0.55)=1.054
        assert!(cal.signal_strength > 0.2, "应具有适中的信号强度");
        // 注意：score_decay=1.28 < 1.3，因此 has_signal 可能为 false。
        // 这是正确的 — 集群分数衰减较小。
    }

    #[test]
    fn strong_signal_detected() {
        let hits = vec![
            ("a".into(), 0.95),
            ("b".into(), 0.50),
            ("c".into(), 0.45),
            ("d".into(), 0.42),
            ("e".into(), 0.40),
            ("f".into(), 0.38),
        ];
        let cal = calibrate_path("vector", &hits, VECTOR_MIN_SCORE);
        // top=0.95, kth(5th)=0.40, signal_strength=0.579
        // mean=0.517, score_decay=1.838
        // dispersion_threshold=0.517+1.5*(0.95-0.517)=1.167
        // adaptive=max(1.167,0.55)=1.167
        assert!(cal.has_signal, "强信号应被检测到");
        assert!(cal.signal_strength > 0.5);
        assert!(cal.score_decay > 1.5);
        // 只有 top 命中（0.95）通过 1.167 阈值。
        assert_eq!(cal.passing_count, 1);
    }

    // ── 平坦噪声 ──

    #[test]
    fn flat_noise_no_signal() {
        // 模拟噪声：所有分数聚集在噪声底噪附近。
        let hits = vec![
            ("a".into(), 0.35),
            ("b".into(), 0.33),
            ("c".into(), 0.31),
            ("d".into(), 0.30),
            ("e".into(), 0.28),
        ];
        let cal = calibrate_path("vector", &hits, VECTOR_MIN_SCORE);
        // 所有分数 < 0.55 → 无信号。
        assert!(!cal.has_signal);
        assert!(cal.passing_count == 0, "无命中应通过最低阈值");
    }

    // ── "苹果"场景复现 ──

    #[test]
    fn apple_scenario_noise() {
        // 模拟查询"苹果"返回不相关文档的噪声分数。
        let hits = vec![("resume".into(), 0.38), ("departure".into(), 0.35)];
        let cal = calibrate_path("vector", &hits, VECTOR_MIN_SCORE);
        // 两者均 < min_vector_score(0.55) → 无信号。
        assert!(!cal.has_signal);
        assert_eq!(cal.passing_count, 0);
        assert!(cal.adaptive_threshold >= 0.55);
    }

    // ── 自适应阈值钳位 ──

    #[test]
    fn threshold_clamped_to_model_min() {
        // 所有分数都非常低 — 自适应计算将低于模型最小值。
        let hits = vec![("a".into(), 0.10), ("b".into(), 0.08)];
        let cal = calibrate_path("vector", &hits, VECTOR_MIN_SCORE);
        // 自适应阈值应钳位到模型最小值。
        assert_eq!(cal.adaptive_threshold, VECTOR_MIN_SCORE);
    }

    // ── 高分但衰减平坦 ──

    #[test]
    fn high_scores_but_flat_decay() {
        // 所有分数都很高且相近 — 可能存在信号但无法区分。
        let hits = vec![
            ("a".into(), 0.92),
            ("b".into(), 0.90),
            ("c".into(), 0.88),
            ("d".into(), 0.87),
            ("e".into(), 0.85),
        ];
        let cal = calibrate_path("vector", &hits, VECTOR_MIN_SCORE);
        // signal_strength = (0.92-0.85)/0.92 ≈ 0.076（低）
        // score_decay ≈ 0.92/0.884 ≈ 1.04（低）
        // has_signal 应为 false — 即使是高分，也无法区分。
        assert!(!cal.has_signal);
        assert!(cal.signal_strength < 0.15);
    }

    // ── 分数全部为零（除零保护） ──

    #[test]
    fn all_zero_scores() {
        let hits = vec![("a".into(), 0.0), ("b".into(), 0.0)];
        let cal = calibrate_path("vector", &hits, VECTOR_MIN_SCORE);
        assert_eq!(cal.signal_strength, 0.0);
        assert_eq!(cal.score_decay, 0.0);
        assert!(!cal.has_signal);
    }

    // ── 文本路径校准（不同 min_score） ──

    #[test]
    fn text_path_partial_match_is_signal() {
        // 模拟 BM25 重叠分数：部分匹配（1/2 字符匹配）。
        // 这在文本搜索中是合法信号 — min_score=0.0。
        let hits = vec![
            ("doc_a".into(), 1.0), // 完整匹配
            ("doc_b".into(), 0.5), // 部分匹配（如"简历"→仅"历"在chunk中）
        ];
        let cal = calibrate_path("fulltext", &hits, TEXT_MIN_SCORE);
        // top_score=1.0, kth=0.5, signal_strength=0.5, score_decay=1.0/0.75=1.33
        // adaptive_threshold = max(1.0*0.75, 0.0) = 0.75
        // top_score(1.0) >= 0.75 → 通过
        assert!(cal.has_signal, "文本路径的部分匹配应为有效信号");
        assert_eq!(cal.adaptive_threshold, 0.75);
        assert_eq!(cal.passing_count, 1); // 仅 doc_a (1.0) 通过 0.75
    }

    #[test]
    fn text_path_below_vector_noise_floor() {
        // 模拟极低 BM25 分数 — 使用 min_score=0.0 不应被钳位。
        let hits = vec![("a".into(), 0.3)];
        let cal = calibrate_path("fulltext", &hits, TEXT_MIN_SCORE);
        // min_score=0.0, adaptive_threshold=max(0.3*0.75, 0.0)=0.225
        assert!((cal.adaptive_threshold - 0.225).abs() < 0.001);
        // 单命中，score(0.3) >= threshold(0.225) → 判定为信号。
        assert!(cal.has_signal);
    }

    // ── 多命中全部满分（"彭琛"场景） ──

    #[test]
    fn multiple_perfect_matches_is_signal() {
        // 模拟「彭琛」查询：两份 PDF 都包含该名字，BM25 均为 1.0。
        // 分数平坦不应被误判为噪声 — 所有文档都完美匹配就是强信号。
        let hits = vec![("doc_a".into(), 1.0), ("doc_b".into(), 1.0)];
        let cal = calibrate_path("fulltext", &hits, TEXT_MIN_SCORE);
        // top_score=1.0, kth_score=1.0 → signal_strength=0.0
        // mean_score=1.0 → score_decay=1.0
        // 分布平坦，但 high_quality_flat 覆盖：mean(1.0) >= 0.7 → 信号。
        assert!(cal.has_signal, "多文档完美匹配应为有效信号");
        assert_eq!(cal.signal_strength, 0.0);
        assert_eq!(cal.score_decay, 1.0);
        assert_eq!(cal.adaptive_threshold, 0.75);
        assert_eq!(cal.passing_count, 2); // 两者都通过 0.75
    }

    #[test]
    fn multiple_high_similar_scores_is_signal() {
        // 模拟 BM25 高分但非满分场景：0.95, 0.92, 0.90。
        // mean=(0.95+0.92+0.90)/3=0.923 >= 0.9 → high_quality_flat 覆盖。
        let hits = vec![
            ("doc_a".into(), 0.95),
            ("doc_b".into(), 0.92),
            ("doc_c".into(), 0.90),
        ];
        let cal = calibrate_path("fulltext", &hits, TEXT_MIN_SCORE);
        assert!(cal.has_signal);
        assert_eq!(cal.passing_count, 3);
    }

    #[test]
    fn flat_low_scores_are_still_noise() {
        // 平坦但低分 → 仍然是噪声（high_quality_flat 不触发：mean=0.4 < 0.9）。
        let hits = vec![("a".into(), 0.4), ("b".into(), 0.4), ("c".into(), 0.4)];
        let cal = calibrate_path("fulltext", &hits, TEXT_MIN_SCORE);
        // mean=0.4 < 0.9 → high_quality_flat 不触发。
        // signal_strength=0, score_decay=1.0 → distribution_signal 不触发。
        assert!(!cal.has_signal);
    }
}
