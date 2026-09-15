// ============================================================================
// 记忆去重与重排 — 读写双路径共享的相似度工具
// ============================================================================
//
// 写路径（hook.rs）用 [`max_cosine`] 判断「新提取的事实是否与既有同类目
// 记忆近重复」；读路径（recall.rs）用 [`greedy_dedup_indices`] 剔除同一批
// 命中里的近重复条目，再用 [`rerank_score`] 重排。两条路径共用同一份余弦
// 实现（`knowledge_base::engine::cosine_similarity`）与同一批权重常量，
// 避免判定口径分叉。
//
// 门控：`ConsolidationConfig.dedup_enforce`（默认 `false` = shadow）。
// shadow 下写路径照常写入、读路径只重排不去重 —— 标定报告人工抽检通过前
// 必须保持 `false`。
//
// 非致命性：本模块全部是纯函数，不产生 IO、不上抛错误；调用方在嵌入
// 不可用时降级为「放行 / 原序」，去重永不阻塞读写主链路。

use chrono::{DateTime, Utc};

use super::consolidation::time_from_parts;

/// 重排权重：profile 最稳定，semantic 次之，episodic 最低。
///
/// 只做相对排序，绝对量级不参与任何阈值判定。
pub fn category_weight(source_path: &str) -> f32 {
    match source_path {
        "ppa_profile" => 1.2,
        "ppa_semantic" => 1.1,
        "ppa_episodic" => 1.0,
        // 非记忆来源（不应出现于 @private_memory）按中性权重处理
        _ => 1.0,
    }
}

/// 时间衰减因子：只作用于 episodic，`0.5^(age_days / half_life_days)`。
///
/// 无时间源（title 毫秒与 captured-at footer 皆不可读）→ `1.0` 不衰减
/// （无数据不判定）；`half_life_days == 0` 关闭衰减；未来时刻按 `age = 0`
/// 处理（不因时钟偏差放大小于 1 的权重）。
pub fn recency_factor(
    source_path: &str,
    title: &str,
    content: &str,
    now: DateTime<Utc>,
    half_life_days: u64,
) -> f32 {
    if source_path != "ppa_episodic" || half_life_days == 0 {
        return 1.0;
    }
    let Some(t) = time_from_parts(None, title, content) else {
        return 1.0;
    };
    let age_days = (now - t).num_seconds().max(0) as f64 / 86_400.0;
    0.5f64.powf(age_days / half_life_days as f64) as f32
}

/// 读路径重排分数：`原始 score * 类别权重 * 时间衰减`。
pub fn rerank_score(
    score: f32,
    source_path: &str,
    title: &str,
    content: &str,
    now: DateTime<Utc>,
    half_life_days: u64,
) -> f32 {
    score
        * category_weight(source_path)
        * recency_factor(source_path, title, content, now, half_life_days)
}

/// 单条向量对一组候选向量的最大余弦。
///
/// 候选为空 → `0.0`（阈值 `dedup_cos` 恒为正，等价于「不判重」）。
pub fn max_cosine(v: &[f32], others: &[Vec<f32>]) -> f32 {
    others
        .iter()
        .map(|o| knowledge_base::engine::cosine_similarity(v, o))
        .fold(0.0f32, f32::max)
}

