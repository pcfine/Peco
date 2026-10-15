//! `documents` 表投影 —— 把同步结果（manifest + 本轮失败项）重建为 REST 面注册表。

use std::path::{Path, PathBuf};

use peco_core::knowledge::{KnowledgeManager, KnowledgeModuleError, SyncReport};
use sqlx::SqlitePool;

use crate::db::documents;
use crate::error::ApiError;

/// 某 KB 的 `docs/` 目录 —— workspace 内知识库布局的单点定义。
pub fn docs_dir(workspace_dir: &Path, kb_name: &str) -> PathBuf {
    workspace_dir
        .join("knowledge")
        .join(knowledge_base::sanitize_kb_name(kb_name))
        .join("docs")
}

/// 由 manifest 重投影该 KB 的 `documents` 行，并清理已消失的行。
///
/// manifest 读不到时直接返回、不删任何行（目录消失可能是误删）。`report.errors`
/// 中的文件也登记为 error，否则会在 UI 上凭空消失。
pub async fn reconcile_documents(
    db: &SqlitePool,
    km: &KnowledgeManager,
    kb_id: &str,
    kb_name: &str,
    docs_dir: &Path,
    report: &SyncReport,
) -> Result<(), ApiError> {
    let manifest = match km.read_manifest(kb_name).await {
        Ok(manifest) => manifest,
        Err(KnowledgeModuleError::NotFound(_)) => return Ok(()),
        Err(e) => return Err(ApiError::Internal(format!("read manifest failed: {e}"))),
    };

    let mut keep: Vec<String> = manifest.files.keys().cloned().collect();

    for (relative_path, entry) in &manifest.files {
        let mime_type = mime_guess::from_path(relative_path)
            .first_or_octet_stream()
            .to_string();
        documents::upsert_synced(
            db,
            kb_id,
            relative_path,
            &file_name(relative_path),
            &docs_dir.join(relative_path).to_string_lossy(),
            entry.size as i64,
            &mime_type,
            &entry.doc_id,
        )
        .await?;
    }

    for (relative_path, error_msg) in &report.errors {
        if manifest.files.contains_key(relative_path) {
            continue;
        }
        keep.push(relative_path.clone());
        documents::upsert_error(
            db,
            kb_id,
            relative_path,
            &file_name(relative_path),
            &docs_dir.join(relative_path).to_string_lossy(),
            error_msg,
        )
        .await?;
    }

    documents::delete_synced_missing(db, kb_id, &keep).await?;
    Ok(())
}

/// 取相对路径的文件名部分（子目录内文件用 basename 展示）。
fn file_name(relative_path: &str) -> String {
    Path::new(relative_path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| relative_path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use peco_core::knowledge::FileHashManifest;
    use peco_core::knowledge::hash_manifest::FileEntry;

    use crate::db::{connect, run_migrations};

    /// 不变量：命中记 ready，manifest 中消失则删行，仅 report 报错则记 error。
    #[tokio::test]
    async fn reconcile_projects_manifest_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();

        let url = format!("sqlite:{}/reconcile.db?mode=rwc", workspace.display());
        let pool = connect(&url).await.unwrap();
        run_migrations(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO users (id, username, email, password_hash) VALUES ('u1','u','u@e','h')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO knowledge_bases (id, user_id, name, description) VALUES ('kb1','u1','recon','')")
            .execute(&pool)
            .await
            .unwrap();

        let kb_name = "recon";
        let knowledge_root = workspace.join("knowledge");
        let km = KnowledgeManager::new(knowledge_root.clone());
        km.ensure_loaded().await.unwrap();

        let kb_dir = knowledge_root.join(knowledge_base::sanitize_kb_name(kb_name));
        tokio::fs::create_dir_all(kb_dir.join("docs"))
            .await
            .unwrap();
        let mut manifest = FileHashManifest::default();
        manifest.files.insert(
            "a.md".into(),
            FileEntry {
                hash: "deadbeef".into(),
                size: 7,
                doc_id: "backend-doc-1".into(),
                ingested_at: "2026-01-01T00:00:00+00:00".into(),
            },
        );
        manifest.save(&kb_dir).await.unwrap();

        let docs = docs_dir(&workspace, kb_name);
        let report = SyncReport::new(kb_name);

        reconcile_documents(&pool, &km, "kb1", kb_name, &docs, &report)
            .await
            .unwrap();

        let rows = documents::list_by_kb(&pool, "kb1", 0, 10, None)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "manifest 中一条文档应投影出一行");
        assert_eq!(rows[0].status, "ready");
        assert_eq!(rows[0].relative_path.as_deref(), Some("a.md"));
        assert_eq!(rows[0].doc_id.as_deref(), Some("backend-doc-1"));
        assert_eq!(rows[0].file_size, 7);

        // manifest 中该 entry 消失 → 投影行被删
        FileHashManifest::default().save(&kb_dir).await.unwrap();
        reconcile_documents(&pool, &km, "kb1", kb_name, &docs, &report)
            .await
            .unwrap();
        let rows = documents::list_by_kb(&pool, "kb1", 0, 10, None)
            .await
            .unwrap();
        assert!(rows.is_empty(), "manifest 中消失的文档应删除其投影行");

        // 仅 report.errors 命中 → 记 error
        let mut error_report = SyncReport::new(kb_name);
        error_report
            .errors
            .push(("b.md".into(), "parse boom".into()));
        reconcile_documents(&pool, &km, "kb1", kb_name, &docs, &error_report)
            .await
            .unwrap();
        let rows = documents::list_by_kb(&pool, "kb1", 0, 10, None)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "失败文件应登记为一行");
        assert_eq!(rows[0].status, "error");
        assert_eq!(rows[0].error_msg.as_deref(), Some("parse boom"));
    }

    /// KB 目录不存在时不得删任何投影行。
    #[tokio::test]
    async fn reconcile_keeps_rows_when_kb_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();

        let url = format!("sqlite:{}/reconcile2.db?mode=rwc", workspace.display());
        let pool = connect(&url).await.unwrap();
        run_migrations(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO users (id, username, email, password_hash) VALUES ('u1','u','u@e','h')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO knowledge_bases (id, user_id, name, description) VALUES ('kb1','u1','gone','')")
            .execute(&pool)
            .await
            .unwrap();
        documents::upsert_synced(
            &pool,
            "kb1",
            "keep.md",
            "keep.md",
            "/x/keep.md",
            3,
            "text/markdown",
            "backend-doc-1",
        )
        .await
        .unwrap();

        // 不创建 KB 目录 → read_manifest 报 NotFound
        let km = KnowledgeManager::new(workspace.join("knowledge"));
        km.ensure_loaded().await.unwrap();
        let report = SyncReport::new("gone");
        reconcile_documents(
            &pool,
            &km,
            "kb1",
            "gone",
            &docs_dir(&workspace, "gone"),
            &report,
        )
        .await
        .unwrap();

        let rows = documents::list_by_kb(&pool, "kb1", 0, 10, None)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "KB 目录缺失时不得清除既有投影行");
    }
}
