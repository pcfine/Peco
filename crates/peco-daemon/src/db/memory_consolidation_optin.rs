// ============================================================================
// memory_consolidation_optin 表 DAO — 自动整理用户级 opt-in 开关
// ============================================================================
//
// fail-closed 语义：**无行 = 未 opt-in**，只有显式写入 enabled=1 的用户
// 才参与自动记忆整理。控制面（同意开关）落 SQLite，数据面（记忆）留 LanceDB，
// 物理隔离 —— 同意与否不进检索面，也不会被概率召回扭曲。

use sqlx::SqlitePool;

/// 判断用户是否已 opt-in 自动整理；无行 = false。
pub async fn is_opted_in(pool: &SqlitePool, user_id: &str) -> Result<bool, sqlx::Error> {
    let enabled = sqlx::query_scalar::<_, i64>(
        "SELECT enabled FROM memory_consolidation_optin WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(enabled.unwrap_or(0) != 0)
}

/// 写入用户的 opt-in 开关（upsert）。
///
/// 语义契约：
/// - `opted_in_at` 仅在原值为 NULL 时写入 —— 首次开启时刻一经记录即保留，
///   后续幂等重开不覆盖、关闭也不清除（重新开启时能看到最初同意的时间）；
/// - `updated_at` 每次调用恒刷新为本次时刻（含关闭）。
pub async fn set_enabled(
    pool: &SqlitePool,
    user_id: &str,
    enabled: bool,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    // insert 分支：开启时记 opted_in_at = now，关闭时留 NULL（从未同意过）
    let opted_in_at = if enabled { Some(&now) } else { None };

    sqlx::query(
        "INSERT INTO memory_consolidation_optin (user_id, enabled, opted_in_at, updated_at) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT (user_id) DO UPDATE SET \
         enabled = excluded.enabled, \
         opted_in_at = COALESCE(opted_in_at, excluded.opted_in_at), \
         updated_at = excluded.updated_at",
    )
    .bind(user_id)
    .bind(i64::from(enabled))
    .bind(opted_in_at)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

/// 列出全部已 opt-in 的用户 id（按 user_id 排序）。
///
/// 自动整理 cron 的消费入口：与服务器总开关取交集后逐用户整理。
/// 空表返回空集 —— fail-closed 契约，未表态的用户永不被自动整理。
pub async fn list_opted_in(pool: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT user_id FROM memory_consolidation_optin WHERE enabled = 1 ORDER BY user_id",
    )
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

    /// 读取 opted_in_at / updated_at 原始列（DAO 不暴露行结构，测试直查）。
    async fn raw(pool: &SqlitePool, user_id: &str) -> Option<(i64, Option<String>, String)> {
        sqlx::query_as::<_, (i64, Option<String>, String)>(
            "SELECT enabled, opted_in_at, updated_at FROM memory_consolidation_optin \
             WHERE user_id = ?",
        )
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn test_is_opted_in_defaults_to_false_without_row() {
        let (pool, _dir) = test_pool().await;

        assert!(!is_opted_in(&pool, "u1").await.unwrap(), "无行 = 未 opt-in");
    }

    #[tokio::test]
    async fn test_set_enabled_true_records_opted_in_at() {
        let (pool, _dir) = test_pool().await;

        set_enabled(&pool, "u1", true).await.unwrap();

        assert!(is_opted_in(&pool, "u1").await.unwrap());
        let (enabled, opted_in_at, updated_at) = raw(&pool, "u1").await.unwrap();
        assert_eq!(enabled, 1);
        let opted_in_at = opted_in_at.expect("开启时应记录首次同意时刻");
        assert!(!opted_in_at.is_empty());
        assert_eq!(updated_at, opted_in_at, "首次开启两个时刻相同");
    }

    #[tokio::test]
    async fn test_set_enabled_is_idempotent_and_keeps_opted_in_at() {
        let (pool, _dir) = test_pool().await;

        set_enabled(&pool, "u1", true).await.unwrap();
        let first = raw(&pool, "u1").await.unwrap().1;

        set_enabled(&pool, "u1", true).await.unwrap();

        let (enabled, opted_in_at, _) = raw(&pool, "u1").await.unwrap();
        assert_eq!(enabled, 1);
        assert_eq!(opted_in_at, first, "幂等重开不得覆盖首次同意时刻");
    }

    #[tokio::test]
    async fn test_disable_then_reenable_preserves_first_opted_in_at() {
        let (pool, _dir) = test_pool().await;

        set_enabled(&pool, "u1", true).await.unwrap();
        let first = raw(&pool, "u1").await.unwrap().1;

        set_enabled(&pool, "u1", false).await.unwrap();
        assert!(!is_opted_in(&pool, "u1").await.unwrap());
        let (enabled, opted_in_at, _) = raw(&pool, "u1").await.unwrap();
        assert_eq!(enabled, 0);
        assert_eq!(opted_in_at, first, "关闭不清除首开时刻");

        set_enabled(&pool, "u1", true).await.unwrap();
        let (enabled, opted_in_at, _) = raw(&pool, "u1").await.unwrap();
        assert_eq!(enabled, 1);
        assert_eq!(opted_in_at, first, "重新开启保留最初同意时刻");
    }

    #[tokio::test]
    async fn test_disable_first_keeps_opted_in_at_null() {
        let (pool, _dir) = test_pool().await;

        // 从未同意过的用户直接关闭：不得凭空写入同意时刻
        set_enabled(&pool, "u1", false).await.unwrap();

        let (enabled, opted_in_at, _) = raw(&pool, "u1").await.unwrap();
        assert_eq!(enabled, 0);
        assert!(
            opted_in_at.is_none(),
            "未同意过的用户 opted_in_at 应保持 NULL"
        );
        assert!(!is_opted_in(&pool, "u1").await.unwrap());
    }

    #[tokio::test]
    async fn test_list_opted_in_only_returns_enabled_users_sorted() {
        let (pool, _dir) = test_pool().await;

        set_enabled(&pool, "u2", true).await.unwrap();
        set_enabled(&pool, "u1", true).await.unwrap();
        set_enabled(&pool, "u3", false).await.unwrap();

        assert_eq!(
            list_opted_in(&pool).await.unwrap(),
            vec!["u1".to_string(), "u2".to_string()],
            "只返回 enabled=1 且按 user_id 排序"
        );
    }

    #[tokio::test]
    async fn test_list_opted_in_is_empty_on_fresh_table() {
        let (pool, _dir) = test_pool().await;

        assert!(
            list_opted_in(&pool).await.unwrap().is_empty(),
            "空表 = 无人 opt-in（fail-closed：未表态的用户不被自动整理）"
        );
    }
}
