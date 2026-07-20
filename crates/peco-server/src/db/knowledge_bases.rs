// ============================================================================
// 知识库数据库查询函数
// ============================================================================

use sqlx::SqlitePool;

/// knowledge_bases 表完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct KnowledgeBaseRow {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub description: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 创建知识库的参数（由 handler 层传入）。
pub struct CreateKbParams {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub description: String,
}

/// 插入新知识库记录。
pub async fn insert(pool: &SqlitePool, params: &CreateKbParams) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO knowledge_bases (id, user_id, name, description) VALUES (?, ?, ?, ?)")
        .bind(&params.id)
        .bind(&params.user_id)
        .bind(&params.name)
        .bind(&params.description)
        .execute(pool)
        .await?;
    Ok(())
}

/// 查询用户的知识库列表。
pub async fn list_by_user(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Vec<KnowledgeBaseRow>, sqlx::Error> {
    sqlx::query_as::<_, KnowledgeBaseRow>(
        "SELECT id, user_id, name, description, created_at, updated_at \
         FROM knowledge_bases WHERE user_id = ? ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// 按 ID 查询单个知识库。
pub async fn find_by_id(
    pool: &SqlitePool,
    kb_id: &str,
) -> Result<Option<KnowledgeBaseRow>, sqlx::Error> {
    sqlx::query_as::<_, KnowledgeBaseRow>(
        "SELECT id, user_id, name, description, created_at, updated_at \
         FROM knowledge_bases WHERE id = ?",
    )
    .bind(kb_id)
    .fetch_optional(pool)
    .await
}

/// 按 ID 和 user_id 查询单个知识库（所有权校验）。
pub async fn find_by_id_and_user(
    pool: &SqlitePool,
    kb_id: &str,
    user_id: &str,
) -> Result<Option<KnowledgeBaseRow>, sqlx::Error> {
    sqlx::query_as::<_, KnowledgeBaseRow>(
        "SELECT id, user_id, name, description, created_at, updated_at \
         FROM knowledge_bases WHERE id = ? AND user_id = ?",
    )
    .bind(kb_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// 按名称查找用户的知识库（去重检查）。
pub async fn find_by_name_and_user(
    pool: &SqlitePool,
    name: &str,
    user_id: &str,
) -> Result<Option<KnowledgeBaseRow>, sqlx::Error> {
    sqlx::query_as::<_, KnowledgeBaseRow>(
        "SELECT id, user_id, name, description, created_at, updated_at \
         FROM knowledge_bases WHERE name = ? AND user_id = ?",
    )
    .bind(name)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// 更新知识库的 updated_at 时间戳。
pub async fn touch(pool: &SqlitePool, kb_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE knowledge_bases SET updated_at = datetime('now') WHERE id = ?")
        .bind(kb_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// 删除知识库记录。
pub async fn delete(pool: &SqlitePool, kb_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM knowledge_bases WHERE id = ?")
        .bind(kb_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
