//! 空间关闭（`stop`）。删除与回收站在 [`super::trash`]。
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
use crate::space::report::{self, ReportBody};

/// force 关闭时写进失联说明的原因：这是人主动做的决定，不是设备/网络故障。
const FORCE_CLOSE_REASON: &str = "空间被强制关闭";

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

/// 门槛：先统计，不通过就 409 且不动实例表；`force` 时把在飞台账置 `orphan`。
pub(crate) async fn pass_gate(
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
    // 先取在飞行再改状态：`mark_orphan` 之后这批行就离开 `TASK_ACTIVE_STATES`，
    // 再也读不回来了。
    let doomed = dispatches::list_active_by_target(&env.pool, ws_id).await?;
    let orphaned = dispatches::mark_orphan(&env.pool, ws_id).await?;
    tracing::warn!(
        ws_id,
        orphaned,
        dispatch = inflight.dispatch,
        foreign = inflight.foreign,
        "inflight ledger rows marked orphan"
    );
    // 这些行已不在在飞状态，重启对账永不重扫它们 —— 空间目录里的失联说明是它们
    // 唯一可能留下的交付物。台账没被改写就不写（报告会与台账打架）；写盘失败由
    // `write_for_row` 自己 warn，空间目录可能已被清理，属预期。
    if orphaned > 0 {
        for row in &doomed {
            report::write_for_row(
                &env.pool,
                row,
                ReportBody::Orphan {
                    reason: FORCE_CLOSE_REASON,
                    // 失联时刻不可考：报告按落盘时间估计并自行标注。
                    terminal_at: None,
                },
            )
            .await;
        }
    }
    Ok((inflight, orphaned))
}

/// 已打开实例的判定口径：路径必须存在且 Hub 里可见。
pub fn is_open(hub: &WorkspaceHub, row: &workspaces::WorkspaceRow) -> bool {
    hub.is_open(Path::new(&row.work_dir))
}
