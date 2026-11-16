// ============================================================================
// Messages 数据库查询函数
// ============================================================================

use sqlx::SqlitePool;

/// messages 表完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct MessageRow {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub created_at: String,
}

/// 插入新消息记录（数据库层面记录概要）。
pub async fn insert(
    pool: &SqlitePool,
    id: &str,
    conversation_id: &str,
    role: &str,
    content: &str,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO messages (id, conversation_id, role, content, agent_id, agent_name) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(conversation_id)
    .bind(role)
    .bind(content)
    .bind(agent_id)
    .bind(agent_name)
    .execute(pool)
    .await?;
    Ok(())
}

/// 查询对话的消息列表，按时间升序，支持分页。
///
/// 排序带 `rowid ASC` 作同秒 tiebreak：`created_at` 只到秒（`datetime('now')`），
/// 同一轮的 user 行与 assistant 行完全可能同秒，而分页边界上的「偶然顺序」会让
/// 重复请求拿到不同的行序。
///
/// 为什么不加迁移：`id` 是 TEXT 主键（不是 `INTEGER PRIMARY KEY` 别名），表因此
/// **本来就有**隐式 `rowid`；它只作并列次序，不新增列、不新增索引、不重写数据。
/// 只依赖相对顺序（插入顺序 = 阅读顺序），插入由单个 run 任务串行完成 —— 连
/// `VACUUM` 改变 rowid 也不影响相对次序。
pub async fn list_by_conversation(
    pool: &SqlitePool,
    conversation_id: &str,
    offset: i64,
    limit: i64,
) -> Result<Vec<MessageRow>, sqlx::Error> {
    sqlx::query_as::<_, MessageRow>(
        "SELECT id, conversation_id, role, content, agent_id, agent_name, created_at \
         FROM messages WHERE conversation_id = ? \
         ORDER BY created_at ASC, rowid ASC LIMIT ? OFFSET ?",
    )
    .bind(conversation_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// 删除对话关联的所有消息。
pub async fn delete_by_conversation(
    pool: &SqlitePool,
    conversation_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM messages WHERE conversation_id = ?")
        .bind(conversation_id)
        .execute(pool)
        .await?;
    Ok(())
}
