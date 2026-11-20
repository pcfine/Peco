//! 停机原语与停机序列。
//!
//! 顺序不可换：SSE 断流必须早于停泵，否则泵的最后一帧会写到一个已经关掉的流上。
//! 这一顺序由调用时机保证 —— [`drain`] 只在 `http::serve` 返回之后被调用。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use model_provider::{Content, InputItem};
use peco_core::session::{AnnotatedMessage, MessageSource, SessionPersister};
use sqlx::SqlitePool;
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::db::{conversations, workspaces};
use crate::session_store::SqliteSessionPersister;
use crate::space::close;
use crate::state::AppState;

/// 等 chat 运行注册表排空的上限。
/// 必须长于 [`crate::chat::run::SHUTDOWN_SETTLE`]，否则序列先放弃这一轮，快照就落不下来。
pub(crate) const DRAIN_RUNS: Duration = Duration::from_secs(5);
/// 整个停机序列的硬预算。
const DRAIN_TOTAL: Duration = Duration::from_secs(8);
/// 关连接池的上限。
const DRAIN_POOL: Duration = Duration::from_secs(2);
/// 轮询注册表的间隔。
const DRAIN_POLL: Duration = Duration::from_millis(20);

/// 停机写进台账与排期行的成因串（机器可读，与既有行值同一套下划线拼写）。
const LEDGER_INTERRUPT_REASON: &str = "service_shutdown";

/// 停机写进 chat 历史的中断措辞（人类可读）。
///
/// core 的取消路径只会写「cancelled by user」，而停机不是用户停的：说法由 daemon
/// 收尾之后改写，core 不必知道停机这回事。
const HISTORY_INTERRUPT_REASON: &str = "service shutdown";

/// core 取消路径写进正文的那句旧措辞。
const CANCELLED_LABEL: &str = "cancelled by user";

/// 停机闩：`true` 表示「别再开新活了」。
///
/// `watch` 而不是 `AtomicBool`：既要能同步判定（handler 快速拒绝），又要能被 `select!` 等待。
#[derive(Clone)]
pub struct Shutdown {
    tx: Arc<watch::Sender<bool>>,
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}

impl Shutdown {
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self { tx: Arc::new(tx) }
    }

    pub fn cancel(&self) {
        self.tx.send_replace(true);
    }

    /// 同步判定：handler 用它当场拒绝新活，不必等一个 future。
    pub fn is_cancelled(&self) -> bool {
        *self.tx.borrow()
    }

    /// 供 `select!` 使用；已置位时立即返回。
    pub async fn cancelled(&self) {
        let mut rx = self.tx.subscribe();
        // 发送端由 `self` 持有，等不到 Err；真拿到 Err 也只能说明这里已无人等。
        let _ = rx.wait_for(|cancelled| *cancelled).await;
    }
}

/// 停机序列。调用前提：HTTP 服务已返回（SSE 全部结束）、闩已置位。
///
/// 超预算直接退出进程：否则只能等 systemd 的 `TimeoutStopSec` 送 SIGKILL。
pub async fn drain(state: &AppState) {
    // 幂等：信号闭包已经置过，测试与其它入口也要能直接驱动整条序列。
    state.shutdown.cancel();
    let started = std::time::Instant::now();
    match tokio::time::timeout(DRAIN_TOTAL, drain_steps(state)).await {
        Ok(()) => info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "shutdown drain complete"
        ),
        Err(_) => {
            error!(
                budget_secs = DRAIN_TOTAL.as_secs(),
                open_sessions = state.chat_runs.active_count(),
                "shutdown drain did not converge; exiting anyway"
            );
            std::process::exit(0);
        }
    }
}

async fn drain_steps(state: &AppState) {
    // 先停泵与调度：两者都会起新的一轮，走晚了会与下面的取消抢同一个会话。
    state.wake.stop().await;
    state.schedules.stop().await;

    let cancelled = state.chat_runs.cancel_all_for_shutdown();
    let open = wait_runs_drained(state, DRAIN_RUNS).await;
    if open > 0 {
        // 没收敛就不改写：此刻库里还是旧快照，写回去会盖掉这一轮该落的东西。
        warn!(
            open_sessions = open,
            cancelled = cancelled.len(),
            budget_secs = DRAIN_RUNS.as_secs(),
            "in-flight turns did not finish within the drain budget"
        );
    } else {
        info!(cancelled = cancelled.len(), "in-flight turns finished");
        relabel_interrupted_turns(state, &cancelled).await;
    }

    interrupt_schedule_runs(state).await;
    orphan_dispatches(state).await;
    close_open_spaces(state).await;
    close_pool(state).await;
}

