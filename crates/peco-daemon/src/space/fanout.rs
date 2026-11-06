//! 团队级 agent 缓存失效扇出。
//!
//! 同 team 的 N 个空间共用一份 `teams/<team_id>/agents/`，但各自持有一份
//! `AgentManager`，两级缓存都在实例内部。一次写入只刷发起方自己的缓存，
//! 另一个已打开的空间会继续用旧 `Arc<Agent>`（含 provider、工具集）——
//! 列表、派发、会话全都在用旧配置。
//!
//! 失效必须两段都做：`rescan()` 只重建 Tier-1 元数据，Tier-2 的已加载实例
//! 仍然留在缓存里；只 rescan 的实现看起来「刷新了」，读到的仍是旧正文。

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use peco_core::tools::WorkspaceHub;
use sqlx::SqlitePool;
use tracing::{debug, info, warn};

use crate::{db::agents, db::workspaces, state::AppState};

/// 刷新同 team 所有已打开空间的 agent 缓存，返回实际刷新的空间数。
///
/// 只统计在实例表里真的找到的空间：库里未关闭但实例已被 drop 的行不算，
/// 否则调用方无法用返回值判断扇出是否覆盖到了正在读的实例。
///
/// 缓存刷新是尽力而为：查库失败只 warn 并返回 0。调用方（写接口、文件监听）
/// 的主职责不是索引维护，不该因为一次查询失败而整体失败。
pub async fn refresh_team_agents(state: &AppState, team_id: &str) -> usize {
    refresh_open_spaces(&state.hub, &state.pool, team_id).await
}

/// 写接口手里只有 `ws_id`：先查库拿到团队，再按团队扇出。
pub async fn refresh_space_team(state: &AppState, ws_id: &str) -> usize {
    match workspaces::by_id(&state.pool, ws_id).await {
        Ok(Some(row)) => refresh_team_agents(state, &row.team_id).await,
        Ok(None) => {
            warn!(ws_id, "space row missing; skipping agent cache fanout");
            0
        }
        Err(e) => {
            warn!(ws_id, error = %e, "cannot read space row; skipping agent cache fanout");
            0
        }
    }
}

/// 不依赖 `AppState` 的扇出实现：文件监听只持连接池与 `Weak<WorkspaceHub>`，
/// 拿不到（也不该持有）整个 `AppState`。
pub(crate) async fn refresh_open_spaces(
    hub: &Arc<WorkspaceHub>,
    pool: &SqlitePool,
    team_id: &str,
) -> usize {
    let dirs = match workspaces::open_work_dirs_by_team(pool, team_id).await {
        Ok(dirs) => dirs,
        Err(e) => {
            warn!(team_id, error = %e, "cannot list open spaces; skipping agent cache fanout");
            return 0;
        }
    };
    if dirs.is_empty() {
        return 0;
    }

    let db_names = match agents::list_index_by_team(pool, team_id).await {
        Ok(rows) => rows.into_iter().map(|row| row.name).collect::<Vec<_>>(),
        Err(e) => {
            warn!(error = %e, "cannot list agent index rows; fanning out with disk names only");
            Vec::new()
        }
    };

    let mut refreshed = 0usize;
    let mut invalidated = 0usize;
    for dir in &dirs {
        // Hub 是唯一实例表：库里有行不代表实例还在（可能已关闭）。
        let Some(handle) = hub.get(Path::new(dir)) else {
            debug!(work_dir = %dir, "space not open; skipping cache fanout");
            continue;
        };
        let ws = handle.workspace();
        let manager = ws.agent_manager();

        // rescan 之前的 Tier-1 是「刚被删掉的 agent」唯一还留着名字的地方：
        // 磁盘上已无目录、库里已无行时，只有它能被失效，否则别的空间会
        // 继续从 Tier-2 交出已删除的实例。
        let mut names: BTreeSet<String> = manager.list_names().into_iter().collect();
        if let Err(e) = manager.rescan() {
            warn!(work_dir = %dir, error = %e, "agent rescan failed; invalidating known names only");
        }
        names.extend(manager.list_names());
        names.extend(db_names.iter().cloned());

        for name in &names {
            manager.invalidate(name);
        }
        invalidated += names.len();
        refreshed += 1;
    }

    if refreshed > 0 {
        info!(
            team_id,
            spaces = refreshed,
            invalidated,
            "agent caches fanned out"
        );
    }
    refreshed
}
