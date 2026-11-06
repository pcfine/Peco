// ============================================================================
// Conversation 容量控制 — 每个空间每个 Agent 上限 100 条活跃对话
// ============================================================================

use sqlx::SqlitePool;

/// 每个 Agent 最大活跃对话数。
pub const MAX_ACTIVE_PER_AGENT: usize = 100;

/// 创建对话前检查是否超限。
///
/// 若已达上限，自动归档最旧的 N 条活跃对话，确保创建成功。
/// 返回归档的对话数量。
///
/// 上限按 `workspace_id` 计：同团队两个空间聊同一个 Agent 是两份历史，
/// 合并计数会让一个空间把另一个空间的会话顶掉。
pub async fn auto_archive_oldest_if_needed(
    pool: &SqlitePool,
    workspace_id: &str,
    agent_name: &str,
) -> Result<usize, sqlx::Error> {
    let active_count =
        crate::db::conversations::count_active(pool, workspace_id, agent_name).await?;
    if active_count >= MAX_ACTIVE_PER_AGENT {
        let to_archive = active_count - MAX_ACTIVE_PER_AGENT + 1;
        let archived =
            crate::db::conversations::archive_oldest(pool, workspace_id, agent_name, to_archive)
                .await?;
        tracing::info!(
            workspace_id = %workspace_id,
            agent_name = %agent_name,
            active_before = active_count,
            archived,
            "Auto-archived old conversations"
        );
        Ok(archived)
    } else {
        Ok(0)
    }
}
