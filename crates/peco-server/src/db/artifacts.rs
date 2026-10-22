// ============================================================================
// artifacts 数据库查询函数
// ============================================================================
//
// 这里只存**投影**：文件真实内容在工作层目录（`<data_dir>/work/<user_id>/`），
// 表中记相对路径。换 `PECO_DATA_DIR` 根时文件与行一起走，不需重写路径。
//
// 文件被清理后行仍保留 —— 这正是「保留元数据、UI 显示已过期」所需的行为。

use sqlx::SqlitePool;

/// artifacts 表完整行。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ArtifactRow {
    pub id: String,
    pub conversation_id: String,
    pub user_id: String,
    /// `image` / `video` / `audio` / `document`
    pub kind: String,
    /// 声明或按扩展名推断出的真实类型（取流时再按内联白名单收窄）。
    pub mime: String,
    pub title: String,
    /// 相对 `work/<user_id>/` 的路径，如 `output/media-gen/2026-10-20/x.jpg`
    pub rel_path: String,
    pub size_bytes: i64,
    pub created_at: String,
}

/// 插入一条产物登记。
///
/// 返回 `false` 表示同一对话内同路径已存在（`ON CONFLICT DO NOTHING`）——
/// 去重由唯一索引 `(conversation_id, rel_path)` 保证，不做 check-then-insert：
/// 同一对话两条流并发声明同一文件时，那个窗口会双插。
pub async fn insert(pool: &SqlitePool, row: &ArtifactRow) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        "INSERT INTO artifacts (id, conversation_id, user_id, kind, mime, title, rel_path, size_bytes) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(conversation_id, rel_path) DO NOTHING",
    )
    .bind(&row.id)
    .bind(&row.conversation_id)
    .bind(&row.user_id)
    .bind(&row.kind)
    .bind(&row.mime)
    .bind(&row.title)
    .bind(&row.rel_path)
    .bind(row.size_bytes)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// 按对话列出产物（时间正序 —— 与消息流方向一致）。
pub async fn list_by_conversation(
    pool: &SqlitePool,
    conversation_id: &str,
) -> Result<Vec<ArtifactRow>, sqlx::Error> {
    sqlx::query_as::<_, ArtifactRow>(
        "SELECT id, conversation_id, user_id, kind, mime, title, rel_path, size_bytes, created_at \
         FROM artifacts WHERE conversation_id = ? ORDER BY created_at ASC, rowid ASC",
    )
    .bind(conversation_id)
    .fetch_all(pool)
    .await
}

/// 按 id + 属主查单条（取流端点用；不返回他人产物）。
pub async fn find_by_id_and_user(
    pool: &SqlitePool,
    id: &str,
    user_id: &str,
) -> Result<Option<ArtifactRow>, sqlx::Error> {
    sqlx::query_as::<_, ArtifactRow>(
        "SELECT id, conversation_id, user_id, kind, mime, title, rel_path, size_bytes, created_at \
         FROM artifacts WHERE id = ? AND user_id = ?",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// 按对话倒序列出一页产物（最新在前 —— 产物面板要"最近生成的先看到"）。
///
/// `rowid` 参与排序：`created_at` 只到秒，同秒多条靠插入序定序，
/// 只按 `created_at` 排会让同一秒内的顺序不确定，翻页时前后页互相穿插。
pub async fn list_by_conversation_desc(
    pool: &SqlitePool,
    conversation_id: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<ArtifactRow>, sqlx::Error> {
    sqlx::query_as::<_, ArtifactRow>(
        "SELECT id, conversation_id, user_id, kind, mime, title, rel_path, size_bytes, created_at \
         FROM artifacts WHERE conversation_id = ? \
         ORDER BY created_at DESC, rowid DESC LIMIT ? OFFSET ?",
    )
    .bind(conversation_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// 某对话的产物总数（分页信封的 `total`，不随分页变化）。
pub async fn count_by_conversation(
    pool: &SqlitePool,
    conversation_id: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM artifacts WHERE conversation_id = ?")
        .bind(conversation_id)
        .fetch_one(pool)
        .await
}
