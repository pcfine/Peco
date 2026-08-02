// ============================================================================
// 工作空间 DB 双向同步 — 将文件系统状态同步到 SQLite 索引
// ============================================================================
//
// 独立的 DB 工具函数，不耦合 workspace 模块，避免 file_watcher ↔ workspace 循环依赖。

use std::collections::{HashMap, HashSet};

use peco_core::workspace::WorkSpace;
use sqlx::SqlitePool;
use tracing;

/// Agent 模块 DB 双向同步。
///
/// 1. 磁盘有、DB 无 → 自动注册
/// 2. DB 有、磁盘无 → 清理僵尸
/// 3. 描述不一致 → 更新
pub async fn sync_agents_with_db(user_id: &str, db: &SqlitePool, ws: &WorkSpace) {
    use crate::db::agents::{self, CreateAgentParams, UpdateAgentParams};

    let db_rows = match agents::list_index_by_user(db, user_id).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(%user_id, error = %e, "Failed to list agents from DB for sync");
            return;
        }
    };

    let disk_metas = ws.agent_manager().list_meta();
    let disk_names: HashSet<String> = disk_metas.iter().map(|m| m.name.clone()).collect();
    let db_names: HashSet<String> = db_rows.iter().map(|r| r.name.clone()).collect();

    // 1. 磁盘有、DB 无 → 自动注册
    for meta in &disk_metas {
        if !db_names.contains(&meta.name) {
            let params = CreateAgentParams {
                id: uuid::Uuid::new_v4().to_string(),
                user_id: user_id.to_string(),
                name: meta.name.clone(),
                description: meta.description.clone(),
                icon: "🤖".to_string(),
                color: "#6366f1".to_string(),
                background_color: String::new(),
            };
            match agents::insert(db, &params).await {
                Ok(()) => {
                    tracing::info!(
                        agent = %meta.name,
                        "Auto-registered agent from disk during sync"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        agent = %meta.name,
                        error = %e,
                        "Failed to auto-register agent during sync"
                    );
                }
            }
        }
    }

    // 2. DB 有、磁盘无 → 清理僵尸
    for db_name in db_names.difference(&disk_names) {
        if let Ok(Some(id)) = agents::find_id_by_name_and_user(db, db_name, user_id).await {
            match agents::delete(db, &id).await {
                Ok(true) => {
                    tracing::info!(
                        agent = %db_name,
                        "Cleaned up zombie agent from DB during sync"
                    );
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(
                        agent = %db_name,
                        error = %e,
                        "Failed to clean up zombie agent during sync"
                    );
                }
            }
        }
    }

    // 3. 描述不一致 → 更新
    let disk_desc: HashMap<&str, &str> = disk_metas
        .iter()
        .map(|m| (m.name.as_str(), m.description.as_str()))
        .collect();
    for row in &db_rows {
        if let Some(disk_desc) = disk_desc.get(row.name.as_str())
            && *disk_desc != row.description
        {
            if let Err(e) = agents::update(
                db,
                &row.id,
                &UpdateAgentParams {
                    name: None,
                    description: Some(disk_desc.to_string()),
                    icon: None,
                    color: None,
                    background_color: None,
                },
            )
            .await
            {
                tracing::warn!(
                    agent = %row.name,
                    error = %e,
                    "Failed to sync agent description"
                );
            } else {
                tracing::debug!(
                    agent = %row.name,
                    "Synced agent description from file to DB"
                );
            }
        }
    }
}
