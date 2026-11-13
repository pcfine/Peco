//! 跨空间任务的观察落点：`TaskObservationSink` 的 SQLite 实现。
//!
//! `DispatchWatchHook` 在派发成功后订阅目标空间的任务流，把注册 / 状态 /
//! 终态 / 失联翻译成本模块的四个回调。本模块只做两件事：写 `d_dispatches`
//! 台账，并发一条进程内广播事件（供后续 SSE 使用）。
//!
//! 注册那一刻按 [`wake::qualifies`] 决定这行要不要进唤醒队列（进则停在
//! `awaiting`，终态落库时才转 `pending`）：来源判据只有那一份，这里不再写一遍名单。

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use peco_core::agent::hooks::{DispatchObservation, MAX_DIGEST_CHARS, TaskObservationSink};
use peco_core::machine::{ObsEvent, TaskSnapshot, UpdateBody};
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::db::dispatch_state::{WakeSkipReason, WakeState};
use crate::db::dispatches::{self, NewDispatch};
use crate::db::workspaces;
use crate::space::peco::PecoIdentity;
use crate::space::report;
use crate::wake;
use crate::wake::pump::WakePump;

/// 广播缓冲：慢消费者只会收到 `Lagged`，不会阻塞 watcher。
const EVENT_BUFFER: usize = 256;

/// 台账变化通知（供后续 SSE 层消费）。
#[derive(Clone, Debug, Serialize)]
pub struct TaskEvent {
    /// `registered` / `state` / `terminal` / `orphaned` / `wake_turn_done`。
    pub kind: &'static str,
    /// `wake_turn_done` 是空串：那类事件没有台账行。
    pub task_id: String,
    /// 仅在 `registered` 时能直接确定；其余事件从台账查询。
    pub target_ws_id: Option<String>,
    pub state: String,
    pub dispatch_id: Option<String>,
    /// 只有 `wake_turn_done` 有值：它说的是「某个会话里多了一条消息」，
    /// 订阅方据此判定该刷新消息，不去查台账。
    pub conversation_id: Option<String>,
}

/// `d_dispatches` 的写入端 + 进程内广播。
pub struct TaskObserver {
    pool: SqlitePool,
    events: broadcast::Sender<TaskEvent>,
    /// 判据要区分「Peco 的待办」与「别的空间的自有任务」，故这里必须知道
    /// Peco 是谁；晚绑定的理由见 [`TaskObserver::attach_peco`]。
    peco: OnceLock<Arc<PecoIdentity>>,
    /// 终态落地后要有人去投递：唤醒泵是唯一入口，观察者只负责叫醒它。
    wake: Arc<WakePump>,
}

impl TaskObserver {
    pub fn new(pool: SqlitePool, wake: Arc<WakePump>) -> Self {
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        Self {
            pool,
            events,
            peco: OnceLock::new(),
            wake,
        }
    }

    /// 注入 Peco 身份。晚绑定是构造顺序决定的：观察者先于
    /// [`SpaceEnv`](crate::space::SpaceEnv) 建出（它是那边的依赖），两份值只在
    /// 那个装配点同时在场。
    ///
    /// 同一个身份被注入多次是装配常态（测试夹具与 `SpaceEnv` 各来一次），静默；
    /// 只有换了一个 `ws_id` 才说明装配点拿错了身份，那才 warn —— 先到的那份才是
    /// 本进程真正在用的。
    pub fn attach_peco(&self, peco: Arc<PecoIdentity>) {
        let Err(repeated) = self.peco.set(peco) else {
            return;
        };
        let kept = self
            .peco
            .get()
            .map(|kept| kept.ws_id.clone())
            .unwrap_or_default();
        if kept != repeated.ws_id {
            warn!(
                kept = %kept,
                rejected = %repeated.ws_id,
                "task observer already has another peco identity; keeping the first one"
            );
        }
    }

    /// 这一行的唤醒态与（跳过时的）成因；身份未注入时一律不唤醒 —— 宁可少一条
    /// 通知，也不能把别的空间的自有任务当成 Peco 的待办叫醒它。
    ///
    /// 两种跳过来自完全不同的故障：来源不合规是正常语义，身份缺失是装配异常，
    /// 台账上必须分得开。
    fn wake_disposition(&self, obs: &DispatchObservation) -> (WakeState, Option<WakeSkipReason>) {
        let Some(peco) = self.peco.get() else {
            warn!(
                task_id = %obs.task_id,
                "task observer has no peco identity; dispatch ledger row will not wake"
            );
            return (
                WakeState::Skipped,
                Some(WakeSkipReason::PecoIdentityMissing),
            );
        };
        if wake::qualifies(&obs.origin.ws_id, &obs.origin_kind, peco) {
            // 注册时任务还没跑完：此刻没有可投的内容，终态落库时才入队。
            (WakeState::Awaiting, None)
        } else {
            (WakeState::Skipped, Some(WakeSkipReason::NotPecoOrigin))
        }
    }

