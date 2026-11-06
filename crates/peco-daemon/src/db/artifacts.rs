// ============================================================================
// artifacts 数据库查询函数
// ============================================================================
//
// 这里只存**投影**：文件真实内容在该空间自己的 work 目录里，表中记相对路径。
// 相对而非绝对：空间的 work 目录随目录即寻址（搬目录不改库），行的归属由
// `conversation_id` 决定，`rel_path` 只在自己的空间根下有意义。
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

/// 空间级产物投影：多带一列所属对话的 agent 名，看板与产物页都不必再查一次。
///
/// `agent_name` 取 `conversations.agent_name`：产物自己只记 `conversation_id`，
/// 归属靠会话行，agent 名同理。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WorkspaceArtifactRow {
    pub id: String,
    pub conversation_id: String,
    pub agent_name: String,
    pub kind: String,
    pub rel_path: String,
    pub size_bytes: i64,
    pub created_at: String,
}

/// 按空间列出产物（最新在前）。
///
/// 归属经 `conversations.workspace_id` 判定，不看 `artifacts.user_id`：单用户
/// daemon 里那一列恒为固定值，按它过滤等于不过滤。JOIN 而非两步查询 —— 会话行
/// 缺失时产物行就该一起消失（外键保证不会，但过滤条件必须只有一个真源）。
pub async fn list_by_workspace(
    pool: &SqlitePool,
    workspace_id: &str,
    limit: i64,
) -> Result<Vec<WorkspaceArtifactRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkspaceArtifactRow>(
        "SELECT a.id, a.conversation_id, c.agent_name, a.kind, a.rel_path, a.size_bytes, \
         a.created_at \
         FROM artifacts a JOIN conversations c ON c.id = a.conversation_id \
         WHERE c.workspace_id = ? \
         ORDER BY a.created_at DESC, a.rowid DESC LIMIT ?",
    )
    .bind(workspace_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// 空间内的产物总数；`total` 是分页信封里的定值，不随 limit 变化。
pub async fn count_by_workspace(pool: &SqlitePool, workspace_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM artifacts a JOIN conversations c ON c.id = a.conversation_id \
         WHERE c.workspace_id = ?",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
}
