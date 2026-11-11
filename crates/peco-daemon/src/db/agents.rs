// ============================================================================
// Agent 数据库查询函数（轻量索引）
// ============================================================================
//
// 完整 Agent 配置（model, provider, system_prompt, tools, mcp, skills）
// 存储为 agents/{name}/agent.md 文件。DB 仅保存索引和 UI 元数据。

use sqlx::SqlitePool;

use crate::LOCAL_USER_ID;

/// agents 表轻量索引行。
///
/// 不含 system_prompt / model / provider / config_json —
/// 这些字段从 agent.md 文件读取。
#[derive(Debug, sqlx::FromRow)]
pub struct AgentRow {
    pub id: String,
    /// 单用户身份，恒为 [`LOCAL_USER_ID`]：不是资源归属维度。
    pub user_id: String,
    /// 资源归属维度：同名 agent 在不同团队是不同的行。
    pub team_id: String,
    pub name: String,        // 对应 agents/{name}/ 目录名
    pub description: String, // 缓存副本，来自 agent.md YAML（列表加速）
    pub status: String,      // 运行时状态
    pub created_at: String,
    pub updated_at: String,
}

/// 创建 Agent 的索引参数（由 handler 层传入）。
pub struct CreateAgentParams {
    pub id: String,
    pub team_id: String,
    pub name: String,
    pub description: String,
}

/// 更新 Agent 索引的参数（所有字段可选）。
pub struct UpdateAgentParams {
    pub name: Option<String>,
    pub description: Option<String>,
}

// ── 查询函数 ──────────────────────────────────────────────────────────────────────

/// 列表查询：只查索引列，不含大字段。
pub async fn list_index_by_team(
    pool: &SqlitePool,
    team_id: &str,
) -> Result<Vec<AgentRow>, sqlx::Error> {
    sqlx::query_as::<_, AgentRow>(
        "SELECT id, user_id, team_id, name, description, status, created_at, updated_at \
         FROM agents WHERE team_id = ? ORDER BY created_at DESC",
    )
    .bind(team_id)
    .fetch_all(pool)
    .await
}

/// 按 ID 查 name（用于 handler 中获取目录名）。
pub async fn find_name_by_id(
    pool: &SqlitePool,
    agent_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT name FROM agents WHERE id = ?")
        .bind(agent_id)
        .fetch_optional(pool)
        .await
}

/// 按 ID + team_id 查完整索引行（团队归属校验）。
pub async fn find_index_by_id_and_team(
    pool: &SqlitePool,
    agent_id: &str,
    team_id: &str,
) -> Result<Option<AgentRow>, sqlx::Error> {
    sqlx::query_as::<_, AgentRow>(
        "SELECT id, user_id, team_id, name, description, status, created_at, updated_at \
         FROM agents WHERE id = ? AND team_id = ?",
    )
    .bind(agent_id)
    .bind(team_id)
    .fetch_optional(pool)
    .await
}

/// 按 name + team_id 查 id（用于唯一性检查和 name→id 查找）。
pub async fn find_id_by_name_and_team(
    pool: &SqlitePool,
    name: &str,
    team_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT id FROM agents WHERE name = ? AND team_id = ?")
        .bind(name)
        .bind(team_id)
        .fetch_optional(pool)
        .await
}

// ── 写入函数 ──────────────────────────────────────────────────────────────────────

/// 插入新 Agent（仅索引列）。
///
/// `user_id` 列在函数内部写死单用户身份：调用方只提供资源归属的 `team_id`，
/// 免得把空间或团队塞进用户列。
pub async fn insert(pool: &SqlitePool, params: &CreateAgentParams) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO agents (id, user_id, team_id, name, description) VALUES (?, ?, ?, ?, ?)",
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

/// 插入一行索引；`(team_id, name)` 已存在时什么都不做。
///
/// 返回 1 表示本次真的写了新行 —— 「磁盘有、库里没有」的补行靠它计数，
/// 重复调用必须是 0。`status` 列留给 DDL 默认值：它是纯运行时字段，
/// 磁盘上的 `agent.md` 没有任何对应来源。
pub async fn insert_if_absent(
    pool: &SqlitePool,
    team_id: &str,
    name: &str,
    description: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO agents (id, user_id, team_id, name, description) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(team_id, name) DO NOTHING",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(LOCAL_USER_ID)
    .bind(team_id)
    .bind(name)
    .bind(description)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 部分更新（仅索引列）。
///
/// 返回 `true` 表示至少一行被更新，`false` 表示未找到匹配行。
pub async fn update(
    pool: &SqlitePool,
    agent_id: &str,
    team_id: &str,
    params: &UpdateAgentParams,
) -> Result<bool, sqlx::Error> {
    let mut sets: Vec<String> = Vec::new();
    let mut binds: Vec<String> = Vec::new();

    if let Some(ref name) = params.name {
        sets.push(format!("name = ?{}", binds.len() + 1));
        binds.push(name.clone());
    }
    if let Some(ref description) = params.description {
        sets.push(format!("description = ?{}", binds.len() + 1));
        binds.push(description.clone());
    }
    if sets.is_empty() {
        return Ok(false);
    }

    sets.push("updated_at = datetime('now')".to_string());

    let sql = format!(
        "UPDATE agents SET {} WHERE id = ?{} AND team_id = ?{}",
        sets.join(", "),
        binds.len() + 1,
        binds.len() + 2
    );

    let mut query = sqlx::query(&sql);
    for value in &binds {
        query = query.bind(value);
    }
    query = query.bind(agent_id).bind(team_id);

    let result = query.execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

/// 更新 Agent 的运行状态字段。
pub async fn update_status(
    pool: &SqlitePool,
    agent_id: &str,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE agents SET status = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(status)
        .bind(agent_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// 删除 Agent 索引记录（限定团队：别的团队的同行 id 不在本团队的删除权内）。
pub async fn delete(pool: &SqlitePool, agent_id: &str, team_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM agents WHERE id = ? AND team_id = ?")
        .bind(agent_id)
        .bind(team_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