    /// 唤醒轮跑完了：让前端知道 `conversation_id` 里多了一条新消息。
    ///
    /// 与台账无关 —— 唤醒轮本身不产生台账行，故 `task_id` 留空串。订阅方据
    /// `conversation_id` 判定「该刷新消息」，不拿它去查台账。
    pub fn notify_wake_turn(&self, ws_id: &str, conversation_id: &str) {
        self.emit(TaskEvent {
            kind: "wake_turn_done",
            task_id: String::new(),
            target_ws_id: Some(ws_id.to_string()),
            state: String::new(),
            dispatch_id: None,
            conversation_id: Some(conversation_id.to_string()),
        });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        self.events.subscribe()
    }

    fn emit(&self, event: TaskEvent) {
        // 没有订阅者时 `send` 返回 Err：广播是可选能力，不影响落库。
        let _ = self.events.send(event);
    }
}

#[async_trait]
impl TaskObservationSink for TaskObserver {
    async fn on_registered(&self, obs: &DispatchObservation, snapshot: &TaskSnapshot) {
        let target_dir = obs.target_dir.to_string_lossy().into_owned();
        let row = match workspaces::by_work_dir(&self.pool, &target_dir).await {
            Ok(row) => row,
            Err(e) => {
                warn!(task_id = %obs.task_id, error = %e, "dispatch ledger lookup failed");
                return;
            }
        };
        let Some(row) = row else {
            warn!(
                task_id = %obs.task_id,
                target_dir,
                "dispatch target has no workspace row; ledger insert skipped"
            );
            return;
        };

        let state = snapshot.state.as_str();
        let id = format!("d_{}", obs.task_id);
        let (wake_state, wake_skip_reason) = self.wake_disposition(obs);
        let inserted = dispatches::insert(
            &self.pool,
            &NewDispatch {
                id: &id,
                task_id: &obs.task_id.to_string(),
                target_ws_id: &row.id,
                target_work_dir: &row.work_dir,
                target_team_id: &row.team_id,
                origin_ws_id: &obs.origin.ws_id,
                agent_name: &obs.agent_name,
                origin_kind: &obs.origin_kind,
                task_state: state,
                wake_state,
                wake_skip_reason,
                prompt_digest: obs.prompt_digest.as_deref(),
                dispatch_id: obs.dispatch_id.as_deref(),
            },
        )
        .await;
        match inserted {
            Ok(_) => self.emit(TaskEvent {
                kind: "registered",
                task_id: obs.task_id.to_string(),
                target_ws_id: Some(row.id),
                state: state.to_string(),
                dispatch_id: obs.dispatch_id.clone(),
                conversation_id: None,
            }),
            Err(e) => warn!(task_id = %obs.task_id, error = %e, "dispatch ledger insert failed"),
        }
    }

    async fn on_event(&self, obs: &DispatchObservation, event: &ObsEvent) {
        // 只关心状态变化；Progress/Output/Artifact 没有对应列，A+B 阶段不落库。
        let ObsEvent::Update(update) = event else {
            return;
        };
        let UpdateBody::Status { state, .. } = &update.body else {
            return;
        };
        let state_text = state.as_str();
        match dispatches::set_task_state(&self.pool, &obs.task_id.to_string(), state_text).await {
            Ok(_) => self.emit(TaskEvent {
                kind: "state",
                task_id: obs.task_id.to_string(),
                target_ws_id: None,
                state: state_text.to_string(),
                dispatch_id: obs.dispatch_id.clone(),
                conversation_id: None,
            }),
            Err(e) => {
                warn!(task_id = %obs.task_id, error = %e, "dispatch ledger state update failed")
            }
        }
    }

