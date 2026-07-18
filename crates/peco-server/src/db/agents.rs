// ============================================================================
// Agent 数据库查询函数
// ============================================================================

use sqlx::SqlitePool;

/// agents 表完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct AgentRow {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    pub model: String,
    pub provider: String,
    pub icon: String,
    pub color: String,
    pub status: String,
    pub config_json: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 创建 Agent 的参数（由 handler 层传入）。
pub struct CreateAgentParams {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    pub model: String,
    pub provider: String,
    pub icon: String,
    pub color: String,
    pub config_json: String,
}

/// 更新 Agent 的参数（所有字段可选）。
pub struct UpdateAgentParams {
    pub name: Option<String>,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub config_json: Option<String>,
}

/// 插入新 Agent 记录。
pub async fn insert(pool: &SqlitePool, params: &CreateAgentParams) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO agents (id, user_id, name, description, system_prompt, model, provider, icon, color, config_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&params.id)
    .bind(&params.user_id)
    .bind(&params.name)
    .bind(&params.description)
    .bind(&params.system_prompt)
    .bind(&params.model)
    .bind(&params.provider)
    .bind(&params.icon)
    .bind(&params.color)
    .bind(&params.config_json)
    .execute(pool)
    .await?;
    Ok(())
}

/// 查询用户的 Agent 列表。
pub async fn list_by_user(pool: &SqlitePool, user_id: &str) -> Result<Vec<AgentRow>, sqlx::Error> {
    sqlx::query_as::<_, AgentRow>(
        "SELECT id, user_id, name, description, system_prompt, model, provider, icon, color, status, config_json, created_at, updated_at \
         FROM agents WHERE user_id = ? ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// 按 ID 查询单个 Agent（不校验 user_id，由调用方自行校验）。
pub async fn find_by_id(pool: &SqlitePool, agent_id: &str) -> Result<Option<AgentRow>, sqlx::Error> {
    sqlx::query_as::<_, AgentRow>(
        "SELECT id, user_id, name, description, system_prompt, model, provider, icon, color, status, config_json, created_at, updated_at \
         FROM agents WHERE id = ?",
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await
}

/// 按 ID 和 user_id 查询单个 Agent。
pub async fn find_by_id_and_user(
    pool: &SqlitePool,
    agent_id: &str,
    user_id: &str,
) -> Result<Option<AgentRow>, sqlx::Error> {
    sqlx::query_as::<_, AgentRow>(
        "SELECT id, user_id, name, description, system_prompt, model, provider, icon, color, status, config_json, created_at, updated_at \
         FROM agents WHERE id = ? AND user_id = ?",
    )
    .bind(agent_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// 更新 Agent 的部分字段。
pub async fn update(
    pool: &SqlitePool,
    agent_id: &str,
    params: &UpdateAgentParams,
) -> Result<bool, sqlx::Error> {
    // 动态构建 SET 子句
    let mut sets: Vec<String> = Vec::new();
    let mut binds: Vec<String> = Vec::new();

    // 使用序号占位符（sqlx SQLite 支持 ?1, ?2, ...）
    if params.name.is_some() {
        sets.push(format!("name = ?{}", binds.len() + 1));
        binds.push(params.name.clone().unwrap());
    }
    if params.description.is_some() {
        sets.push(format!("description = ?{}", binds.len() + 1));
        binds.push(params.description.clone().unwrap());
    }
    if params.system_prompt.is_some() {
        sets.push(format!("system_prompt = ?{}", binds.len() + 1));
        binds.push(params.system_prompt.clone().unwrap());
    }
    if params.model.is_some() {
        sets.push(format!("model = ?{}", binds.len() + 1));
        binds.push(params.model.clone().unwrap());
    }
    if params.provider.is_some() {
        sets.push(format!("provider = ?{}", binds.len() + 1));
        binds.push(params.provider.clone().unwrap());
    }
    if params.icon.is_some() {
        sets.push(format!("icon = ?{}", binds.len() + 1));
        binds.push(params.icon.clone().unwrap());
    }
    if params.color.is_some() {
        sets.push(format!("color = ?{}", binds.len() + 1));
        binds.push(params.color.clone().unwrap());
    }
    if params.config_json.is_some() {
        sets.push(format!("config_json = ?{}", binds.len() + 1));
        binds.push(params.config_json.clone().unwrap());
    }

    if sets.is_empty() {
        return Ok(false);
    }

    sets.push(format!("updated_at = datetime('now')"));

    // 添加 agent_id 作为最后一个 bind
    let sql = format!(
        "UPDATE agents SET {} WHERE id = ?{}",
        sets.join(", "),
        binds.len() + 1
    );

    // sqlx 不支持动态 SQL 的 compile-time check，使用 query 配合动态参数
    let mut query = sqlx::query(&sql);
    for value in &binds {
        query = query.bind(value);
    }
    query = query.bind(agent_id);

    let result = query.execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

/// 更新 Agent 的状态字段。
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

/// 按名称查找用户的 Agent（用于去重检查）。
pub async fn find_by_name_and_user(
    pool: &SqlitePool,
    name: &str,
    user_id: &str,
) -> Result<Option<AgentRow>, sqlx::Error> {
    sqlx::query_as::<_, AgentRow>(
        "SELECT id, user_id, name, description, system_prompt, model, provider, icon, color, status, config_json, created_at, updated_at \
         FROM agents WHERE name = ? AND user_id = ?",
    )
    .bind(name)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// 删除 Agent 及其关联的 agent.md 文件路径记录。
pub async fn delete(pool: &SqlitePool, agent_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM agents WHERE id = ?")
        .bind(agent_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
