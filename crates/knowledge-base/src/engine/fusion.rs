use std::collections::HashMap;

use crate::traits::RrfConfig;

/// 倒数排名融合（Reciprocal Rank Fusion）。
///
/// 将多个排序列表（每个列表包含按文档的分数）融合为单个排序列表。
/// 公式：`score(d) = Σ weight_i / (k + rank_i(d))`。
///
/// # 参数
/// * `ranked_lists` — 每个列表是一个 `(weight, &[(doc_id, raw_score)])` 对。
/// * `config` — RRF 参数（k, min_score）。
pub fn rrf_fuse(
    ranked_lists: &[(f32, &[(String, f32)])],
    config: &RrfConfig,
) -> Vec<(String, f32)> {
    let mut score_map: HashMap<String, f32> = HashMap::new();

    for (weight, list) in ranked_lists {
        for (rank, (doc_id, _raw_score)) in list.iter().enumerate() {
            let rrf_score = *weight / (config.k + (rank as f32) + 1.0);
            *score_map.entry(doc_id.clone()).or_insert(0.0) += rrf_score;
        }
    }

    let mut fused: Vec<(String, f32)> = score_map.into_iter().collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // 应用 min_score 阈值。
    fused.retain(|(_, s)| *s >= config.min_score);

    fused
}

/// 当排名不可用时（仅有原始分数）的更简单加权分数融合。
///
/// 在应用权重之前，将每个列表内的分数归一化到 [0, 1]。
pub fn weighted_score_fuse(
    ranked_lists: &[(f32, &[(String, f32)])],
    min_score: f32,
) -> Vec<(String, f32)> {
    let mut score_map: HashMap<String, f32> = HashMap::new();

    for (weight, list) in ranked_lists {
        if list.is_empty() {
            continue;
        }

        // 归一化：每个列表的最大分数 → 1.0
        let max = list
            .iter()
            .map(|(_, s)| *s)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(1.0);

        for (doc_id, score) in list.iter() {
            let norm = if max > 0.0 { score / max } else { 0.0 };
            *score_map.entry(doc_id.clone()).or_insert(0.0) += weight * norm;
        }
    }

    let mut fused: Vec<(String, f32)> = score_map.into_iter().collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    fused.retain(|(_, s)| *s >= min_score);

    fused
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_single_list() {
        let list = vec![("a".into(), 0.9), ("b".into(), 0.8), ("c".into(), 0.7)];
        let config = RrfConfig::default();
        let fused = rrf_fuse(&[(1.0, list.as_slice())], &config);
        assert_eq!(fused[0].0, "a");
        assert_eq!(fused[1].0, "b");
        assert_eq!(fused[2].0, "c");
    }

    #[test]
    fn rrf_two_lists() {
        let list_a = vec![("x".into(), 0.9), ("y".into(), 0.8)];
        let list_b = vec![("y".into(), 0.9), ("z".into(), 0.7)];
        let config = RrfConfig {
            k: 60.0,
            min_score: 0.0,
        };
        let fused = rrf_fuse(
            &[(0.5, list_a.as_slice()), (0.5, list_b.as_slice())],
            &config,
        );
        // "y" 同时出现在两个列表中 → 融合分数更高。
        assert_eq!(fused[0].0, "y");
    }

    #[test]
    fn rrf_empty_input() {
        let config = RrfConfig::default();
        let fused = rrf_fuse(&[], &config);
        assert!(fused.is_empty());
    }

    #[test]
    fn rrf_min_score_filter() {
        let list = vec![("a".into(), 1.0)];
        let config = RrfConfig {
            k: 60.0,
            min_score: 0.5,
        };
        let fused = rrf_fuse(&[(1.0, list.as_slice())], &config);
        // 1.0 / (60 + 1) ≈ 0.0164 < 0.5 → 被过滤掉。
        assert!(fused.is_empty());
    }

    #[test]
    fn weighted_score_fuse_basic() {
        let list_a = vec![("a".into(), 0.8), ("b".into(), 0.4)];
        let list_b = vec![("c".into(), 0.9)];
        let fused = weighted_score_fuse(&[(0.5, list_a.as_slice()), (0.5, list_b.as_slice())], 0.0);
        // a: 0.5*(0.8/0.8) = 0.5, c: 0.5*(0.9/0.9) = 0.5, b: 0.5*(0.4/0.8) = 0.25
        let scores: HashMap<_, _> = fused.into_iter().collect();
        assert!((scores["a"] - 0.5).abs() < 0.01);
        assert!((scores["c"] - 0.5).abs() < 0.01);
        assert!((scores["b"] - 0.25).abs() < 0.01);
    }
}
