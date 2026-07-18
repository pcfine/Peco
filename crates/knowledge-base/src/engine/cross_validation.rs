//! Layer 3 — 跨路径信号交叉验证。
//!
//! 单条路径可能产生假阳性（例如，向量嵌入对不相关文档
//! 也返回非零余弦相似度）。交叉验证通过检查多条独立
//! 检索路径是否对相同文档达成一致来降低此风险。
//!
//! # 原理
//!
//! 当向量搜索和全文搜索都将同一文档排在 top-N 时，
//! 该文档相关的可能性远高于仅单条路径返回它时。
//! 这是两个正交信号（语义相似度 vs 词项重叠）的收敛。
//!
//! # 一致性级别
//!
//! * **StrongAgreement** — ≥2 条路径，top-1 文档相同。
//! * **WeakAgreement** — ≥2 条路径，top-3 中有共享文档。
//! * **SinglePath** — 仅 1 条路径有信号。
//! * **NoSignal** — 无路径有信号 → 返回空。

use crate::engine::score_calibration::PathCalibration;

// ---------------------------------------------------------------------------
// CrossValidation
// ---------------------------------------------------------------------------

/// 跨路径信号一致性评估。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossValidation {
    /// ≥2 条活跃路径，top-1 文档相同。
    StrongAgreement,
    /// ≥2 条活跃路径，top-3 中有共享文档。
    WeakAgreement,
    /// 仅 1 条路径有信号（或仅 1 条路径配置）。
    SinglePath,
    /// 无路径有信号 — 应以空结果短路。
    NoSignal,
}

// ---------------------------------------------------------------------------
// validate_signals
// ---------------------------------------------------------------------------

