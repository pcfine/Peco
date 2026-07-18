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
pub async fn list_by_conversation(
    pool: &SqlitePool,
    conversation_id: &str,
    offset: i64,
    limit: i64,
) -> Result<Vec<MessageRow>, sqlx::Error> {
    sqlx::query_as::<_, MessageRow>(
        "SELECT id, conversation_id, role, content, agent_id, agent_name, created_at \
         FROM messages WHERE conversation_id = ? ORDER BY created_at ASC LIMIT ? OFFSET ?",
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
