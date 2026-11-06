//! 空间关闭与删除（`stop` / `purge`）。
//!
//! 两者共用同一道门槛：drop `WorkSpace` 会连带销毁唯一持有任务结果的 Hub，
//! 未终态台账必须先落 `orphan`，否则重启对账无从判断任务去哪了。
//!
//! 实例表是进程级 `WorkspaceHub`：`begin_close` 让 `get/ensure_open` 立刻看不到
//! 实例，`finish_close` 才真正 drop。`finish_close` 必须在 `begin_close` 之后
//! 无条件执行，否则该目录会永久停在 Busy。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use peco_core::tools::WorkspaceHub;

use crate::db::dispatches::{self, InflightCounts};
use crate::db::workspaces;
use crate::error::DaemonError;
use crate::space::SpaceEnv;
use crate::space::open::map_tool_error;

/// 关闭/删除的结果。
#[derive(Debug, Clone, Copy)]
pub struct CloseOutcome {
    /// 门槛统计（关闭动作发生后这些行可能已变 `orphan`）。
    pub inflight: InflightCounts,
    /// 本次被置 `orphan` 的台账行数。
    pub orphaned: u64,
    /// 是否删掉了空间行（`purge` 为真，`stop` 为假）。
    pub purged: bool,
}

/// 关闭空间实例但保留行与目录；`closed_at` 记录关闭时刻。
pub async fn close_workspace(
    env: &SpaceEnv,
    hub: &Arc<WorkspaceHub>,
    ws_id: &str,
    force: bool,
) -> Result<CloseOutcome, DaemonError> {
    let row =
        workspaces::by_id(&env.pool, ws_id)
            .await?
            .ok_or_else(|| DaemonError::SpaceNotFound {
                ws_id: ws_id.to_string(),
            })?;

    let (inflight, orphaned) = pass_gate(env, ws_id, force).await?;

    let dir = PathBuf::from(&row.work_dir);
    hub.begin_close(&dir)
        .await
        .map_err(|e| map_tool_error(e, ws_id))?;

    let result = workspaces::set_closed_at(&env.pool, ws_id)
        .await
        .map_err(DaemonError::from);
    hub.finish_close(&dir).await;
    result?;

    Ok(CloseOutcome {
        inflight,
        orphaned,
        purged: false,
    })
}

/// 删除空间行（台账保留）。
pub async fn purge_workspace(
    env: &SpaceEnv,
    hub: &Arc<WorkspaceHub>,
    ws_id: &str,
    force: bool,
) -> Result<CloseOutcome, DaemonError> {
    // Peco 空间是 Peco 会话的宿主：删掉行等于删掉主管，只能关闭不能删除。
    // 判定先于查行：保留 id 无论索引在不在都不该被删。
    if env.peco.is_peco_space(ws_id) {
        return Err(DaemonError::ReservedSpace {
            ws_id: ws_id.to_string(),
        });
    }

    let row =
        workspaces::by_id(&env.pool, ws_id)
            .await?
            .ok_or_else(|| DaemonError::SpaceNotFound {
                ws_id: ws_id.to_string(),
            })?;

    let (inflight, orphaned) = pass_gate(env, ws_id, force).await?;

    let dir = PathBuf::from(&row.work_dir);
    hub.begin_close(&dir)
        .await
        .map_err(|e| map_tool_error(e, ws_id))?;

    let result = workspaces::delete(&env.pool, ws_id)
        .await
        .map(|_| ())
        .map_err(DaemonError::from);
    hub.finish_close(&dir).await;
    result?;

    Ok(CloseOutcome {
        inflight,
        orphaned,
        purged: true,
    })
}

/// 门槛：先统计，不通过就 409 且不动实例表；`force` 时把在飞台账置 `orphan`。
async fn pass_gate(
    env: &SpaceEnv,
    ws_id: &str,
    force: bool,
) -> Result<(InflightCounts, u64), DaemonError> {
    let inflight = dispatches::inflight_counts(&env.pool, ws_id, &env.peco.ws_id).await?;
    if inflight.total() == 0 {
        return Ok((inflight, 0));
    }
    if !force {
        return Err(DaemonError::InflightTasks {
            ws_id: ws_id.to_string(),
            inflight_dispatch: inflight.dispatch,
            inflight_foreign: inflight.foreign,
        });
    }
    let orphaned = dispatches::mark_orphan(&env.pool, ws_id).await?;
    tracing::warn!(
        ws_id,
        orphaned,
        dispatch = inflight.dispatch,
        foreign = inflight.foreign,
        "inflight ledger rows marked orphan"
    );
    Ok((inflight, orphaned))
}

/// 已打开实例的判定口径：路径必须存在且 Hub 里可见。
pub fn is_open(hub: &WorkspaceHub, row: &workspaces::WorkspaceRow) -> bool {
    hub.is_open(Path::new(&row.work_dir))
}
