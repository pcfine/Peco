// ============================================================================
// workspace_hashes 表 DAO — 模块文件哈希管理
// ============================================================================
//
// 每个用户每个模块（agents / skills / mcp / workflows / providers）
// 存储一条 SHA-256 哈希记录，用于启动时快速判断文件系统是否有变更。

use std::collections::HashMap;

use sqlx::SqlitePool;

/// 获取指定用户的所有模块哈希。
///
/// 返回 `module → hash` 的映射。若用户从未计算过哈希，返回空 HashMap。
pub async fn get_hashes(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<HashMap<String, String>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT module, hash FROM workspace_hashes WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().collect())
}

/// 获取指定用户单个模块的哈希值。
///
/// 返回 `None` 表示该模块尚未计算过哈希。
pub async fn get_hash(
    pool: &SqlitePool,
    user_id: &str,
    module: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT hash FROM workspace_hashes WHERE user_id = ? AND module = ?",
    )
    .bind(user_id)
    .bind(module)
    .fetch_optional(pool)
    .await
}

/// 插入或更新指定模块的哈希值。
///
/// 使用 `INSERT OR REPLACE` 确保幂等。
pub async fn upsert_hash(
    pool: &SqlitePool,
    user_id: &str,
    module: &str,
    hash: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT OR REPLACE INTO workspace_hashes (user_id, module, hash, updated_at) \
         VALUES (?, ?, ?, datetime('now'))",
    )
    .bind(user_id)
    .bind(module)
    .bind(hash)
    .execute(pool)
    .await?;
    Ok(())
}

/// 批量更新多个模块的哈希值（同一事务内，保证原子性）。
pub async fn upsert_hashes_batch(
    pool: &SqlitePool,
    user_id: &str,
    hashes: &HashMap<String, String>,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for (module, hash) in hashes {
        sqlx::query(
            "INSERT OR REPLACE INTO workspace_hashes (user_id, module, hash, updated_at) \
             VALUES (?, ?, ?, datetime('now'))",
        )
        .bind(user_id)
        .bind(module)
        .bind(hash)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// 删除指定用户的所有哈希记录。
///
/// 用于强制全量刷新场景。
pub async fn delete_hashes(pool: &SqlitePool, user_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM workspace_hashes WHERE user_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}
