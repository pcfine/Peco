//! 启动对账（W3）：进程重启后所有 watcher 都消失了，只有台账知道哪些任务在飞。
//!
//! 只按台账点查目标空间的 Hub，不扫 Hub —— 扫描成本随累计任务数增长，而台账
//! 里非终态的行数上界是「未完成任务数」。
//!
//! 一行的三种结局：目标空间没恢复（或任务已不在 Hub 里）→ `orphan`；
//! 拿到终态快照 → `finish`；仍在跑 → 重新订阅。**仍在跑的任务绝不能 orphan**：
//! 它会继续跑并产生结果，标成孤儿等于对外谎报「结果已丢失」。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use model_provider::ToolCall;
use peco_core::agent::{LooperHook, hooks::DispatchWatchHook};
use peco_core::machine::{MachineError, TaskId};
use tracing::warn;
use uuid::Uuid;

use crate::db::dispatches::{self, DispatchRow};
use crate::space::report;
use crate::space::tasks::{result_digest, result_error, result_output, terminal_at};
use crate::state::AppState;

/// `run_machine_task` 的工具名：重建观察时把它当作「刚返回的结果」，让
/// [`DispatchWatchHook`] 自己解析并起 watcher —— 重订阅逻辑因此只有一份实现。
const RUN_MACHINE_TASK_TOOL_NAME: &str = "run_machine_task";

/// 对账结果计数；写日志由调用方做（`main` 与测试看同一份数字）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// 扫到的非终态台账行数。
    pub scanned: usize,
    /// 目标空间里拿到终态快照、已补 `finish` 的行数。
    pub finished: usize,
    /// 目标空间未恢复或任务已不在 Hub 里的行数。
    pub orphaned: usize,
    /// 仍在跑、已重新订阅观察的行数。
    pub resubscribed: usize,
}

/// 单行的结局。计数与副作用分开，`reconcile_ledger` 只负责累加。
enum Outcome {
    Finished,
    Orphaned,
    Resubscribed,
    /// 查不动（非 `NotFound` 的错误）：保持原状，不猜。
    Left,
}

pub async fn reconcile_ledger(state: &AppState) -> ReconcileReport {
    let rows = match dispatches::list_active(&state.pool).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "cannot list active ledger rows; skipping reconcile");
            return ReconcileReport::default();
        }
    };

    let mut report = ReconcileReport {
        scanned: rows.len(),
        ..ReconcileReport::default()
    };
    for row in &rows {
        match reconcile_row(state, row).await {
            Outcome::Finished => report.finished += 1,
            Outcome::Orphaned => report.orphaned += 1,
            Outcome::Resubscribed => report.resubscribed += 1,
            Outcome::Left => {}
        }
    }
    report
}

async fn reconcile_row(state: &AppState, row: &DispatchRow) -> Outcome {
    let dir = PathBuf::from(&row.target_work_dir);
    let Some(handle) = state.hub.get(&dir) else {
        return orphan(state, row, "target space is not open in this process").await;
    };
    // runtime 已被释放说明这台 machine 不会再跑：任务结果无从取回。
    let Some(runtime) = handle.machine_runtime().upgrade() else {
        return orphan(state, row, "target space has no machine runtime").await;
    };
    let task_id = match Uuid::parse_str(&row.task_id) {
        Ok(id) => TaskId(id),
        Err(e) => {
            warn!(task_id = %row.task_id, error = %e, "ledger task id is not a uuid; left as is");
            return Outcome::Left;
        }
    };
    let hub = match runtime.hub() {
        Ok(hub) => hub,
        Err(e) => {
            warn!(task_id = %row.task_id, error = %e, "target space hub is unavailable; left as is");
            return Outcome::Left;
        }
    };

    match hub.snapshot(&task_id).await {
        Ok(snapshot) if snapshot.state.is_terminal() => {
            let digest = result_digest(&snapshot);
            let finished = dispatches::finish(
                &state.pool,
                &row.task_id,
                snapshot.state.as_str(),
                digest.as_deref(),
                result_output(&snapshot),
                false,
            )
            .await;
            match finished {
                Ok(changed) => {
                    // 这条路径完全绕过 sink，报告必须在这里另写一次，否则重启期间
                    // 补终态的任务在空间目录里永远没有交付物。
                    if changed > 0 {
                        let finished_at = terminal_at(&snapshot);
                        report::write_for_row(
                            &state.pool,
                            row,
                            report::ReportBody::Terminal {
                                state: snapshot.state.as_str(),
                                terminal_at: Some(&finished_at),
                                // 正文、摘要与截断标志都由报告回读台账取得：快照里的
                                // 这份可能比台账旧，行里那份摘要也可能已被 `finish`
                                // 换掉。这里的值只是读不到行时的兜底。
                                output: result_output(&snapshot),
                                digest: digest.as_deref(),
                                output_truncated: false,
                                error: result_error(&snapshot),
                            },
                        )
                        .await;
                    }
                    Outcome::Finished
                }
                Err(e) => {
                    warn!(task_id = %row.task_id, error = %e, "ledger finish failed");
                    Outcome::Left
                }
            }
        }
        // 仍在跑：重新订阅，状态保持不变（任务没死，标 orphan 会骗人）。
        Ok(_) => {
            resubscribe(state, row, &dir);
            Outcome::Resubscribed
        }
        Err(MachineError::NotFound { .. }) => {
            orphan(state, row, "target hub does not know this task").await
        }
        Err(e) => {
            warn!(task_id = %row.task_id, error = %e, "task snapshot failed; left as is");
            Outcome::Left
        }
    }
}

