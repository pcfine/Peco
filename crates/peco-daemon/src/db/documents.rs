// ============================================================================
// 文档数据库查询函数
// ============================================================================

use sqlx::SqlitePool;

/// documents 表完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct DocumentRow {
    pub id: String,
    pub kb_id: String,
    pub filename: String,
    pub filepath: String,
    pub file_size: i64,
    pub mime_type: String,
    pub status: String,
    pub error_msg: Option<String>,
    pub created_at: String,
    /// 相对 `docs/` 的路径 —— manifest 投影的自然键。
    pub relative_path: Option<String>,
    /// KB 后端的文档 id —— 注册表与后端的 join key。
    pub doc_id: Option<String>,
}

/// 登记 upload 文档的参数。不含 `id`：行 id 由 `upsert_pending` 返回。
pub struct CreateDocumentParams {
    pub kb_id: String,
    pub filename: String,
    pub filepath: String,
    pub file_size: i64,
    pub mime_type: String,
    pub relative_path: String,
}

/// 插入或重置一条 pending 记录（upload 路径），返回 `(kb_id, relative_path)` 对应行的 id。
///
/// 调用方必须用返回值回写状态：重复上传时它可能与新建的候选 id 不同。
pub async fn upsert_pending(
    pool: &SqlitePool,
    params: &CreateDocumentParams,
) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO documents (id, kb_id, filename, filepath, file_size, mime_type, relative_path) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(kb_id, relative_path) DO UPDATE SET \
           filename = excluded.filename, \
           filepath = excluded.filepath, \
           file_size = excluded.file_size, \
           mime_type = excluded.mime_type, \
           status = 'pending', \
           error_msg = NULL \
         RETURNING id",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&params.kb_id)
    .bind(&params.filename)
    .bind(&params.filepath)
    .bind(params.file_size)
    .bind(&params.mime_type)
    .bind(&params.relative_path)
    .fetch_one(pool)
    .await
}

