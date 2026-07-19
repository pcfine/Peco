// ============================================================================
// Agent 数据库查询函数（轻量索引）
// ============================================================================
//
// 完整 Agent 配置（model, provider, system_prompt, tools, mcp, skills）
// 存储为 agents/{name}/agent.md 文件。DB 仅保存索引和 UI 元数据。

use sqlx::SqlitePool;

/// agents 表轻量索引行。
///
/// 不含 system_prompt / model / provider / config_json —
/// 这些字段从 agent.md 文件读取。
#[derive(Debug, sqlx::FromRow)]
pub struct AgentRow {
    pub id: String,
    pub user_id: String,
    pub name: String,        // 对应 agents/{name}/ 目录名
    pub description: String, // 缓存副本，来自 agent.md YAML（列表加速）
    pub icon: String,        // 纯 UI
    pub color: String,       // 纯 UI
    pub status: String,      // 运行时状态
    pub created_at: String,
    pub updated_at: String,
}

/// 创建 Agent 的索引参数（由 handler 层传入）。
pub struct CreateAgentParams {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub color: String,
}

/// 更新 Agent 索引的参数（所有字段可选）。
pub struct UpdateAgentParams {
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
}

// ── 查询函数 ──────────────────────────────────────────────────────────────────────

/// 列表查询：只查索引列，不含大字段。
pub async fn list_index_by_user(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Vec<AgentRow>, sqlx::Error> {
    sqlx::query_as::<_, AgentRow>(
        "SELECT id, user_id, name, description, icon, color, status, created_at, updated_at \
         FROM agents WHERE user_id = ? ORDER BY created_at DESC",
    )
    .bind(user_id)
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

/// 按 ID + user_id 查完整索引行（含权限校验）。
pub async fn find_index_by_id_and_user(
    pool: &SqlitePool,
    agent_id: &str,
    user_id: &str,
) -> Result<Option<AgentRow>, sqlx::Error> {
    sqlx::query_as::<_, AgentRow>(
        "SELECT id, user_id, name, description, icon, color, status, created_at, updated_at \
         FROM agents WHERE id = ? AND user_id = ?",
    )
    .bind(agent_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// 按 name + user_id 查 id（用于唯一性检查和 name→id 查找）。
pub async fn find_id_by_name_and_user(
    pool: &SqlitePool,
    name: &str,
    user_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT id FROM agents WHERE name = ? AND user_id = ?")
        .bind(name)
        .bind(user_id)
        .fetch_optional(pool)
        .await
}

// ── 写入函数 ──────────────────────────────────────────────────────────────────────

/// 插入新 Agent（仅索引列）。
pub async fn insert(pool: &SqlitePool, params: &CreateAgentParams) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO agents (id, user_id, name, description, icon, color) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&params.id)
    .bind(&params.user_id)
    .bind(&params.name)
    .bind(&params.description)
    .bind(&params.icon)
    .bind(&params.color)
    .execute(pool)
    .await?;
    Ok(())
}

/// 部分更新（仅索引列）。
///
/// 返回 `true` 表示至少一行被更新，`false` 表示未找到匹配行。
pub async fn update(
    pool: &SqlitePool,
    agent_id: &str,
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
    if let Some(ref icon) = params.icon {
        sets.push(format!("icon = ?{}", binds.len() + 1));
        binds.push(icon.clone());
    }
    if let Some(ref color) = params.color {
        sets.push(format!("color = ?{}", binds.len() + 1));
        binds.push(color.clone());
    }

    if sets.is_empty() {
        return Ok(false);
    }

    sets.push("updated_at = datetime('now')".to_string());

    let sql = format!(
        "UPDATE agents SET {} WHERE id = ?{}",
        sets.join(", "),
        binds.len() + 1
    );

    let mut query = sqlx::query(&sql);
    for value in &binds {
        query = query.bind(value);
    }
    query = query.bind(agent_id);

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

/// 删除 Agent 索引记录。
pub async fn delete(pool: &SqlitePool, agent_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM agents WHERE id = ?")
        .bind(agent_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
