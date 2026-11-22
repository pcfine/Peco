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

/// 查询对话的消息列表，返回**最新的一页**，页内按时间升序。
///
/// 分页从最新一端往回数：`offset=0` 是最后 `limit` 条，`offset=limit` 再往前一页。
/// 聊天历史是「打开会话就看到最近聊了什么」，从最旧一端分页会让超过 `limit` 条
/// 的会话永远看不到最新内容（前端只取一页，且没有「加载更多」的入口）。
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
         FROM ( \
             SELECT id, conversation_id, role, content, agent_id, agent_name, created_at, \
                    rowid AS seq \
             FROM messages WHERE conversation_id = ? \
             ORDER BY created_at DESC, seq DESC LIMIT ? OFFSET ? \
         ) ORDER BY created_at ASC, seq ASC",
    )
    .bind(conversation_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = db::connect(&url).await.unwrap();
        db::run_migrations(&pool).await.unwrap();
        (pool, dir)
    }

    async fn seed(pool: &SqlitePool, conversation_id: &str, count: usize) {
        // created_at 全部同秒：分页与页内顺序都只能靠 rowid tiebreak，正是
        // 同一轮 user / assistant 行同秒落库的真实形态。
        for index in 0..count {
            sqlx::query(
                "INSERT INTO messages (id, conversation_id, role, content, created_at) \
                 VALUES (?, ?, 'user', ?, '2026-01-01 00:00:00')",
            )
            .bind(format!("{conversation_id}-m{index}"))
            .bind(conversation_id)
            .bind(format!("m{index}"))
            .execute(pool)
            .await
            .unwrap();
        }
    }

    /// 一页是**最新**的 limit 条（页内升序），offset 从最新一端往回数。
    ///
    /// 变异方法：内层改回 `ORDER BY created_at ASC ... LIMIT ? OFFSET ?` →
    /// `offset=0` 拿到最旧的 3 条，断言变红。
    #[tokio::test]
    async fn list_returns_newest_page_in_ascending_order() {
        let (pool, _dir) = test_pool().await;
        super::super::conversations::insert(
            &pool,
            &super::super::conversations::CreateConversationParams {
                id: "c1".into(),
                workspace_id: "ws_1".into(),
                agent_id: None,
                agent_name: "@master".into(),
                title: "t".into(),
            },
        )
        .await
        .unwrap();
        seed(&pool, "c1", 5).await;
        // 另一条会话的消息不能混进来。
        super::super::conversations::insert(
            &pool,
            &super::super::conversations::CreateConversationParams {
                id: "c2".into(),
                workspace_id: "ws_1".into(),
                agent_id: None,
                agent_name: "@master".into(),
                title: "t".into(),
            },
        )
        .await
        .unwrap();
        seed(&pool, "c2", 2).await;

        let page = list_by_conversation(&pool, "c1", 0, 3).await.unwrap();
        let ids: Vec<&str> = page.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, vec!["c1-m2", "c1-m3", "c1-m4"], "最新一页");

        let older = list_by_conversation(&pool, "c1", 3, 3).await.unwrap();
        let ids: Vec<&str> = older.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, vec!["c1-m0", "c1-m1"], "往回翻页");

        let beyond = list_by_conversation(&pool, "c1", 5, 3).await.unwrap();
        assert!(beyond.is_empty(), "越界页为空");
    }
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
