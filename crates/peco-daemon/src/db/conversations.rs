// ============================================================================
// Conversations 数据库查询函数
// ============================================================================

use sqlx::SqlitePool;

use crate::LOCAL_USER_ID;

/// conversations 表完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct ConversationRow {
    pub id: String,
    /// 单用户身份，不是会话归属维度：会话按 `workspace_id` 隔离。
    pub user_id: String,
    pub agent_id: Option<String>,
    pub agent_name: String,
    /// 会话归属的空间：同团队两个空间聊同一个 agent 各有独立历史。
    pub workspace_id: String,
    pub title: String,
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 创建对话的参数。
pub struct CreateConversationParams {
    pub id: String,
    pub workspace_id: String,
    pub agent_id: Option<String>,
    pub agent_name: String,
    pub title: String,
}

/// 插入新对话记录。
///
/// `user_id` 列在函数内部写死单用户身份：调用方只提供会话归属的 `workspace_id`。
pub async fn insert(
    pool: &SqlitePool,
    params: &CreateConversationParams,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO conversations (id, user_id, workspace_id, agent_id, agent_name, title) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&params.id)
    .bind(LOCAL_USER_ID)
    .bind(&params.workspace_id)
    .bind(&params.agent_id)
    .bind(&params.agent_name)
    .bind(&params.title)
    .execute(pool)
    .await?;
    Ok(())
}

/// 幂等创建 peco 私有会话行（每个空间一条）。
///
/// `artifacts.conversation_id` 的外键要求该行先存在，否则产物登记会被数据库
/// 拒绝。id / agent_name / title 由调用方给出：peco 的会话 id 格式属于上层
/// 知识，嵌进 db 层会让两层各持一份格式约定。
///
/// 单条 upsert 而非 check-then-insert：两条桥并发懒建时，后者会输掉竞态并需要
/// 外部事务；SQLite 在同一条语句内完成「不存在则插」，既无窗口也不需事务。
pub async fn ensure_private_session(
    pool: &SqlitePool,
    user_id: &str,
    workspace_id: &str,
    conversation_id: &str,
    agent_name: &str,
    title: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO conversations (id, user_id, workspace_id, agent_id, agent_name, title, scope) \
         VALUES (?, ?, ?, NULL, ?, ?, 'peco') \
         ON CONFLICT(id) DO NOTHING",
    )
    .bind(conversation_id)
    .bind(user_id)
    .bind(workspace_id)
    .bind(agent_name)
    .bind(title)
    .execute(pool)
    .await?;
    Ok(())
}

/// 空间页单会话的标题：一个空间只有这一条 `scope='space'` 行，标题不承担
/// 区分职责，由界面统一显示。
const SPACE_SESSION_TITLE: &str = "空间对话";

/// 取该空间该 agent 的唯一 `scope='space'` 会话；不存在则建一条。
///
/// 返回 `(conversation_id, 是否本次新建)`。
///
/// 用「查 → 插 → 撞键回查」而不是 `ON CONFLICT(workspace_id, agent_name)`：
/// SQLite 对带 `WHERE` 的部分唯一索引做冲突目标推断并不可靠，写错目标只会在
/// 运行时报语法错。回查把并发留给索引：两个请求同时查空之后只有一个插得进去，
/// 输的那个读回赢家的 id —— 幂等由唯一索引保证，不靠应用层加锁。
pub async fn ensure_space_session(
    pool: &SqlitePool,
    workspace_id: &str,
    agent_name: &str,
) -> Result<(String, bool), sqlx::Error> {
    if let Some(existing) = find_space_session(pool, workspace_id, agent_name).await? {
        return Ok((existing, false));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let inserted = sqlx::query(
        "INSERT INTO conversations (id, user_id, workspace_id, agent_id, agent_name, title, scope) \
         VALUES (?, ?, ?, NULL, ?, ?, 'space')",
    )
    .bind(&id)
    .bind(LOCAL_USER_ID)
    .bind(workspace_id)
    .bind(agent_name)
    .bind(SPACE_SESSION_TITLE)
    .execute(pool)
    .await;

    match inserted {
        Ok(_) => Ok((id, true)),
        // 唯一索引挡下的是「另一个请求刚刚插成功」这一种失败；只有回查到行才算
        // 竞态，查不到说明是别的错误（外键、磁盘），原样上报。
        Err(e) => match find_space_session(pool, workspace_id, agent_name).await? {
            Some(winner) => Ok((winner, false)),
            None => Err(e),
        },
    }
}

async fn find_space_session(
    pool: &SqlitePool,
    workspace_id: &str,
    agent_name: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT id FROM conversations \
         WHERE scope = 'space' AND workspace_id = ? AND agent_name = ? \
         LIMIT 1",
    )
    .bind(workspace_id)
    .bind(agent_name)
    .fetch_optional(pool)
    .await
}

