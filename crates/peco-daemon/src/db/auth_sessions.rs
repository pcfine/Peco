//! `auth_sessions` 的 DAO —— 浏览器会话。
//!
//! 库里只落票据的 sha256 摘要：库文件泄漏不等于会话泄漏，摘要也换不回 cookie。
//! 时间列一律 UTC 文本（与 `datetime('now')` 同格式），时间算术全部交给 SQLite，
//! 免得进程与库各算一遍、各用一套时区。

use sqlx::SqlitePool;

use crate::LOCAL_USER_ID;

/// 会话有效期，与 [`crate::web_auth`] 的 cookie `Max-Age` 同一量级。
const SESSION_TTL: &str = "+30 days";

/// 滑动续期的节流窗口。
///
/// 每个请求都写一行会让只读轮询把 WAL 撑起来；续期精度到分钟级就够，会话过期
/// 还有 30 天。
const TOUCH_WINDOW: &str = "-5 minutes";

pub struct NewSession<'a> {
    pub id: &'a str,
    /// 票据的 sha256 摘要。
    pub token_hash: &'a str,
    pub user_agent: Option<&'a str>,
    pub ip: Option<&'a str>,
}

/// 未过期的一条会话。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ValidSession {
    pub id: String,
    /// 距上次活跃已过节流窗口 —— 调用方据此决定要不要续期。
    pub stale: bool,
}

pub async fn insert(pool: &SqlitePool, session: &NewSession<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO auth_sessions \
         (id, user_id, token_hash, expires_at, last_seen_at, user_agent, ip) \
         VALUES (?, ?, ?, datetime('now', ?), datetime('now'), ?, ?)",
    )
    .bind(session.id)
    .bind(LOCAL_USER_ID)
    .bind(session.token_hash)
    .bind(SESSION_TTL)
    .bind(session.user_agent)
    .bind(session.ip)
    .execute(pool)
    .await?;
    Ok(())
}

/// 按摘要找未过期的会话。过期的行照旧留在表里，由 [`purge_expired`] 回收 ——
/// 每次登录都删一遍过期行即可，没有必要在读路径上写库。
pub async fn find_valid(
    pool: &SqlitePool,
    token_hash: &str,
) -> Result<Option<ValidSession>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, last_seen_at <= datetime('now', ?) AS stale FROM auth_sessions \
         WHERE token_hash = ? AND expires_at > datetime('now')",
    )
    .bind(TOUCH_WINDOW)
    .bind(token_hash)
    .fetch_optional(pool)
    .await
}

/// 滑动续期：`last_seen_at` 与 `expires_at` 一起推。
pub async fn touch(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE auth_sessions SET last_seen_at = datetime('now'), expires_at = datetime('now', ?) \
         WHERE id = ?",
    )
    .bind(SESSION_TTL)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM auth_sessions WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 吊销该用户的其它会话。`keep` 是当前这次请求用的会话，改完密码自己不该被踢下线。
pub async fn delete_others(
    pool: &SqlitePool,
    user_id: &str,
    keep: Option<&str>,
) -> Result<u64, sqlx::Error> {
    let result = match keep {
        Some(keep) => {
            sqlx::query("DELETE FROM auth_sessions WHERE user_id = ? AND id != ?")
                .bind(user_id)
                .bind(keep)
                .execute(pool)
                .await?
        }
        None => {
            sqlx::query("DELETE FROM auth_sessions WHERE user_id = ?")
                .bind(user_id)
                .execute(pool)
                .await?
        }
    };
    Ok(result.rows_affected())
}

pub async fn purge_expired(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM auth_sessions WHERE expires_at <= datetime('now')")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        (pool, dir)
    }

    async fn seed(pool: &SqlitePool, id: &str, hash: &str) {
        insert(
            pool,
            &NewSession {
                id,
                token_hash: hash,
                user_agent: Some("test-agent"),
                ip: Some("192.0.2.1"),
            },
        )
        .await
        .unwrap();
    }

    /// 摘要能查到会话，过期行查不到；刚建的会话不算陈旧。
    #[tokio::test]
    async fn only_live_sessions_are_found() {
        let (pool, _dir) = test_pool().await;
        seed(&pool, "s1", "hash-1").await;

        let found = find_valid(&pool, "hash-1").await.unwrap().unwrap();
        assert_eq!(found.id, "s1");
        assert!(!found.stale, "刚建的会话不该立刻触发续期");
        assert!(find_valid(&pool, "hash-unknown").await.unwrap().is_none());

        sqlx::query("UPDATE auth_sessions SET expires_at = datetime('now', '-1 second')")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            find_valid(&pool, "hash-1").await.unwrap().is_none(),
            "过期会话必须判否"
        );
    }

    /// 续期要同时推 `last_seen_at` 与 `expires_at`：只推一个的话滑动窗口不成立。
    ///
    /// 变异方法：把 `touch` 里的 `expires_at` 去掉 ⇒ 过期时刻不再被推后的断言变红。
    #[tokio::test]
    async fn touch_slides_both_timestamps() {
        let (pool, _dir) = test_pool().await;
        seed(&pool, "s1", "hash-1").await;
        sqlx::query(
            "UPDATE auth_sessions SET last_seen_at = datetime('now', '-1 hour'), \
             expires_at = datetime('now', '+1 hour') WHERE id = 's1'",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(find_valid(&pool, "hash-1").await.unwrap().unwrap().stale);
        let before: String = sqlx::query_scalar("SELECT expires_at FROM auth_sessions")
            .fetch_one(&pool)
            .await
            .unwrap();

        touch(&pool, "s1").await.unwrap();
        assert!(!find_valid(&pool, "hash-1").await.unwrap().unwrap().stale);
        let after: String = sqlx::query_scalar("SELECT expires_at FROM auth_sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(after > before, "过期时刻必须被推后：{before} → {after}");
    }

    /// 改密码踢掉其它会话、留下当前这条。
    #[tokio::test]
    async fn other_sessions_are_revoked_but_the_current_one_survives() {
        let (pool, _dir) = test_pool().await;
        seed(&pool, "s1", "hash-1").await;
        seed(&pool, "s2", "hash-2").await;

        assert_eq!(
            delete_others(&pool, LOCAL_USER_ID, Some("s1"))
                .await
                .unwrap(),
            1
        );
        assert!(find_valid(&pool, "hash-1").await.unwrap().is_some());
        assert!(find_valid(&pool, "hash-2").await.unwrap().is_none());

        seed(&pool, "s3", "hash-3").await;
        assert_eq!(delete_others(&pool, LOCAL_USER_ID, None).await.unwrap(), 2);
        assert!(find_valid(&pool, "hash-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_and_purge_remove_rows() {
        let (pool, _dir) = test_pool().await;
        seed(&pool, "s1", "hash-1").await;
        seed(&pool, "s2", "hash-2").await;

        assert_eq!(delete(&pool, "s1").await.unwrap(), 1);
        assert!(find_valid(&pool, "hash-1").await.unwrap().is_none());

        sqlx::query("UPDATE auth_sessions SET expires_at = datetime('now', '-1 second')")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(purge_expired(&pool).await.unwrap(), 1);
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM auth_sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }
}
