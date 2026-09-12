//! 进程内向量比较 — 余弦相似度与连通分量聚类纯函数。
//!
//! 为记忆整理（聚类去重 / 沉淀分组）、召回去重等机器判定逻辑
//! 提供「可与阈值比较的相似度值」。全部纯函数、显式传参、无 IO。
//!
//! 度量定义统一为 `cosine_similarity = 1 − cosine_distance`，取值 [-1, 1]。
//! 与 `SearchResult.score`（RRF 融合分）语义不同，二者禁止混用。

use std::collections::HashMap;

/// 两个向量的余弦相似度（1 − cosine_distance）。
///
/// 长度不一致、任一向量为全零或为空时无可比方向，返回 0。
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        return 0.0;
    }
    (dot / denom).clamp(-1.0, 1.0)
}

/// 并查集查找（带路径压缩）。
fn find(parent: &mut [usize], x: usize) -> usize {
    let mut root = x;
    while parent[root] != root {
        root = parent[root];
    }
    let mut cur = x;
    while parent[cur] != root {
        let next = parent[cur];
        parent[cur] = root;
        cur = next;
    }
    root
}

/// 按余弦相似度阈值对向量做连通分量聚类。
///
/// `vectors[i]` 与 `vectors[j]` 的相似度 ≥ `threshold` 即归入同组；
/// 组内传递相连（A~B、B~C 则 A/B/C 同组，即使 A~C 低于阈值）。
///
/// 返回各连通分量的下标分组：组内下标升序，组间按最小下标升序，
/// 全部组不重叠且并集覆盖输入。单元素向量各自成组。
pub fn connected_component_clusters(vectors: &[Vec<f32>], threshold: f32) -> Vec<Vec<usize>> {
    let n = vectors.len();
    if n == 0 {
        return Vec::new();
    }

    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for j in (i + 1)..n {
            if cosine_similarity(&vectors[i], &vectors[j]) >= threshold {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[rj] = ri;
                }
            }
        }
    }

    // 按根收集成员
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut roots: HashMap<usize, usize> = HashMap::new();
    for idx in 0..n {
        let p = parent[idx];
        let root = find(&mut parent, p);
        let slot = *roots.entry(root).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[slot].push(idx);
    }

    // 确定性输出：组内升序，组间按最小成员排序
    for members in &mut groups {
        members.sort_unstable();
    }
    groups.sort_by_key(|m| m[0]);
    groups
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn cosine_same_direction_is_one() {
        let a = vec![0.3f32, -1.2, 4.0, 0.0];
        let b = vec![0.9f32, -3.6, 12.0, 0.0];
        assert!(close(cosine_similarity(&a, &b), 1.0, 1e-5));
        assert!(close(cosine_similarity(&a, &a), 1.0, 1e-6));
    }

    #[test]
    fn cosine_opposite_is_minus_one() {
        let a = vec![1.0f32, 2.0, 3.0];
        let b = vec![-1.0f32, -2.0, -3.0];
        assert!(close(cosine_similarity(&a, &b), -1.0, 1e-6));
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        assert!(close(cosine_similarity(&a, &b), 0.0, 1e-6));
    }

    #[test]
    fn cosine_degenerate_inputs_return_zero() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn clusters_empty_input() {
        assert!(connected_component_clusters(&[], 0.85).is_empty());
    }

    #[test]
    fn clusters_single_element() {
        assert_eq!(
            connected_component_clusters(&[vec![1.0, 0.0]], 0.85),
            vec![vec![0]]
        );
    }

    #[test]
    fn clusters_pair_above_threshold_merge() {
        let a = vec![1.0f32, 0.0];
        let b = vec![1.0f32, 0.1]; // cos ≈ 0.995
        assert_eq!(
            connected_component_clusters(&[a, b], 0.85),
            vec![vec![0, 1]]
        );
    }

    #[test]
    fn clusters_below_threshold_stay_apart() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0]; // cos = 0
        assert_eq!(
            connected_component_clusters(&[a, b], 0.85),
            vec![vec![0], vec![1]]
        );
    }

    #[test]
    fn clusters_transitive_merge_across_threshold_boundary() {
        // a~b ≈ 0.867（≥0.85）、b~c ≈ 0.999（≥0.85）、a~c ≈ 0.845（< 0.85）
        // 传递相连 → 三者一组
        let a = vec![1.0f32, 0.0];
        let b = vec![0.87f32, 0.5];
        let c = vec![0.87f32, 0.55];
        assert_eq!(
            connected_component_clusters(&[a, b, c], 0.85),
            vec![vec![0, 1, 2]]
        );
    }

    #[test]
    fn clusters_deterministic_ordering() {
        // 无关组 c 与 a/b 组交错输入时，输出按最小下标排列
        let a = vec![1.0f32, 0.0];
        let b = vec![1.0f32, 0.1];
        let c = vec![0.0f32, 1.0];
        assert_eq!(
            connected_component_clusters(&[c, a, b], 0.85),
            vec![vec![0], vec![1, 2]]
        );
    }
}