/// 查询某空间在某 Agent 下的对话列表，支持归档状态过滤。
pub async fn list_by_workspace_and_agent(
    pool: &SqlitePool,
    workspace_id: &str,
    agent_name: &str,
    include_archived: bool,
) -> Result<Vec<ConversationRow>, sqlx::Error> {
    if include_archived {
        sqlx::query_as::<_, ConversationRow>(
            "SELECT id, user_id, agent_id, agent_name, workspace_id, title, archived_at, created_at, updated_at \
             FROM conversations \
             WHERE workspace_id = ? AND agent_name = ? AND scope = 'agent' \
             ORDER BY updated_at DESC",
        )
        .bind(workspace_id)
        .bind(agent_name)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, ConversationRow>(
            "SELECT id, user_id, agent_id, agent_name, workspace_id, title, archived_at, created_at, updated_at \
             FROM conversations \
             WHERE workspace_id = ? AND agent_name = ? AND archived_at IS NULL \
               AND scope = 'agent' \
             ORDER BY updated_at DESC",
        )
        .bind(workspace_id)
        .bind(agent_name)
        .fetch_all(pool)
        .await
    }
}

/// 查询某空间的对话列表，按更新时间降序。支持按 agent 过滤。
pub async fn list_by_workspace(
    pool: &SqlitePool,
    workspace_id: &str,
    agent_name_filter: Option<&str>,
) -> Result<Vec<ConversationRow>, sqlx::Error> {
    if let Some(agent_name) = agent_name_filter {
        sqlx::query_as::<_, ConversationRow>(
            "SELECT id, user_id, agent_id, agent_name, workspace_id, title, archived_at, created_at, updated_at \
             FROM conversations \
             WHERE workspace_id = ? AND agent_name = ? AND scope = 'agent' \
             ORDER BY updated_at DESC",
        )
        .bind(workspace_id)
        .bind(agent_name)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, ConversationRow>(
            "SELECT id, user_id, agent_id, agent_name, workspace_id, title, archived_at, created_at, updated_at \
             FROM conversations WHERE workspace_id = ? AND scope = 'agent' \
             ORDER BY updated_at DESC",
        )
        .bind(workspace_id)
        .fetch_all(pool)
        .await
    }
}

/// 按 ID 查询对话（不校验归属，由调用方自行校验）。
///
/// 不过滤 `scope`：产物列表端点要靠它（及其归属版）命中 peco 私有会话。
pub async fn find_by_id(
    pool: &SqlitePool,
    conversation_id: &str,
) -> Result<Option<ConversationRow>, sqlx::Error> {
    sqlx::query_as::<_, ConversationRow>(
        "SELECT id, user_id, agent_id, agent_name, workspace_id, title, archived_at, created_at, updated_at \
         FROM conversations WHERE id = ?",
    )
    .bind(conversation_id)
    .fetch_optional(pool)
    .await
}

/// 按 ID + workspace_id 查询对话（验证归属）。
///
/// 同样不过滤 `scope` —— 私有会话的产物列表必须能通过这道归属校验。
pub async fn find_by_id_and_workspace(
    pool: &SqlitePool,
    conversation_id: &str,
    workspace_id: &str,
) -> Result<Option<ConversationRow>, sqlx::Error> {
    sqlx::query_as::<_, ConversationRow>(
        "SELECT id, user_id, agent_id, agent_name, workspace_id, title, archived_at, created_at, updated_at \
         FROM conversations WHERE id = ? AND workspace_id = ?",
    )
    .bind(conversation_id)
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
}

/// 按 ID + workspace_id 查 **agent 会话** —— 删除端点的 404 判定。
///
/// 删除私有会话会经 `ON DELETE CASCADE` 连带删掉它的产物行，所以删除入口必须
/// 用本函数；而 [`find_by_id_and_workspace`] 不能改成这样，产物列表端点依赖它
/// 命中私有会话。
pub async fn find_agent_conversation_by_id_and_workspace(
    pool: &SqlitePool,
    conversation_id: &str,
    workspace_id: &str,
) -> Result<Option<ConversationRow>, sqlx::Error> {
    sqlx::query_as::<_, ConversationRow>(
        "SELECT id, user_id, agent_id, agent_name, workspace_id, title, archived_at, created_at, updated_at \
         FROM conversations WHERE id = ? AND workspace_id = ? AND scope = 'agent'",
    )
    .bind(conversation_id)
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
}

/// 统计某空间某 Agent 下活跃对话数量。
pub async fn count_active(
    pool: &SqlitePool,
    workspace_id: &str,
    agent_name: &str,
) -> Result<usize, sqlx::Error> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM conversations \
         WHERE workspace_id = ? AND agent_name = ? AND archived_at IS NULL AND scope = 'agent'",
    )
    .bind(workspace_id)
    .bind(agent_name)
    .fetch_one(pool)
    .await?;
    Ok(count as usize)
}

/// 归档某空间最旧的 N 条活跃对话。
pub async fn archive_oldest(
    pool: &SqlitePool,
    workspace_id: &str,
    agent_name: &str,
    count: usize,
) -> Result<usize, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE conversations SET archived_at = datetime('now') \
         WHERE id IN ( \
           SELECT id FROM conversations \
           WHERE workspace_id = ? AND agent_name = ? AND archived_at IS NULL \
             AND scope = 'agent' \
           ORDER BY updated_at ASC LIMIT ? \
         )",
    )
    .bind(workspace_id)
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