/// 评估所有检索路径之间的一致性。
///
/// # 参数
/// * `calibrations` — 每条路径的校准结果（来自 Layer 2）。
/// * `path_docs` — 每条路径的 `(doc_id, score)` 命中列表。
///   必须与 `calibrations` 的顺序和长度匹配。
///
/// # 返回
/// 表示总体信号一致性的 `CrossValidation` 变体。
///
/// # 行为
/// * 无活跃路径 → `NoSignal`。
/// * 1 条活跃路径 → `SinglePath`。
/// * ≥2 条活跃路径 + top-1 匹配 → `StrongAgreement`。
/// * ≥2 条活跃路径 + top-3 重叠 → `WeakAgreement`。
/// * ≥2 条活跃路径 + 无重叠 → `SinglePath`（降级：将每条路径视为独立）。
pub fn validate_signals(
    calibrations: &[PathCalibration],
    path_docs: &[&[(String, f32)]],
) -> CrossValidation {
    // 收集有信号的路径索引。
    let active: Vec<usize> = calibrations
        .iter()
        .enumerate()
        .filter(|(_, cal)| cal.has_signal)
        .map(|(i, _)| i)
        .collect();

    match active.len() {
        0 => CrossValidation::NoSignal,
        1 => CrossValidation::SinglePath,
        _ => {
            // 检查 top-1 重合度。
            let top1_sets: Vec<std::collections::HashSet<&str>> = active
                .iter()
                .filter_map(|&i| {
                    path_docs
                        .get(i)
                        .and_then(|hits| hits.first())
                        .map(|(id, _)| {
                            let mut s = std::collections::HashSet::new();
                            s.insert(id.as_str());
                            s
                        })
                })
                .collect();

            // 所有活跃路径的 top-1 文档是否相同？
            if top1_sets.len() >= 2 {
                let first = &top1_sets[0];
                let all_same = top1_sets.iter().all(|s| s == first);
                if all_same {
                    return CrossValidation::StrongAgreement;
                }
            }

            // 检查 top-3 重叠度。
            let top3_sets: Vec<std::collections::HashSet<&str>> = active
                .iter()
                .filter_map(|&i| {
                    path_docs
                        .get(i)
                        .map(|hits| hits.iter().take(3).map(|(id, _)| id.as_str()).collect())
                })
                .collect();

            if top3_sets.len() >= 2 {
                for i in 0..top3_sets.len() {
                    for j in (i + 1)..top3_sets.len() {
                        if top3_sets[i].intersection(&top3_sets[j]).next().is_some() {
                            return CrossValidation::WeakAgreement;
                        }
                    }
                }
            }

            // 无重叠 — 将每条路径视为独立（降级到 SinglePath）。
            CrossValidation::SinglePath
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn active_cal(name: &str) -> PathCalibration {
        PathCalibration {
            path_name: name.to_string(),
            has_signal: true,
            signal_strength: 0.5,
            score_decay: 2.0,
            adaptive_threshold: 0.55,
            passing_count: 3,
        }
    }

    fn inactive_cal(name: &str) -> PathCalibration {
        PathCalibration {
            path_name: name.to_string(),
            has_signal: false,
            signal_strength: 0.0,
            score_decay: 0.0,
            adaptive_threshold: 0.55,
            passing_count: 0,
        }
    }

    // ── NoSignal ──

    #[test]
    fn no_active_paths_is_no_signal() {
        let cals = vec![inactive_cal("vector"), inactive_cal("text")];
        let docs: Vec<&[(String, f32)]> = vec![&[], &[]];
        let cv = validate_signals(&cals, &docs);
        assert_eq!(cv, CrossValidation::NoSignal);
    }

    #[test]
    fn empty_calibrations_is_no_signal() {
        let cv = validate_signals(&[], &[]);
        assert_eq!(cv, CrossValidation::NoSignal);
    }

    // ── SinglePath ──

    #[test]
    fn single_active_path_is_single_path() {
        let cals = vec![active_cal("vector"), inactive_cal("text")];
        let arr: [(String, f32); 1] = [("doc_a".into(), 0.9)];
        let docs: Vec<&[(String, f32)]> = vec![&arr, &[]];
        let cv = validate_signals(&cals, &docs);
        assert_eq!(cv, CrossValidation::SinglePath);
    }

    // ── StrongAgreement ──

    #[test]
    fn same_top1_is_strong_agreement() {
        let cals = vec![active_cal("vector"), active_cal("text")];
        let arr1: [(String, f32); 2] = [("doc_x".into(), 0.95), ("doc_y".into(), 0.7)];
        let arr2: [(String, f32); 2] = [("doc_x".into(), 0.88), ("doc_z".into(), 0.5)];
        let docs: Vec<&[(String, f32)]> = vec![&arr1, &arr2];
        let cv = validate_signals(&cals, &docs);
        assert_eq!(cv, CrossValidation::StrongAgreement);
    }

    #[test]
    fn three_paths_all_agree_is_strong() {
        let cals = vec![
            active_cal("vector"),
            active_cal("text"),
            active_cal("graph"),
        ];
        let arr1: [(String, f32); 1] = [("doc_x".into(), 0.9)];
        let arr2: [(String, f32); 1] = [("doc_x".into(), 0.8)];
        let arr3: [(String, f32); 1] = [("doc_x".into(), 0.3)];
        let docs: Vec<&[(String, f32)]> = vec![&arr1, &arr2, &arr3];
        let cv = validate_signals(&cals, &docs);
        assert_eq!(cv, CrossValidation::StrongAgreement);
    }

    // ── WeakAgreement ──

    #[test]
    fn top3_overlap_is_weak_agreement() {
        let cals = vec![active_cal("vector"), active_cal("text")];
        let arr1: [(String, f32); 3] = [("a".into(), 0.9), ("b".into(), 0.8), ("c".into(), 0.7)];
        let arr2: [(String, f32); 3] = [("x".into(), 0.9), ("c".into(), 0.8), ("y".into(), 0.7)];
        let docs: Vec<&[(String, f32)]> = vec![&arr1, &arr2];
        let cv = validate_signals(&cals, &docs);
        assert_eq!(cv, CrossValidation::WeakAgreement);
    }

    // ── 多条路径无重叠 → SinglePath ──

    #[test]
    fn multiple_paths_no_overlap_is_single_path() {
        let cals = vec![active_cal("vector"), active_cal("text")];
        let arr1: [(String, f32); 3] = [("a".into(), 0.9), ("b".into(), 0.8), ("c".into(), 0.7)];
        let arr2: [(String, f32); 3] = [("x".into(), 0.9), ("y".into(), 0.8), ("z".into(), 0.7)];
        let docs: Vec<&[(String, f32)]> = vec![&arr1, &arr2];
        let cv = validate_signals(&cals, &docs);
        assert_eq!(cv, CrossValidation::SinglePath);
    }

    // ── 单路径文档不足 ──

    #[test]
    fn single_path_with_fewer_than_three_docs() {
        let cals = vec![active_cal("vector"), active_cal("text")];
        let arr1: [(String, f32); 1] = [("a".into(), 0.9)];
        let arr2: [(String, f32); 1] = [("b".into(), 0.8)];
        let docs: Vec<&[(String, f32)]> = vec![&arr1, &arr2];
        let cv = validate_signals(&cals, &docs);
        assert_eq!(cv, CrossValidation::SinglePath);
    }
}
