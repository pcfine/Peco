// ============================================================================
// Conversation 容量控制 — 每 Agent 上限 100 条活跃对话
// ============================================================================

use sqlx::SqlitePool;

/// 每个 Agent 最大活跃对话数。
pub const MAX_ACTIVE_PER_AGENT: usize = 100;

/// 创建对话前检查是否超限。
///
/// 若已达上限，自动归档最旧的 N 条活跃对话，确保创建成功。
/// 返回归档的对话数量。
pub async fn auto_archive_oldest_if_needed(
    pool: &SqlitePool,
    user_id: &str,
    agent_name: &str,
) -> Result<usize, sqlx::Error> {
    let active_count =
        super::super::db::conversations::count_active(pool, user_id, agent_name).await?;
    if active_count >= MAX_ACTIVE_PER_AGENT {
        let to_archive = active_count - MAX_ACTIVE_PER_AGENT + 1;
        let archived =
            super::super::db::conversations::archive_oldest(pool, user_id, agent_name, to_archive)
                .await?;
        tracing::info!(
            user_id = %user_id,
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
