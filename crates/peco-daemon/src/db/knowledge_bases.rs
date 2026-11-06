// ============================================================================
// 知识库数据库查询函数
// ============================================================================

use sqlx::SqlitePool;

use crate::LOCAL_USER_ID;

/// knowledge_bases 表完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct KnowledgeBaseRow {
    pub id: String,
    /// 单用户身份，恒为 [`LOCAL_USER_ID`]：不是资源归属维度。
    pub user_id: String,
    /// 资源归属维度：同名 KB 在不同团队是两份资源。
    pub team_id: String,
    pub name: String,
    pub description: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 创建知识库的参数（由 handler 层传入）。
pub struct CreateKbParams {
    pub id: String,
    pub team_id: String,
    pub name: String,
    pub description: String,
}

/// 插入新知识库记录。
///
/// `user_id` 列在函数内部写死单用户身份：调用方只提供资源归属的 `team_id`。
pub async fn insert(pool: &SqlitePool, params: &CreateKbParams) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO knowledge_bases (id, user_id, team_id, name, description) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&params.id)
    .bind(LOCAL_USER_ID)
    .bind(&params.team_id)
    .bind(&params.name)
    .bind(&params.description)
    .execute(pool)
    .await?;
    Ok(())
}

/// 查询某团队的知识库列表。
pub async fn list_by_team(
    pool: &SqlitePool,
    team_id: &str,
) -> Result<Vec<KnowledgeBaseRow>, sqlx::Error> {
    sqlx::query_as::<_, KnowledgeBaseRow>(
        "SELECT id, user_id, team_id, name, description, created_at, updated_at \
         FROM knowledge_bases WHERE team_id = ? ORDER BY created_at DESC",
    )
    .bind(team_id)
    .fetch_all(pool)
    .await
}

/// 按 ID 查询单个知识库。
pub async fn find_by_id(
    pool: &SqlitePool,
    kb_id: &str,
) -> Result<Option<KnowledgeBaseRow>, sqlx::Error> {
    sqlx::query_as::<_, KnowledgeBaseRow>(
        "SELECT id, user_id, team_id, name, description, created_at, updated_at \
         FROM knowledge_bases WHERE id = ?",
    )
    .bind(kb_id)
    .fetch_optional(pool)
    .await
}

/// 按 ID 和 team_id 查询单个知识库（团队归属校验）。
pub async fn find_by_id_and_team(
    pool: &SqlitePool,
    kb_id: &str,
    team_id: &str,
) -> Result<Option<KnowledgeBaseRow>, sqlx::Error> {
    sqlx::query_as::<_, KnowledgeBaseRow>(
        "SELECT id, user_id, team_id, name, description, created_at, updated_at \
         FROM knowledge_bases WHERE id = ? AND team_id = ?",
    )
    .bind(kb_id)
    .bind(team_id)
    .fetch_optional(pool)
    .await
}

/// 按名称查找团队的知识库（去重检查）。
pub async fn find_by_name_and_team(
    pool: &SqlitePool,
    name: &str,
    team_id: &str,
) -> Result<Option<KnowledgeBaseRow>, sqlx::Error> {
    sqlx::query_as::<_, KnowledgeBaseRow>(
        "SELECT id, user_id, team_id, name, description, created_at, updated_at \
         FROM knowledge_bases WHERE name = ? AND team_id = ?",
    )
    .bind(name)
    .bind(team_id)
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

/// 删除知识库记录（限定团队）。
pub async fn delete(pool: &SqlitePool, kb_id: &str, team_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM knowledge_bases WHERE id = ? AND team_id = ?")
        .bind(kb_id)
        .bind(team_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
