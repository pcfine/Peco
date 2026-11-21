//! 旧记忆清理：删除升级前写入、没有空间标签的记忆文档。
//!
//! 只删 `ppa_*`（自动提取的记忆）且 `metadata.extra` 里没有 [`WS_ID_TAG`] 的
//! 文档：这类文档无法判定归属，按设计不迁移、不备份，直接删掉让读路径从空
//! 开始。用户上传到记忆库的其它文档不会被这条规则命中 —— 它们的 source 不是
//! `ppa_*`，删它们等于替用户做主。
//!
//! 幂等：删掉的文档不再出现，再跑一次返回 0。

use knowledge_base::Document;
use peco_core::knowledge::{KnowledgeManager, KnowledgeModuleError};
use tracing::warn;

use super::config::MemoryConfig;
use super::{MEMORY_SCAN_LIMIT, WS_ID_TAG};

/// 删除无空间标签的旧记忆，返回删除条数。
///
/// KB 不存在按「没有旧记忆」处理（`Ok(0)`）：团队模板没装或用户删过库时，
/// 调用方不该看见一个把「无事可做」当成故障的错误。
pub async fn purge_legacy(
    km: &KnowledgeManager,
    config: &MemoryConfig,
) -> Result<usize, KnowledgeModuleError> {
    let documents = match km
        .list_memory_documents_with_content(&config.kb_name, MEMORY_SCAN_LIMIT)
        .await
    {
        Ok(documents) => documents,
        Err(KnowledgeModuleError::NotFound(_)) => return Ok(0),
        Err(e) => return Err(e),
    };

    let mut deleted = 0;
    for document in documents.iter().filter(|doc| is_legacy(doc)) {
        match km.delete_document(&config.kb_name, &document.id).await {
            Ok(_) => deleted += 1,
            // 单条删不掉不影响其它条：清理是尽力而为的迁移动作，卡在一条上
            // 会让整个库的旧记忆永远留着。
            Err(e) => warn!(
                kb = %config.kb_name,
                doc_id = %document.id,
                error = %e,
                "legacy memory delete failed; leaving it in place"
            ),
        }
    }
    Ok(deleted)
}

/// 旧记忆的判据：记忆来源标签 + 没有空间归属。
fn is_legacy(doc: &Document) -> bool {
    doc.source_path.starts_with("ppa_") && !doc.metadata.extra.contains_key(WS_ID_TAG)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::test_support::memory_kb;
    use knowledge_base::DocumentMetadata;

    fn config() -> MemoryConfig {
        MemoryConfig::default()
    }

    async fn seed(km: &KnowledgeManager, ws_id: Option<&str>, text: &str, source: &str) -> String {
        match ws_id {
            Some(ws_id) => {
                let mut metadata = DocumentMetadata::default();
                metadata
                    .extra
                    .insert(WS_ID_TAG.to_string(), ws_id.to_string());
                km.add_text_to_kb_with_metadata("@private_memory", "memory", text, source, metadata)
                    .await
                    .unwrap()
                    .id
            }
            None => {
                km.add_text_to_kb("@private_memory", "memory", text, source)
                    .await
                    .unwrap()
                    .id
            }
        }
    }

    async fn remaining(km: &KnowledgeManager) -> Vec<String> {
        km.list_memory_documents_with_content("@private_memory", 100)
            .await
            .unwrap()
            .into_iter()
            .map(|doc| doc.content)
            .collect()
    }

    /// 旧文档被删、新文档保留、计数正确，且第二次运行为 0（可重入）。
    ///
    /// 同时钉住「非 ppa_ 来源的文档不删」：用户上传的资料与自动提取的记忆
    /// 在同一个库里，误删不可恢复。
    #[tokio::test]
    async fn purge_removes_only_untagged_memory_documents_and_is_reentrant() {
        let (km, _dir) = memory_kb().await;
        seed(&km, None, "旧记忆：用户偏好简洁的回答", "ppa_profile").await;
        seed(&km, None, "旧记忆：用户在用 Rust", "ppa_semantic").await;
        seed(&km, Some("ws_a"), "新记忆：用户偏好中文", "ppa_profile").await;
        seed(&km, None, "用户上传的资料", "uploaded/notes.md").await;

        let deleted = purge_legacy(&km, &config()).await.unwrap();
        assert_eq!(deleted, 2, "只删两条无标签的 ppa_ 文档");

        let kept = remaining(&km).await;
        assert_eq!(kept.len(), 2, "保留的应只有新记忆与上传资料: {kept:?}");
        assert!(kept.iter().any(|c| c.contains("新记忆")), "{kept:?}");
        assert!(kept.iter().any(|c| c.contains("上传的资料")), "{kept:?}");

        assert_eq!(
            purge_legacy(&km, &config()).await.unwrap(),
            0,
            "删干净后再跑一次必须是 0"
        );
        assert_eq!(remaining(&km).await.len(), 2);
    }

    #[tokio::test]
    async fn missing_kb_purges_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let km = KnowledgeManager::new(dir.path().to_path_buf());
        km.ensure_loaded().await.unwrap();

        assert_eq!(purge_legacy(&km, &config()).await.unwrap(), 0);
    }
}
