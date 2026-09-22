pub mod memory;
pub mod memory_graph;

#[cfg(feature = "lancedb")]
pub mod lancedb;

#[cfg(feature = "helixdb")]
pub mod helixdb;

use crate::error::KnowledgeError;
use crate::traits::graph_store::{EdgeType, KnowledgeEdge};

/// `edges_between` / `remove_edges_between` 的共享匹配谓词（内存后端使用）。
///
/// 语义与 `GraphStore` trait 文档一致，两个内存后端共用一份，避免口径漂移：
/// - `edge_type == Some(t)`：**有向**匹配 `source → target` 且边类型精确相等
///   （谓词无任何规范化，大小写敏感）；
/// - `edge_type == None`：**双向**匹配，只要两端点相同即可，不看方向。
pub(crate) fn edge_matches_between(
    edge: &KnowledgeEdge,
    source_id: &str,
    target_id: &str,
    edge_type: Option<&EdgeType>,
) -> bool {
    match edge_type {
        Some(t) => {
            edge.source_id == source_id && edge.target_id == target_id && &edge.edge_type == t
        }
        None => {
            (edge.source_id == source_id && edge.target_id == target_id)
                || (edge.source_id == target_id && edge.target_id == source_id)
        }
    }
}

/// `remove_node` 的共享实现（内存后端使用）：返回删除前观测到的关联边数。
///
/// `cascade == false` 且仍有边时返回 [`KnowledgeError::InvalidInput`]。
pub(crate) fn remove_node_from_memory(
    edges: &mut Vec<KnowledgeEdge>,
    node_id: &str,
    cascade: bool,
) -> Result<usize, KnowledgeError> {
    let attached = edges
        .iter()
        .filter(|e| e.source_id == node_id || e.target_id == node_id)
        .count();
    if attached > 0 && !cascade {
        return Err(KnowledgeError::InvalidInput(format!(
            "Node '{node_id}' still has {attached} edges; pass cascade=true to delete it together with them"
        )));
    }
    edges.retain(|e| e.source_id != node_id && e.target_id != node_id);
    Ok(attached)
}

/// 聚合逐条删除索引条目的结果：任一失败即报错，全部成功才返回 `Ok(())`。
///
/// 删除过程中静默吞掉单条失败会留下「文档已删但召回仍在」的幽灵数据，
/// 因此每个后端的 `remove` 实现都必须把失败上抛。
/// `failures` 为 `(条目 id, 错误原因)` 列表；`wrap` 将聚合详情包装为
/// 对应的错误变体（向量索引用 [`KnowledgeError::VectorError`]，
/// 全文索引用 [`KnowledgeError::TextSearchError`]）。
pub(crate) fn aggregate_remove_failures(
    failures: Vec<(String, String)>,
    wrap: fn(String) -> KnowledgeError,
) -> Result<(), KnowledgeError> {
    if failures.is_empty() {
        return Ok(());
    }
    let detail = failures
        .iter()
        .map(|(id, err)| format!("{id}: {err}"))
        .collect::<Vec<_>>()
        .join("; ");
    Err(wrap(format!(
        "Failed to remove {} index entries: {detail}",
        failures.len()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_failures_yields_ok() {
        assert!(aggregate_remove_failures(vec![], KnowledgeError::VectorError).is_ok());
    }

    #[test]
    fn single_failure_reports_id_and_reason() {
        let err = aggregate_remove_failures(
            vec![("chunk-1".into(), "io error".into())],
            KnowledgeError::VectorError,
        )
        .unwrap_err();
        assert!(matches!(err, KnowledgeError::VectorError(_)));
        let msg = err.to_string();
        assert!(msg.contains("chunk-1"), "错误信息需包含失败 id: {msg}");
        assert!(msg.contains("io error"), "错误信息需包含失败原因: {msg}");
    }

    #[test]
    fn multiple_failures_are_all_aggregated() {
        let err = aggregate_remove_failures(
            vec![
                ("chunk-1".into(), "boom".into()),
                ("chunk-2".into(), "bang".into()),
            ],
            KnowledgeError::TextSearchError,
        )
        .unwrap_err();
        assert!(matches!(err, KnowledgeError::TextSearchError(_)));
        let msg = err.to_string();
        assert!(msg.contains("chunk-1") && msg.contains("chunk-2"), "{msg}");
        assert!(msg.contains("boom") && msg.contains("bang"), "{msg}");
    }
}