    async fn on_terminal(&self, obs: &DispatchObservation, snapshot: &TaskSnapshot) {
        let state = snapshot.state.as_str();
        let digest = result_digest(snapshot);
        match dispatches::finish(
            &self.pool,
            &obs.task_id.to_string(),
            state,
            digest.as_deref(),
            result_output(snapshot),
            false,
        )
        .await
        {
            Ok(changed) => {
                self.emit(TaskEvent {
                    kind: "terminal",
                    task_id: obs.task_id.to_string(),
                    target_ws_id: None,
                    state: state.to_string(),
                    dispatch_id: obs.dispatch_id.clone(),
                    conversation_id: None,
                });
                // 无条件叫醒：这一行该不该投、能不能投，由判据与 `claim` 的条件
                // 更新决定；在这里先猜一遍等于多出第二份判据。重放是幂等的。
                self.wake.notify_one();
                // 为 0 说明这行早已终态或从未落库：前者结果已随空间消失，此刻补一份
                // completed 的报告只会与台账打架。
                if changed > 0 {
                    let finished_at = terminal_at(snapshot);
                    report::write_for_observation(
                        &self.pool,
                        obs,
                        report::ReportBody::Terminal {
                            state,
                            terminal_at: Some(&finished_at),
                            // 正文与摘要在报告里回读台账取得（`finish` 的 COALESCE
                            // 可能保住了上一份正文）；这里的快照只是读不到行时的兜底。
                            output: result_output(snapshot),
                            digest: digest.as_deref(),
                            output_truncated: false,
                            error: result_error(snapshot),
                        },
                    )
                    .await;
                } else {
                    debug!(
                        task_id = %obs.task_id,
                        state,
                        "dispatch ledger row is already terminal or missing; no report written"
                    );
                }
            }
            Err(e) => warn!(task_id = %obs.task_id, error = %e, "dispatch ledger finish failed"),
        }
    }

    async fn on_orphaned(&self, obs: &DispatchObservation, reason: &str) {
        match dispatches::orphan_by_task(&self.pool, &obs.task_id.to_string()).await {
            Ok(changed) => {
                warn!(task_id = %obs.task_id, reason, "dispatch observation orphaned");
                self.emit(TaskEvent {
                    kind: "orphaned",
                    task_id: obs.task_id.to_string(),
                    target_ws_id: None,
                    state: "orphan".to_string(),
                    dispatch_id: obs.dispatch_id.clone(),
                    conversation_id: None,
                });
                // 失联说明是这份任务的唯一交付物：没有它，空间目录里连「任务来过」
                // 都看不到。`orphan_by_task` 带状态谓词，故 `changed > 0` 就是「本次
                // 真的发生了迁移」；为 0 时报告已有内容，覆盖它等于抹掉终态结果。
                if changed > 0 {
                    report::write_for_observation(
                        &self.pool,
                        obs,
                        report::ReportBody::Orphan {
                            reason,
                            // 失联时刻不可考，报告按落盘时间估计并自行标注。
                            terminal_at: None,
                        },
                    )
                    .await;
                }
            }
            Err(e) => {
                warn!(task_id = %obs.task_id, error = %e, "dispatch ledger orphan update failed")
            }
        }
    }
}

/// 终态正文：`d_dispatches.output` 的来源，非文本载荷为 `None`。
///
/// 启动对账补 `finish` 时也用它：正文只该有一份取法，两处各写一遍会让「同一任务
/// 在两条路径上落库的正文」不一样。
pub(crate) fn result_output(snapshot: &TaskSnapshot) -> Option<&str> {
    let output = snapshot.outcome.as_ref()?.output.as_ref()?;
    let serde_json::Value::String(text) = &output.payload else {
        return None;
    };
    Some(text)
}

/// 终态出错原因：报告「错误」段的唯一来源。
pub(crate) fn result_error(snapshot: &TaskSnapshot) -> Option<&str> {
    snapshot
        .outcome
        .as_ref()?
        .error
        .as_ref()
        .map(|e| e.message.as_str())
}

/// 终态时刻。快照的 `updated_at` 就是最后一次状态变更的时刻：任务终态之后不再变，
/// 故两条补写路径给出的时间是同一个。
pub(crate) fn terminal_at(snapshot: &TaskSnapshot) -> String {
    snapshot
        .updated_at
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 终态输出摘要：正文前缀，截断到 [`MAX_DIGEST_CHARS`]。
///
/// 摘要留在台账里是给唤醒消息与老行用的；它永远不是全文的替代品。
pub(crate) fn result_digest(snapshot: &TaskSnapshot) -> Option<String> {
    result_output(snapshot).map(|text| text.chars().take(MAX_DIGEST_CHARS).collect())
}