/// 贪心近重复剔除（纯函数）：按 `order` 顺序扫描，与任一已保留条目余弦
/// `>= dedup_cos` 者丢弃，否则保留。
///
/// `order` 是候选下标序列，调用方保证已按优先级（score）降序 —— 因此
/// 「保留先到者」等价于「近重复只留 score 最高一条」。
pub fn greedy_dedup_indices(vectors: &[Vec<f32>], order: &[usize], dedup_cos: f32) -> Vec<usize> {
    let mut kept: Vec<usize> = Vec::with_capacity(order.len());
    for &i in order {
        let Some(vi) = vectors.get(i) else {
            continue;
        };
        let is_dup = kept.iter().any(|&j| {
            vectors
                .get(j)
                .is_some_and(|vj| knowledge_base::engine::cosine_similarity(vi, vj) >= dedup_cos)
        });
        if !is_dup {
            kept.push(i);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-01T00:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// `t0` 之后 `days` 天的时刻。
    fn after(days: i64) -> DateTime<Utc> {
        t0() + chrono::Duration::days(days)
    }

    fn title_at(days: i64) -> String {
        format!("memory_{}_0", after(days).timestamp_millis())
    }

    #[test]
    fn category_weight_prefers_profile_over_episodic() {
        assert!(category_weight("ppa_profile") > category_weight("ppa_semantic"));
        assert!(category_weight("ppa_semantic") > category_weight("ppa_episodic"));
        // 非记忆来源中性，不参与抬升
        assert!((category_weight("uploaded/doc.md") - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn recency_factor_only_decays_episodic() {
        let now = t0();
        let old = title_at(-365);

        // 一年前的 episodic 半衰期 30 天 → 0.5^12 ≈ 0.000244
        let f = recency_factor("ppa_episodic", &old, "事件", now, 30);
        assert!(f < 0.001, "陈旧 episodic 应大幅衰减，实际 {f}");
        // profile / semantic 同一年龄不衰减
        assert!((recency_factor("ppa_profile", &old, "偏好", now, 30) - 1.0).abs() < f32::EPSILON);
        assert!((recency_factor("ppa_semantic", &old, "事实", now, 30) - 1.0).abs() < f32::EPSILON);

        // 半衰期边界：恰一个半衰期 → 0.5
        assert!(
            (recency_factor("ppa_episodic", &title_at(-30), "事件", now, 30) - 0.5).abs() < 1e-4
        );
    }

    #[test]
    fn recency_factor_is_neutral_without_time_source() {
        let now = t0();
        // 标题不可解析、正文无 captured-at footer → 不衰减（无数据不判定）
        assert!(
            (recency_factor("ppa_episodic", "日语学习记录", "普通记忆内容", now, 30) - 1.0).abs()
                < f32::EPSILON
        );
        // 半衰期 0 → 关闭衰减
        assert!(
            (recency_factor("ppa_episodic", &title_at(-365), "事件", now, 0) - 1.0).abs()
                < f32::EPSILON
        );
        // 未来时刻 → age 按 0 处理，不放大
        assert!(
            (recency_factor("ppa_episodic", &title_at(10), "事件", now, 30) - 1.0).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn recency_factor_reads_captured_at_footer() {
        // 标题自拟（@memory 合并产物形态）→ footer 兜底
        let content = format!("搬家的记录\n[captured-at: {}]", after(-60).to_rfc3339());
        let f = recency_factor("ppa_episodic", "搬家回忆", &content, t0(), 30);
        assert!(
            (f - 0.25).abs() < 1e-3,
            "60 天 = 两个半衰期 → 0.25，实际 {f}"
        );
    }

    #[test]
    fn rerank_score_profile_beats_stale_episodic() {
        let now = t0();
        let stale_episodic = title_at(-365);

        // 同分：新鲜 profile 恒高于陈旧 episodic
        let profile = rerank_score(0.8, "ppa_profile", &title_at(0), "偏好", now, 30);
        let episodic = rerank_score(0.8, "ppa_episodic", &stale_episodic, "事件", now, 30);
        assert!(
            profile > episodic,
            "profile {profile} 应高于陈旧 episodic {episodic}"
        );

        // 权重抬升有限：陈旧的 episodic 仍可能被语义更相关的新 episodic 压过
        let fresh_episodic = rerank_score(1.0, "ppa_episodic", &title_at(0), "事件", now, 30);
        assert!(fresh_episodic > profile, "相关性差距足够大时不受权重左右");
    }

    #[test]
    fn max_cosine_picks_same_category_best_match() {
        let v = vec![1.0f32, 0.0];
        assert_eq!(max_cosine(&v, &[]), 0.0, "无候选 → 不判重");
        assert!((max_cosine(&v, &[vec![1.0, 0.0]]) - 1.0).abs() < 1e-6);
        // 取最大而非第一个
        let others = vec![vec![0.0, 1.0], vec![1.0, 0.0], vec![0.5, 0.5]];
        assert!((max_cosine(&v, &others) - 1.0).abs() < 1e-6);
        // 全负相关也回落到 0.0（阈值恒正，等价不判重）
        assert_eq!(max_cosine(&v, &[vec![-1.0, 0.0]]), 0.0);
    }

    #[test]
    fn greedy_dedup_keeps_highest_priority_of_near_duplicates() {
        // 0~1 近重复（≈1.0）、2 与二者正交
        let vectors = vec![vec![1.0f32, 0.0], vec![1.0, 0.0], vec![0.0, 1.0]];
        // 优先级降序：1 先于 0
        assert_eq!(greedy_dedup_indices(&vectors, &[1, 0, 2], 0.92), vec![1, 2]);
        // 顺序反过来 → 保留 0
        assert_eq!(greedy_dedup_indices(&vectors, &[0, 1, 2], 0.92), vec![0, 2]);

        // 阈值以下不判重
        let far = vec![vec![1.0f32, 0.0], vec![0.6, 0.8]];
        assert_eq!(greedy_dedup_indices(&far, &[0, 1], 0.92), vec![0, 1]);

        // 越界下标静默跳过（调用方以检索结果为准，不 panic）
        assert_eq!(greedy_dedup_indices(&vectors, &[9, 2], 0.92), vec![2]);
    }
}
