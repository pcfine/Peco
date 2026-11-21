//! 存量记忆迁移：团队旧库 → 空间新库，一次性。
//!
//! 旧库文档不删（回滚路径就是它），迁移完成的凭据是记忆根下的 `migrated.json`：
//! 它不存在就说明还有没搬完的文档，下次打开空间继续搬。单条失败同样不写凭据 ——
//! 记下「迁移完成」却漏了几条记忆，比反复重试糟得多。

use std::path::{Path, PathBuf};

use peco_core::knowledge::{KnowledgeManager, KnowledgeModuleError};
use tracing::{info, warn};

use super::config::MemoryConfig;
use super::{MEMORY_SCAN_LIMIT, WS_ID_TAG};

/// 迁移完成的凭据文件名。
pub const MIGRATED_FILE: &str = "migrated.json";

/// 迁移结论；日志与用例共用同一份口径。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// 凭据已存在，本次什么都没做。
    pub skipped: bool,
    /// 旧库里属于本空间的文档数。
    pub found: usize,
    /// 写入新库的文档数。
    pub written: usize,
    /// 写入失败的文档数；非零时不写凭据。
    pub failed: usize,
}

impl MigrationReport {
    /// 全部搬完且没有失败（含「本来就没有存量」）。
    pub fn complete(&self) -> bool {
        self.failed == 0 && (self.skipped || self.written == self.found)
    }
}

/// 凭据文件路径。
pub fn marker_path(memory_root: &Path) -> PathBuf {
    memory_root.join(MIGRATED_FILE)
}

/// 该空间是否已完成迁移。
pub fn is_migrated(memory_root: &Path) -> bool {
    marker_path(memory_root).is_file()
}

/// 把旧库里 `ws_id` 名下的文档搬进空间记忆库。
///
/// 失败不冒泡到打开路径：迁移是升级动作，一个坏掉的知识库不该拦住空间打开。
pub async fn migrate(
    team: &KnowledgeManager,
    memory: &KnowledgeManager,
    memory_root: &Path,
    ws_id: &str,
    config: &MemoryConfig,
) -> MigrationReport {
    let mut report = MigrationReport::default();
    if is_migrated(memory_root) {
        report.skipped = true;
        return report;
    }

    let documents = match team
        .list_memory_documents_with_content(&config.kb_name, MEMORY_SCAN_LIMIT)
        .await
    {
        Ok(documents) => documents,
        // 团队旧库不存在 = 没有存量可搬，这就是迁移完成。
        Err(KnowledgeModuleError::NotFound(_)) => Vec::new(),
        Err(e) => {
            warn!(
                ws_id,
                kb = %config.kb_name,
                error = %e,
                "cannot read the team memory library; migration will retry on the next open"
            );
            report.failed += 1;
            return report;
        }
    };

    let legacy: Vec<_> = documents
        .into_iter()
        .filter(|doc| doc.metadata.extra.get(WS_ID_TAG).map(String::as_str) == Some(ws_id))
        .collect();
    report.found = legacy.len();

    for doc in legacy {
        // 新库只含本空间，归属标签因此不再需要；留着它会让「迁移来的」与
        // 「新写的」在元数据上看起来是两种东西。
        let mut metadata = doc.metadata.clone();
        metadata.extra.remove(WS_ID_TAG);
        match memory
            .add_text_to_kb_with_metadata(
                &config.kb_name,
                &doc.title,
                &doc.content,
                &doc.source_path,
                metadata,
            )
            .await
        {
            Ok(_) => report.written += 1,
            // 单条失败不阻塞其余：其余文档搬完，凭据不写，下次重试整批。
            Err(e) => {
                report.failed += 1;
                warn!(
                    ws_id,
                    doc_id = %doc.id,
                    error = %e,
                    "legacy memory document not migrated; the batch will retry"
                );
            }
        }
    }

    if report.failed > 0 {
        warn!(
            ws_id,
            failed = report.failed,
            found = report.found,
            written = report.written,
            "legacy memory migration incomplete; keeping the retry path open"
        );
        return report;
    }

    match write_marker(memory_root, report.written) {
        Ok(()) => info!(
            ws_id,
            count = report.written,
            "legacy memories migrated into the space library"
        ),
        // 凭据写不下去就当作没迁完：下次重写一遍是幂等的（正文哈希定 id）。
        Err(e) => {
            report.failed += 1;
            warn!(
                ws_id,
                path = %marker_path(memory_root).display(),
                error = %e,
                "cannot record the migration marker; migration will retry"
            );
        }
    }
    report
}

