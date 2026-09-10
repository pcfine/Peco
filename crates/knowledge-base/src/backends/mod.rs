pub mod memory;
pub mod memory_graph;

#[cfg(feature = "lancedb")]
pub mod lancedb;

#[cfg(feature = "helixdb")]
pub mod helixdb;

use crate::error::KnowledgeError;

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
