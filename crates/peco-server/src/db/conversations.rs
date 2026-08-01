// ============================================================================
// Conversations 数据库查询函数
// ============================================================================

use sqlx::SqlitePool;

/// conversations 表完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct ConversationRow {
    pub id: String,
    pub user_id: String,
    pub agent_id: Option<String>,
    pub agent_name: String,
    pub title: String,
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 创建对话的参数。
pub struct CreateConversationParams {
    pub id: String,
    pub user_id: String,
    pub agent_id: Option<String>,
    pub agent_name: String,
    pub title: String,
}

/// 插入新对话记录。
pub async fn insert(
    pool: &SqlitePool,
    params: &CreateConversationParams,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO conversations (id, user_id, agent_id, agent_name, title) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&params.id)
    .bind(&params.user_id)
    .bind(&params.agent_id)
    .bind(&params.agent_name)
    .bind(&params.title)
    .execute(pool)
    .await?;
    Ok(())
}

/// 查询用户在某 Agent 下的对话列表，支持归档状态过滤。
pub async fn list_by_user_and_agent(
    pool: &SqlitePool,
    user_id: &str,
    agent_name: &str,
    include_archived: bool,
) -> Result<Vec<ConversationRow>, sqlx::Error> {
    if include_archived {
        sqlx::query_as::<_, ConversationRow>(
            "SELECT id, user_id, agent_id, agent_name, title, archived_at, created_at, updated_at \
             FROM conversations \
             WHERE user_id = ? AND agent_name = ? \
             ORDER BY updated_at DESC",
        )
        .bind(user_id)
        .bind(agent_name)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, ConversationRow>(
            "SELECT id, user_id, agent_id, agent_name, title, archived_at, created_at, updated_at \
             FROM conversations \
             WHERE user_id = ? AND agent_name = ? AND archived_at IS NULL \
             ORDER BY updated_at DESC",
        )
        .bind(user_id)
        .bind(agent_name)
        .fetch_all(pool)
        .await
    }
}

/// 查询用户的对话列表，按更新时间降序。支持按 agent 过滤。
pub async fn list_by_user(
    pool: &SqlitePool,
    user_id: &str,
    agent_name_filter: Option<&str>,
) -> Result<Vec<ConversationRow>, sqlx::Error> {
    if let Some(agent_name) = agent_name_filter {
        sqlx::query_as::<_, ConversationRow>(
            "SELECT id, user_id, agent_id, agent_name, title, archived_at, created_at, updated_at \
             FROM conversations WHERE user_id = ? AND agent_name = ? ORDER BY updated_at DESC",
        )
        .bind(user_id)
        .bind(agent_name)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, ConversationRow>(
            "SELECT id, user_id, agent_id, agent_name, title, archived_at, created_at, updated_at \
             FROM conversations WHERE user_id = ? ORDER BY updated_at DESC",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
    }
}

/// 按 ID 查询对话（不校验 user_id，由调用方自行校验）。
pub async fn find_by_id(
    pool: &SqlitePool,
    conversation_id: &str,
) -> Result<Option<ConversationRow>, sqlx::Error> {
    sqlx::query_as::<_, ConversationRow>(
        "SELECT id, user_id, agent_id, agent_name, title, archived_at, created_at, updated_at \
         FROM conversations WHERE id = ?",
    )
    .bind(conversation_id)
    .fetch_optional(pool)
    .await
}

/// 按 ID + user_id 查询对话（验证归属）。
pub async fn find_by_id_and_user(
    pool: &SqlitePool,
    conversation_id: &str,
    user_id: &str,
) -> Result<Option<ConversationRow>, sqlx::Error> {
    sqlx::query_as::<_, ConversationRow>(
        "SELECT id, user_id, agent_id, agent_name, title, archived_at, created_at, updated_at \
         FROM conversations WHERE id = ? AND user_id = ?",
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// 统计某用户某 Agent 下活跃对话数量。
pub async fn count_active(
    pool: &SqlitePool,
    user_id: &str,
    agent_name: &str,
) -> Result<usize, sqlx::Error> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM conversations \
         WHERE user_id = ? AND agent_name = ? AND archived_at IS NULL",
    )
    .bind(user_id)
    .bind(agent_name)
    .fetch_one(pool)
    .await?;
    Ok(count as usize)
}

/// 归档最旧的 N 条活跃对话。
pub async fn archive_oldest(
    pool: &SqlitePool,
    user_id: &str,
    agent_name: &str,
    count: usize,
) -> Result<usize, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE conversations SET archived_at = datetime('now') \
         WHERE id IN ( \
           SELECT id FROM conversations \
           WHERE user_id = ? AND agent_name = ? AND archived_at IS NULL \
           ORDER BY updated_at ASC LIMIT ? \
         )",
    )
    .bind(user_id)
    .bind(agent_name)
    .bind(count as i64)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() as usize)
}

/// 归档指定对话。
pub async fn archive(pool: &SqlitePool, conversation_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE conversations SET archived_at = datetime('now'), updated_at = datetime('now') WHERE id = ? AND archived_at IS NULL",
    )
    .bind(conversation_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// 恢复已归档对话。
pub async fn unarchive(pool: &SqlitePool, conversation_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE conversations SET archived_at = NULL, updated_at = datetime('now') WHERE id = ? AND archived_at IS NOT NULL",
    )
    .bind(conversation_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// 更新对话标题。
pub async fn update_title(
    pool: &SqlitePool,
    conversation_id: &str,
    title: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE conversations SET title = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(title)
        .bind(conversation_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// 更新对话的 updated_at 时间戳（消息发送后调用）。
pub async fn touch(pool: &SqlitePool, conversation_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE conversations SET updated_at = datetime('now') WHERE id = ?")
        .bind(conversation_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// 删除对话。
pub async fn delete(pool: &SqlitePool, conversation_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM conversations WHERE id = ?")
        .bind(conversation_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