fn write_marker(memory_root: &Path, count: usize) -> std::io::Result<()> {
    let body = serde_json::json!({
        "from": "team",
        "count": count,
        "at": chrono::Utc::now().to_rfc3339(),
    });
    std::fs::create_dir_all(memory_root)?;
    std::fs::write(marker_path(memory_root), serde_json::to_vec_pretty(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::test_support::memory_kb;
    use knowledge_base::DocumentMetadata;

    async fn seed_team(km: &KnowledgeManager, ws_id: Option<&str>, text: &str) -> String {
        let mut metadata = DocumentMetadata::default();
        if let Some(ws_id) = ws_id {
            metadata
                .extra
                .insert(WS_ID_TAG.to_string(), ws_id.to_string());
        }
        km.add_text_to_kb_with_metadata("@private_memory", "memory", text, "ppa_semantic", metadata)
            .await
            .unwrap()
            .id
    }

    /// 只搬本空间的文档，搬完写凭据，标签不跟过去。
    #[tokio::test]
    async fn only_the_named_space_moves_and_the_marker_records_it() {
        let (team, _team_dir) = memory_kb().await;
        let (memory, memory_dir) = memory_kb().await;
        let root = memory_dir.path().join("ws_a");
        std::fs::create_dir_all(&root).unwrap();
        seed_team(&team, Some("ws_a"), "用户的主开发语言是 Rust").await;
        seed_team(&team, Some("ws_a"), "周五发布 v2").await;
        seed_team(&team, Some("ws_b"), "别人的记忆").await;
        seed_team(&team, None, "没有归属的老记忆").await;

        let report = migrate(&team, &memory, &root, "ws_a", &MemoryConfig::default()).await;

        assert_eq!(report.found, 2, "只有 ws_a 的两条该被搬");
        assert_eq!(report.written, 2);
        assert_eq!(report.failed, 0);
        assert!(report.complete());
        assert!(is_migrated(&root));

        let moved = memory
            .list_memory_documents_with_content("@private_memory", 100)
            .await
            .unwrap();
        assert_eq!(moved.len(), 2);
        for doc in &moved {
            assert!(
                !doc.metadata.extra.contains_key(WS_ID_TAG),
                "新库不再需要归属标签"
            );
        }
        assert!(
            !moved.iter().any(|doc| doc.content.contains("别人的")),
            "别的空间的记忆不得搬进来"
        );
    }

    /// 凭据存在 ⇒ 一条都不搬，哪怕旧库里还留着本空间的文档。
    #[tokio::test]
    async fn an_existing_marker_short_circuits_the_batch() {
        let (team, _team_dir) = memory_kb().await;
        let (memory, memory_dir) = memory_kb().await;
        let root = memory_dir.path().join("ws_a");
        std::fs::create_dir_all(&root).unwrap();
        seed_team(&team, Some("ws_a"), "用户偏好简洁的回答").await;
        write_marker(&root, 1).unwrap();

        let report = migrate(&team, &memory, &root, "ws_a", &MemoryConfig::default()).await;

        assert!(report.skipped);
        assert_eq!(report.written, 0);
        assert!(
            memory
                .list_memory_documents_with_content("@private_memory", 100)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// 团队旧库不存在 ⇒ 视为「没有存量」，凭据照写，下次不再扫。
    #[tokio::test]
    async fn a_missing_team_library_completes_with_zero_documents() {
        let (team, _team_dir) = memory_kb().await;
        let (memory, memory_dir) = memory_kb().await;
        let root = memory_dir.path().join("ws_a");

        let report = migrate(
            &team,
            &memory,
            &root,
            "ws_a",
            &MemoryConfig {
                kb_name: "@absent".to_string(),
                ..MemoryConfig::default()
            },
        )
        .await;

        assert_eq!(report.found, 0);
        assert_eq!(report.failed, 0);
        assert!(report.complete());
        assert!(is_migrated(&root));
    }
}
