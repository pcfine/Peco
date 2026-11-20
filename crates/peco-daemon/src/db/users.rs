//! `users` 表的账号面 —— 单用户 daemon 只有 `id = 'local'` 这一行。
//!
//! 空 `password_hash` 表示「尚未注册」：它由 `schema.sql` 的种子行写死，注册是把
//! 空串换掉的那一次写入，因此「注册过没有」不需要额外一列，也不可能与真实状态不一致。

use sqlx::SqlitePool;

use crate::LOCAL_USER_ID;

/// 本地账号。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Account {
    pub username: String,
    /// 空串 = 未注册。
    pub password_hash: String,
}

impl Account {
    pub fn is_registered(&self) -> bool {
        !self.password_hash.is_empty()
    }
}

pub async fn account(pool: &SqlitePool) -> Result<Option<Account>, sqlx::Error> {
    sqlx::query_as("SELECT username, password_hash FROM users WHERE id = ?")
        .bind(LOCAL_USER_ID)
        .fetch_optional(pool)
        .await
}

/// 注册。返回 `false` 表示已经有密码了。
///
/// 判定在 SQL 里：读取与写入之间挤进第二次注册时，`password_hash = ''` 只会命中
/// 一次，第二个 UPDATE affected=0 —— 应用层「先查再写」拦不住并发。
pub async fn register(
    pool: &SqlitePool,
    username: &str,
    password_hash: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE users SET username = ?, password_hash = ?, updated_at = datetime('now') \
         WHERE id = ? AND password_hash = ''",
    )
    .bind(username)
    .bind(password_hash)
    .bind(LOCAL_USER_ID)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 改密码。返回 `false` 表示这一行不存在（`schema.sql` 的种子行缺失）。
pub async fn set_password_hash(
    pool: &SqlitePool,
    password_hash: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE users SET password_hash = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(password_hash)
    .bind(LOCAL_USER_ID)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
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

    /// 种子行的空摘要就是「未注册」，注册把它换掉，再注册一次必须落空。
    #[tokio::test]
    async fn registration_only_lands_once() {
        let (pool, _dir) = test_pool().await;
        let account = account(&pool).await.unwrap().expect("种子行必须存在");
        assert!(!account.is_registered());

        assert!(register(&pool, "peco", "hash-1").await.unwrap());
        let account = self::account(&pool).await.unwrap().unwrap();
        assert!(account.is_registered());
        assert_eq!(account.username, "peco");

        assert!(
            !register(&pool, "intruder", "hash-2").await.unwrap(),
            "第二次注册必须 affected=0"
        );
        assert_eq!(
            self::account(&pool).await.unwrap().unwrap().username,
            "peco",
            "失败的那次不得改名"
        );
    }

    /// 并发的两次注册只能有一个成功 —— 判定在 UPDATE 的谓词里才成立。
    #[tokio::test]
    async fn concurrent_registrations_have_exactly_one_winner() {
        let (pool, _dir) = test_pool().await;
        let first = register(&pool, "first", "hash-1");
        let second = register(&pool, "second", "hash-2");
        let (first, second) = tokio::join!(first, second);

        let winners = [first.unwrap(), second.unwrap()]
            .iter()
            .filter(|won| **won)
            .count();
        assert_eq!(winners, 1, "并发注册只有一个能成功");
    }

    #[tokio::test]
    async fn changing_the_password_overwrites_the_old_hash() {
        let (pool, _dir) = test_pool().await;
        assert!(register(&pool, "peco", "hash-1").await.unwrap());
        assert!(set_password_hash(&pool, "hash-2").await.unwrap());
        assert_eq!(
            account(&pool).await.unwrap().unwrap().password_hash,
            "hash-2"
        );
    }
}