/// 登记一条已同步文档（manifest 投影命中）：冲突时置 ready 并刷新元数据。
#[allow(clippy::too_many_arguments)]
pub async fn upsert_synced(
    pool: &SqlitePool,
    kb_id: &str,
    relative_path: &str,
    filename: &str,
    filepath: &str,
    file_size: i64,
    mime_type: &str,
    doc_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO documents \
           (id, kb_id, filename, filepath, file_size, mime_type, status, relative_path, doc_id) \
         VALUES (?, ?, ?, ?, ?, ?, 'ready', ?, ?) \
         ON CONFLICT(kb_id, relative_path) DO UPDATE SET \
           filename = excluded.filename, \
           filepath = excluded.filepath, \
           file_size = excluded.file_size, \
           mime_type = excluded.mime_type, \
           status = 'ready', \
           error_msg = NULL, \
           doc_id = excluded.doc_id",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(kb_id)
    .bind(filename)
    .bind(filepath)
    .bind(file_size)
    .bind(mime_type)
    .bind(relative_path)
    .bind(doc_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// 登记一条同步失败文档（manifest 未命中且 report 报错）：冲突时置 error。
pub async fn upsert_error(
    pool: &SqlitePool,
    kb_id: &str,
    relative_path: &str,
    filename: &str,
    filepath: &str,
    error_msg: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO documents \
           (id, kb_id, filename, filepath, status, relative_path, error_msg) \
         VALUES (?, ?, ?, ?, 'error', ?, ?) \
         ON CONFLICT(kb_id, relative_path) DO UPDATE SET \
           status = 'error', \
           error_msg = excluded.error_msg, \
           doc_id = NULL",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(kb_id)
    .bind(filename)
    .bind(filepath)
    .bind(relative_path)
    .bind(error_msg)
    .execute(pool)
    .await?;
    Ok(())
}

/// 删除该 KB 下带 `relative_path`、且不在 `keep` 中的投影行。
pub async fn delete_synced_missing(
    pool: &SqlitePool,
    kb_id: &str,
    keep: &[String],
) -> Result<(), sqlx::Error> {
    if keep.is_empty() {
        sqlx::query("DELETE FROM documents WHERE kb_id = ? AND relative_path IS NOT NULL")
            .bind(kb_id)
            .execute(pool)
            .await?;
        return Ok(());
    }

    let placeholders = std::iter::repeat_n("?", keep.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "DELETE FROM documents WHERE kb_id = ? AND relative_path IS NOT NULL \
         AND relative_path NOT IN ({placeholders})"
    );
    let mut query = sqlx::query(&sql).bind(kb_id);
    for path in keep {
        query = query.bind(path);
    }
    query.execute(pool).await?;
    Ok(())
}

/// 查询知识库下的文档列表，支持分页和状态过滤。
pub async fn list_by_kb(
    pool: &SqlitePool,
    kb_id: &str,
    offset: i64,
    limit: i64,
    status_filter: Option<&str>,
) -> Result<Vec<DocumentRow>, sqlx::Error> {
    if let Some(status) = status_filter {
        sqlx::query_as::<_, DocumentRow>(
            "SELECT id, kb_id, filename, filepath, file_size, mime_type, status, error_msg, created_at, relative_path, doc_id \
             FROM documents WHERE kb_id = ? AND status = ? \
             ORDER BY created_at DESC LIMIT ? OFFSET ?",
        )
        .bind(kb_id)
        .bind(status)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, DocumentRow>(
            "SELECT id, kb_id, filename, filepath, file_size, mime_type, status, error_msg, created_at, relative_path, doc_id \
             FROM documents WHERE kb_id = ? \
             ORDER BY created_at DESC LIMIT ? OFFSET ?",
        )
        .bind(kb_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
    }
}

/// 按 ID 查询单个文档。
pub async fn find_by_id(
    pool: &SqlitePool,
    doc_id: &str,
) -> Result<Option<DocumentRow>, sqlx::Error> {
    sqlx::query_as::<_, DocumentRow>(
        "SELECT id, kb_id, filename, filepath, file_size, mime_type, status, error_msg, created_at, relative_path, doc_id \
         FROM documents WHERE id = ?",
    )
    .bind(doc_id)
    .fetch_optional(pool)
    .await
}

/// 更新文档状态。
pub async fn update_status(
    pool: &SqlitePool,
    doc_id: &str,
    status: &str,
    error_msg: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE documents SET status = ?, error_msg = ? WHERE id = ?")
        .bind(status)
        .bind(error_msg)
        .bind(doc_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// 删除文档记录。
pub async fn delete(pool: &SqlitePool, doc_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM documents WHERE id = ?")
        .bind(doc_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// 按知识库 ID 统计文档数量。
pub async fn count_by_kb(pool: &SqlitePool, kb_id: &str) -> Result<i64, sqlx::Error> {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM documents WHERE kb_id = ?")
        .bind(kb_id)
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::db::{connect, run_migrations};

    /// 建出带迁移的临时库，并预置一行 KB —— `documents.kb_id` 有外键约束。
    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/documents.db?mode=rwc", dir.path().display());
        let pool = connect(&url).await.unwrap();
        run_migrations(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO users (id, username, email, password_hash) VALUES ('u1','u','u@e','h')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO knowledge_bases (id, user_id, name, description) VALUES ('kb1','u1','kb','')",
        )
        .execute(&pool)
        .await
        .unwrap();
        (pool, dir)
    }

    fn params(filename: &str) -> CreateDocumentParams {
        CreateDocumentParams {
            kb_id: "kb1".into(),
            filename: filename.into(),
            filepath: format!("/x/{filename}"),
            file_size: 3,
            mime_type: "text/markdown".into(),
            relative_path: filename.into(),
        }
    }

    /// 重复上传同一自然键必须复用既有行 id，否则失败路径会更新到不存在的行。
    #[tokio::test]
    async fn upsert_pending_reuses_row_id_for_same_natural_key() {
        let (pool, _dir) = test_pool().await;

        let first = upsert_pending(&pool, &params("a.md")).await.unwrap();
        let second = upsert_pending(&pool, &params("a.md")).await.unwrap();
        assert_eq!(first, second, "同一 (kb_id, relative_path) 应复用同一行 id");

        update_status(&pool, &second, "error", Some("boom"))
            .await
            .unwrap();
        let rows = list_by_kb(&pool, "kb1", 0, 10, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, second);
        assert_eq!(rows[0].status, "error");
    }

    /// ready → error 转换必须清掉陈旧 join key，否则 error 行仍指向后端已不存在的文档。
    #[tokio::test]
    async fn upsert_error_clears_stale_doc_id() {
        let (pool, _dir) = test_pool().await;

        upsert_synced(
            &pool,
            "kb1",
            "a.md",
            "a.md",
            "/x/a.md",
            3,
            "text/markdown",
            "backend-doc",
        )
        .await
        .unwrap();
        let rows = list_by_kb(&pool, "kb1", 0, 10, None).await.unwrap();
        assert_eq!(rows[0].doc_id.as_deref(), Some("backend-doc"));

        upsert_error(&pool, "kb1", "a.md", "a.md", "/x/a.md", "parse boom")
            .await
            .unwrap();
        let rows = list_by_kb(&pool, "kb1", 0, 10, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "error");
        assert_eq!(rows[0].doc_id, None, "error 行不得残留 doc_id");
    }
}