async fn orphan(state: &AppState, row: &DispatchRow, reason: &str) -> Outcome {
    // 重启对账的孤儿行保留 `wake_state`，`awaiting` 的那一批转成 `pending`：它们
    // 从没被通知过，正是要告诉 Peco「你派出去的那条断了」的那一类。
    match dispatches::orphan_by_task_on_restart(&state.pool, &row.task_id).await {
        Ok(changed) => {
            warn!(
                task_id = %row.task_id,
                target_ws_id = %row.target_ws_id,
                reason,
                "dispatch ledger row marked orphan on startup"
            );
            // 与运行期失联同理：重启对账也得在目标空间留下失联说明，否则那些任务
            // 在文件页签里从未存在过。
            if changed > 0 {
                report::write_for_row(
                    &state.pool,
                    row,
                    report::ReportBody::Orphan {
                        reason,
                        terminal_at: None,
                    },
                )
                .await;
            }
            Outcome::Orphaned
        }
        Err(e) => {
            warn!(task_id = %row.task_id, error = %e, "ledger orphan update failed");
            Outcome::Left
        }
    }
}

/// 重新订阅：把台账行拼成一条 `run_machine_task` 结果，交给
/// [`DispatchWatchHook`] —— 于是订阅、`Lagged` 重订阅、终态补快照、失联判定
/// 全部复用同一份实现，daemon 侧不复制一遍。
///
/// 该条观察的 `origin_kind` 是 `run_machine_task`，但它不会改写既有行的
/// `origin_kind`：`on_registered` 的插入是 `ON CONFLICT ... DO NOTHING`，
/// 而行必然已存在（是在飞台账里扫出来的）。
fn resubscribe(state: &AppState, row: &DispatchRow, dir: &Path) {
    let payload = serde_json::json!({
        "results": [{
            "agent_name": row.agent_name,
            "task_id": row.task_id,
            "terminal": false,
        }]
    })
    .to_string();
    // principal 取目标团队：台账没有「发起方主体」列，而 sink 只读 `origin.ws_id`。
    let hook: Arc<DispatchWatchHook> = state.env.dispatch_hook(
        row.origin_ws_id.clone(),
        row.target_team_id.clone(),
        dir.to_path_buf(),
        row.origin_conversation_id.clone(),
    );
    let call = ToolCall::new(
        format!("reconcile-{}", row.task_id),
        RUN_MACHINE_TASK_TOOL_NAME,
        "{}",
    );
    // hook 自身立即返回，watcher 由它 spawn；这里没有 turn 可依附，故单独起一个
    // 任务调用它，失败只可能是解析路径（已在 hook 内 warn，不影响后续行）。
    tokio::spawn(async move {
        hook.on_after_tool(0, &call, &payload, false).await;
    });
}