/// 等注册表排空，返回仍未收敛的会话数。
async fn wait_runs_drained(state: &AppState, budget: Duration) -> usize {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let open = state.chat_runs.active_count();
        if open == 0 || tokio::time::Instant::now() >= deadline {
            return open;
        }
        tokio::time::sleep(DRAIN_POLL).await;
    }
}

async fn interrupt_schedule_runs(state: &AppState) {
    match crate::db::schedules::interrupt_running(&state.pool, LEDGER_INTERRUPT_REASON).await {
        Ok(0) => {}
        Ok(count) => info!(count, "schedule runs interrupted by the shutdown"),
        Err(e) => warn!(error = %e, "cannot mark the in-flight schedule runs as interrupted"),
    }
}

/// 在飞任务的台账行标 `orphan`：进程一走，这些结果就再也回捞不到。
async fn orphan_dispatches(state: &AppState) {
    match crate::db::dispatches::orphan_all_active(&state.pool).await {
        Ok(0) => {}
        Ok(count) => info!(count, "in-flight dispatches marked orphan by the shutdown"),
        Err(e) => warn!(error = %e, "cannot orphan the in-flight dispatch rows"),
    }
}

/// 把被停机取消的那些轮次的中断措辞改写成 [`HISTORY_INTERRUPT_REASON`]。
///
/// 只能在 run 全部结束之后跑：快照由 looper 在收尾时落盘，早一步 load 到的是旧快照。
async fn relabel_interrupted_turns(state: &AppState, conversation_ids: &[String]) {
    let persister = SqliteSessionPersister::new(state.pool.clone());
    for conversation_id in conversation_ids {
        match relabel_interrupted_turn(&state.pool, &persister, conversation_id).await {
            Ok(true) => info!(
                conversation_id = %conversation_id,
                "interrupted turn relabelled as service shutdown"
            ),
            Ok(false) => {}
            Err(e) => warn!(
                conversation_id = %conversation_id,
                error = %e,
                "cannot rewrite the interrupted turn reason"
            ),
        }
    }
}

/// 正文里的旧措辞一并改：只动纯文本，图文混排原样保留。
fn retitle_interrupted_text(message: &mut AnnotatedMessage) {
    let InputItem::Message {
        role,
        content: Content::Text(text),
    } = &*message.message
    else {
        return;
    };
    if !text.contains(CANCELLED_LABEL) {
        return;
    }
    message.message = Arc::new(InputItem::Message {
        role: *role,
        content: Content::Text(text.replace(CANCELLED_LABEL, HISTORY_INTERRUPT_REASON)),
    });
}

/// 改写只动最后一轮的成因串，轮次结构与 `turn_index` 不变 —— 在途检查点是否陈旧
/// 全靠后者判定。
async fn relabel_interrupted_turn(
    pool: &SqlitePool,
    persister: &SqliteSessionPersister,
    conversation_id: &str,
) -> Result<bool, String> {
    let Some((mut snapshot, meta)) = persister
        .load(conversation_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(false);
    };
    let Some(turn) = snapshot.committed_turns.last_mut() else {
        return Ok(false);
    };

    let mut changed = false;
    for message in turn.iter_mut() {
        if let MessageSource::InterruptedTurn { reason } = &mut message.source {
            *reason = HISTORY_INTERRUPT_REASON.to_string();
            retitle_interrupted_text(message);
            changed = true;
        }
    }
    if !changed {
        return Ok(false);
    }

    // `save` 整行覆盖描述与创建时间，故两者都得按原值带上：描述取会话标题，
    // 创建时间取快照行里已有的值。
    let description = conversations::find_by_id(pool, conversation_id)
        .await
        .map_err(|e| e.to_string())?
        .map_or(meta.description, |row| row.title);
    persister
        .save(&snapshot, conversation_id, &description, meta.created_at)
        .await
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// 关掉已打开的空间：实例析构会收尾文件监听与知识库句柄，比进程被信号打死干净。
///
/// 清单只能自己枚举（`workspaces` 表 + `hub.is_open`）：core 没有「哪些空间开着」的查询。
async fn close_open_spaces(state: &AppState) {
    let rows = match workspaces::list(&state.pool).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "cannot enumerate the spaces to close");
            return;
        }
    };
    let mut closed = 0usize;
    for row in &rows {
        if !close::is_open(&state.hub, row) {
            continue;
        }
        let dir = Path::new(&row.work_dir);
        if let Err(e) = state.hub.begin_close(dir).await {
            warn!(dir = %dir.display(), error = %e, "cannot close the space during shutdown");
            continue;
        }
        state.hub.finish_close(dir).await;
        closed += 1;
    }
    if closed > 0 {
        info!(count = closed, "open spaces closed");
    }
}

