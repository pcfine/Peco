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
}

/// 创建文档记录的参数（由 handler 层传入）。
pub struct CreateDocumentParams {
    pub id: String,
    pub kb_id: String,
    pub filename: String,
    pub filepath: String,
    pub file_size: i64,
    pub mime_type: String,
}

/// 插入新文档记录（status 默认为 'pending'）。
pub async fn insert(pool: &SqlitePool, params: &CreateDocumentParams) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO documents (id, kb_id, filename, filepath, file_size, mime_type) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&params.id)
    .bind(&params.kb_id)
    .bind(&params.filename)
    .bind(&params.filepath)
    .bind(params.file_size)
    .bind(&params.mime_type)
    .execute(pool)
    .await?;
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
            "SELECT id, kb_id, filename, filepath, file_size, mime_type, status, error_msg, created_at \
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
            "SELECT id, kb_id, filename, filepath, file_size, mime_type, status, error_msg, created_at \
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
        "SELECT id, kb_id, filename, filepath, file_size, mime_type, status, error_msg, created_at \
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
    sqlx::query(
        "UPDATE documents SET status = ?, error_msg = ? WHERE id = ?",
    )
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
    let row: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM documents WHERE kb_id = ?")
            .bind(kb_id)
            .fetch_one(pool)
            .await?;
    Ok(row.0)
}