async fn close_pool(state: &AppState) {
    if tokio::time::timeout(DRAIN_POOL, state.pool.close())
        .await
        .is_err()
    {
        warn!(
            budget_secs = DRAIN_POOL.as_secs(),
            "sqlite pool did not close in time"
        );
    }
}

#[cfg(test)]
mod tests {
    use model_provider::{Content, InputItem, Role};
    use peco_core::session::{AnnotatedMessage, MessageId, SessionSnapshot};

    use super::*;

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        (pool, dir)
    }

    fn message(
        turn_index: usize,
        role: Role,
        text: &str,
        source: MessageSource,
    ) -> AnnotatedMessage {
        AnnotatedMessage::new(
            MessageId(turn_index as u64),
            turn_index,
            InputItem::Message {
                role,
                content: Content::Text(text.to_string()),
            },
            source,
        )
    }

    fn interrupted(reason: &str) -> MessageSource {
        MessageSource::InterruptedTurn {
            reason: reason.to_string(),
        }
    }

    /// 两轮历史：第一轮正常跑完，第二轮被取消（core 写的是「cancelled by user」）。
    fn snapshot_with_a_cancelled_last_turn() -> SessionSnapshot {
        SessionSnapshot {
            committed_turns: vec![
                vec![
                    message(0, Role::User, "先问一句", MessageSource::UserInput),
                    message(0, Role::Assistant, "答完了", MessageSource::ModelGeneration),
                ],
                vec![
                    message(1, Role::User, "跑个长任务", MessageSource::UserInput),
                    message(
                        1,
                        Role::Assistant,
                        "[interrupted] this turn was interrupted before completion: cancelled by user",
                        interrupted("cancelled by user"),
                    ),
                ],
            ],
            turn_index: 2,
            ..snapshot_without_turns()
        }
    }

    fn snapshot_without_turns() -> SessionSnapshot {
        SessionSnapshot {
            committed_turns: Vec::new(),
            turn_index: 0,
            total_usage: Default::default(),
            next_message_id: 0,
            pending_inputs: Vec::new(),
            pinned_summary: None,
        }
    }

    #[tokio::test]
    async fn cancel_is_idempotent_and_visible_to_both_queries() {
        let shutdown = Shutdown::new();
        assert!(!shutdown.is_cancelled());

        shutdown.cancel();
        shutdown.cancel();
        assert!(shutdown.is_cancelled());

        // 置位之后才来等的一方必须立即返回，不能等下一次状态变化。
        tokio::time::timeout(Duration::from_millis(100), shutdown.cancelled())
            .await
            .expect("已置位后 cancelled() 必须立即返回");
    }

    #[tokio::test]
    async fn waiting_cancelled_resolves_when_the_signal_arrives() {
        let shutdown = Shutdown::new();
        let waiter = shutdown.clone();
        let waiting = tokio::spawn(async move { waiter.cancelled().await });

        // 让等待方先挂上，再置位：覆盖「订阅早于信号」的那条路径。
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiting.is_finished(), "未置位时不得提前返回");

        shutdown.cancel();
        tokio::time::timeout(Duration::from_millis(500), waiting)
            .await
            .expect("置位后等待方必须被唤醒")
            .expect("等待任务不得 panic");
    }

    /// 克隆共享同一个闩：任何一方置位，其它持有者都能看见。
    #[tokio::test]
    async fn clones_share_the_same_latch() {
        let shutdown = Shutdown::new();
        let other = shutdown.clone();
        other.cancel();
        assert!(shutdown.is_cancelled());
    }

    /// 停机兜底必须落在排空预算之内：预算更短的话序列会先放弃这一轮，
    /// 快照就再也落不下来。
    #[test]
    fn the_settle_fallback_fits_inside_the_runs_budget() {
        assert!(
            crate::chat::run::SHUTDOWN_SETTLE < DRAIN_RUNS,
            "兜底 {:?} 不得长于排空预算 {DRAIN_RUNS:?}",
            crate::chat::run::SHUTDOWN_SETTLE
        );
    }

    /// 改写只碰最后一轮的中断成因串：轮数与 `turn_index` 不变（在途检查点靠它
    /// 判定陈旧），更早的轮次原样保留。
    ///
    /// 变异方法：把 `*reason = ...` 去掉 → 「必须报告改过」的断言变红；
    /// 改成遍历所有轮 → 「更早的轮次不归停机改写」的断言变红。
    #[tokio::test]
    async fn relabel_rewrites_only_the_last_turn_and_keeps_the_structure() {
        let (pool, _dir) = test_pool().await;
        let persister = SqliteSessionPersister::new(pool.clone());
        crate::db::conversations::insert(
            &pool,
            &crate::db::conversations::CreateConversationParams {
                id: "conv-1".to_string(),
                workspace_id: "ws-1".to_string(),
                agent_id: None,
                agent_name: "master".to_string(),
                title: "长任务会话".to_string(),
            },
        )
        .await
        .unwrap();

        let mut snapshot = snapshot_with_a_cancelled_last_turn();
        // 第一轮也塞一条中断标记，用来证明改写不会越界到更早的轮次。
        snapshot.committed_turns[0].push(message(
            0,
            Role::Assistant,
            "早先的一次中断",
            interrupted("cancelled by user"),
        ));
        persister
            .save(&snapshot, "conv-1", "旧描述", 1_700_000_000)
            .await
            .unwrap();

        assert!(
            relabel_interrupted_turn(&pool, &persister, "conv-1")
                .await
                .unwrap(),
            "有可改写的成因串时必须报告改过"
        );

        let (reloaded, meta) = persister.load("conv-1").await.unwrap().unwrap();
        assert_eq!(reloaded.committed_turns.len(), 2, "轮数不得变");
        assert_eq!(reloaded.turn_index, 2, "轮次编号不得变");
        assert_eq!(
            reloaded.committed_turns[0].last().unwrap().source,
            interrupted("cancelled by user"),
            "更早的轮次不归停机改写"
        );
        let last_turn = reloaded.committed_turns[1].as_slice();
        assert_eq!(last_turn.len(), 2, "最后一轮的消息条数不得变");
        assert_eq!(
            last_turn.last().unwrap().source,
            interrupted(HISTORY_INTERRUPT_REASON),
            "历史里读到的必须是停机，而不是用户取消"
        );
        // 正文是用户直接读到的那句：成因串改了而正文没改，看起来仍像用户自己停的。
        let text = match &*last_turn.last().unwrap().message {
            InputItem::Message {
                content: Content::Text(text),
                ..
            } => text.clone(),
            other => panic!("夹具必须是纯文本消息，实际 {other:?}"),
        };
        assert!(
            text.contains(HISTORY_INTERRUPT_REASON) && !text.contains(CANCELLED_LABEL),
            "正文措辞必须一并改写，实际 {text:?}"
        );
        // 描述取会话标题、创建时间取原快照行 —— `save` 会整行覆盖这两列。
        assert_eq!(meta.description, "长任务会话");
        assert_eq!(meta.created_at, 1_700_000_000);
    }

    /// 没有快照（会话没落过盘）或最后一轮压根没中断：不动库、不报错。
    #[tokio::test]
    async fn relabel_is_a_no_op_without_an_interrupted_turn() {
        let (pool, _dir) = test_pool().await;
        let persister = SqliteSessionPersister::new(pool.clone());

        assert!(
            !relabel_interrupted_turn(&pool, &persister, "conv-absent")
                .await
                .unwrap(),
            "没有快照的会话不必改写"
        );

        persister
            .save(
                &snapshot_without_turns(),
                "conv-empty",
                "空会话",
                1_700_000_001,
            )
            .await
            .unwrap();
        assert!(
            !relabel_interrupted_turn(&pool, &persister, "conv-empty")
                .await
                .unwrap(),
            "没有轮次就没有可改写的成因串"
        );
        let (_, meta) = persister.load("conv-empty").await.unwrap().unwrap();
        assert_eq!(meta.description, "空会话", "无改动时不得重写整行");
    }
}
