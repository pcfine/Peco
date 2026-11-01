use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::future::join_all;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinError;
use tracing::{debug, warn};
use uuid::Uuid;

use super::machine::{Machine, MachineError};
use super::model::{
    Artifact, OutputView, Progress, TaggedJson, Task, TaskError, TaskFilter, TaskId, TaskOutcome,
    TaskSnapshot,
};
use super::sink::{SinkError, TaskSink};
use super::state::{
    ObsEvent, OutputChunk, TaskState, Update, UpdateBody, UpdateOrigin, can_transition,
};
use super::store::{TaskStore, matches_filter};

#[derive(Clone, Debug)]
pub struct HubConfig {
    /// 广播缓冲。高频 `Output` 撑爆它是正常路径，压力由 `output_tail` 有界承担。
    pub update_buffer: usize,
    /// 只作用于 `progress`：可替代增量按时间窗抽样。
    pub emit_min_interval: Duration,
    pub output_tail_bytes: usize,
    pub artifact_max_bytes: u64,
    pub scan_interval: Duration,
    pub terminal_grace: Duration,
    pub cancel_grace: Duration,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            update_buffer: 64,
            emit_min_interval: Duration::from_millis(50),
            output_tail_bytes: 64 * 1024,
            artifact_max_bytes: 8 * 1024 * 1024,
            scan_interval: Duration::from_secs(1),
            terminal_grace: Duration::from_secs(5),
            cancel_grace: Duration::from_secs(2),
        }
    }
}

pub struct TaskHub {
    inner: Arc<HubInner>,
}

/// 观察协议的唯一实现点：`seq` 推进、快照权威副本、广播扇出都收口在这里，
/// 放进 `Machine` 就会每个 impl 一份。
pub(crate) struct HubInner {
    slots: Mutex<HashMap<TaskId, Arc<Slot>>>,
    /// 只装非终态任务：进终态即摘除，扫描代价即 O(活跃数)。
    active: Mutex<HashMap<TaskId, ActiveMeta>>,
    machines: Mutex<HashMap<String, Arc<dyn Machine>>>,
    config: HubConfig,
    store: Arc<dyn TaskStore>,
    store_tx: mpsc::UnboundedSender<(TaskId, TaskSnapshot)>,
}

struct ActiveMeta {
    kind: String,
    deadline: Option<DateTime<Utc>>,
    cancel_requested_at: Option<Instant>,
}

/// 终态不从 `slots` 摘除：一旦摘除，`snapshot()` 就只能回落到 Store，而回落会让新建
/// Hub 复活它从未接纳的任务。
struct Slot {
    inner: Mutex<SlotInner>,
}

struct SlotInner {
    snapshot: TaskSnapshot,
    /// 只服务 `progress` 节流（`Output` 不节流，不需要第二枚时戳）。
    progress_emitted_at: Instant,
    output_tail: VecDeque<u8>,
    /// `output_tail[0]` 对应的全局字节偏移。
    tail_start_offset: u64,
    /// 已写入的总字节数。
    tail_end_offset: u64,
    /// 终态后置 `None`：drop 掉 Sender 订阅者才收到 `End`。
    tx: Option<broadcast::Sender<Arc<Update>>>,
}

impl Slot {
    fn lock(&self) -> Result<MutexGuard<'_, SlotInner>, SinkError> {
        // 中毒说明有 panic 打破了临界区不变量，此后任何写入都不可信。
        self.inner.lock().map_err(|_| SinkError::Closed)
    }
}

impl SlotInner {
    /// 先改快照再广播，且在同一临界区内：`seq` 无空洞，观察者看到某 `seq` 时
    /// 快照已包含它。
    fn emit(&mut self, task_id: TaskId, origin: UpdateOrigin, body: UpdateBody) {
        let seq = self.snapshot.last_seq + 1;
        let at = Utc::now();
        self.snapshot.last_seq = seq;
        self.snapshot.updated_at = at;
        if let Some(tx) = &self.tx {
            // 无订阅者时 send 返回 Err，属正常路径。
            let _ = tx.send(Arc::new(Update {
                task_id,
                seq,
                at,
                origin,
                body,
            }));
        }
    }

    /// 在锁内取快照副本：Store 只记状态转移，故副本与转移点严格对齐。
    fn state_change(&self) -> Applied {
        Applied {
            store: Some(self.snapshot.clone()),
            terminal: self.snapshot.state.is_terminal(),
        }
    }
}

/// 一次写入的副作用：摘活跃表 + 落 Store 都在锁外完成。
struct Applied {
    /// 状态转移后的快照；`None` = 本次未改变状态，不落库。
    store: Option<TaskSnapshot>,
    terminal: bool,
}

impl Applied {
    const QUIET: Self = Self {
        store: None,
        terminal: false,
    };
}

impl TaskHub {
    pub fn new(config: HubConfig, store: Arc<dyn TaskStore>) -> Arc<TaskHub> {
        let (store_tx, store_rx) = mpsc::unbounded_channel();
        let inner = Arc::new(HubInner {
            slots: Mutex::new(HashMap::new()),
            active: Mutex::new(HashMap::new()),
            machines: Mutex::new(HashMap::new()),
            config,
            store: Arc::clone(&store),
            store_tx,
        });
        spawn_store_writer(store, store_rx);
        spawn_deadline_scanner(Arc::downgrade(&inner));
        Arc::new(TaskHub { inner })
    }

    pub fn register(&self, machine: Arc<dyn Machine>) -> Result<(), MachineError> {
        let kind = machine.descriptor().kind;
        let mut machines = self
            .inner
            .machines
            .lock()
            .map_err(|_| MachineError::Unavailable {
                reason: "machine registry lock poisoned".to_string(),
            })?;
        if machines.contains_key(&kind) {
            return Err(MachineError::Illegal {
                reason: format!("kind already registered: {kind}"),
            });
        }
        machines.insert(kind, machine);
        Ok(())
    }

    /// 四段语义：分配 id → 建槽 → 准入 → 上架并 spawn。全程不 await executor future，
    /// 故返回耗时与执行时长无关。
    pub async fn submit(&self, kind: &str, task: Task) -> Result<TaskId, MachineError> {
        let machine = self.machine(kind)?;
        let id = TaskId(Uuid::new_v4());
        let now = Utc::now();

        let mut task = task;
        task.id = id;
        task.kind = kind.to_string();

        let snapshot = TaskSnapshot {
            id,
            kind: task.kind.clone(),
            state: TaskState::Submitted,
            state_detail: None,
            progress: Progress {
                phase: None,
                current: None,
                total: None,
            },
            output: OutputView {
                tail: Vec::new(),
                start_offset: 0,
                end_offset: 0,
            },
            outcome: None,
            artifacts: Vec::new(),
            deadline: task.deadline,
            source: task.source.clone(),
            labels: task.labels.clone(),
            scope: task.context.scope.clone(),
            created_at: task.created_at,
            updated_at: now,
            last_seq: 0,
        };

        // broadcast 不接受 0 容量，配置越界时退到最小可用值而非 panic。
        let capacity = self.inner.config.update_buffer.max(1);
        let (tx, _) = broadcast::channel(capacity);
        let slot = Arc::new(Slot {
            inner: Mutex::new(SlotInner {
                snapshot: snapshot.clone(),
                progress_emitted_at: Instant::now(),
                output_tail: VecDeque::new(),
                tail_start_offset: 0,
                tail_end_offset: 0,
                tx: Some(tx),
            }),
        });
        let sink = TaskSink::new(Arc::downgrade(&self.inner), id);

        // 只建 future 不 poll：`Machine::submit` 借 `&self`，只能随 Arc 一起被 spawn，
        // 故这里把调用推迟到首次 poll，效果与「先建后 spawn」等价且不执行任何 impl 代码。
        let executor = async move { machine.submit(task, sink).await };

        let deadline = snapshot.deadline;
        {
            let mut slots = self.slots()?;
            slots.insert(id, slot);
        }
        {
            let mut active = self
                .inner
                .active
                .lock()
                .map_err(|_| MachineError::Unavailable {
                    reason: "active table lock poisoned".to_string(),
                })?;
            active.insert(
                id,
                ActiveMeta {
                    kind: kind.to_string(),
                    deadline,
                    cancel_requested_at: None,
                },
            );
        }
        if self.inner.store_tx.send((id, snapshot)).is_err() {
            warn!(task_id = %id, "store writer is gone; initial snapshot not persisted");
        }

        let hub = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            // 再套一层 spawn 才能观测 panic：正常返回与 JoinError 走同一条收敛路径。
            let joined = tokio::spawn(executor).await;
            if let Some(inner) = hub.upgrade() {
                inner.on_executor_done(id, joined).await;
            }
        });
        Ok(id)
    }

    /// 同步 fn：临界区内不得 await，克隆首帧快照与注册 receiver 必须在同一把锁内，
    /// 否则首帧 `last_seq` 与 receiver 起点之间会漏 seq。
    pub fn subscribe(&self, id: &TaskId) -> Result<ObsStream, MachineError> {
        let slot = self.inner.slot(id).map_err(machine_error)?;
        let inner = slot.lock().map_err(machine_error)?;
        Ok(ObsStream {
            first: Some(inner.snapshot.clone()),
            rx: inner.tx.as_ref().map(|tx| tx.subscribe()),
            ended: false,
        })
    }

    /// 终态槽常驻 `slots`，缺槽即「本 Hub 从未接纳过该 id」：回落到 Store 会让新建
    /// Hub 复活上一个进程/上一个 Hub 的任务，把「无句柄」谎报成有。
    pub async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
        let slot = self.inner.slot(id).map_err(machine_error)?;
        let inner = slot.lock().map_err(machine_error)?;
        Ok(inner.snapshot.clone())
    }

    /// 终态只认快照：有损通道的 `Lagged`/`End` 只用来触发重读，永不作为终态来源。
    pub async fn await_terminal(
        &self,
        id: &TaskId,
        timeout: Option<Duration>,
    ) -> Result<TaskSnapshot, MachineError> {
        let wait = async {
            let mut remaining = AWAIT_TERMINAL_ATTEMPTS;
            while remaining > 0 {
                let snapshot = self.snapshot(id).await?;
                if snapshot.state.is_terminal() {
                    return Ok(snapshot);
                }
                remaining -= 1;
                let mut stream = self.subscribe(id)?;
                while stream.next().await.is_some() {}
            }
            Err(MachineError::Unavailable {
                reason: format!(
                    "no terminal state for task {id} after {AWAIT_TERMINAL_ATTEMPTS} subscriptions"
                ),
            })
        };

        match timeout {
            Some(limit) => match tokio::time::timeout(limit, wait).await {
                Ok(result) => result,
                Err(_) => Err(MachineError::Unavailable {
                    reason: format!("timeout after {limit:?} waiting for task {id}"),
                }),
            },
            None => wait.await,
        }
    }

    /// 并发等待，返回顺序与 `ids` 严格一致；单任务的失败是快照，不连坐其它位置。
    /// 整体截止用同一起点等价表达，到点只把未完成者标成 `Unavailable`。
    pub async fn await_all(
        &self,
        ids: &[TaskId],
        timeout: Option<Duration>,
    ) -> Vec<Result<TaskSnapshot, MachineError>> {
        join_all(ids.iter().map(|id| self.await_terminal(id, timeout))).await
    }

    pub async fn cancel(&self, id: &TaskId) -> Result<(), MachineError> {
        // 已终态 ⇒ 幂等成功，且不产生 seq、不广播。
        let slot = self.inner.slot(id).map_err(machine_error)?;
        let terminal = {
            let inner = slot.lock().map_err(machine_error)?;
            inner.snapshot.state.is_terminal()
        };
        if terminal {
            return Ok(());
        }

        let kind = {
            let mut active = self
                .inner
                .active
                .lock()
                .map_err(|_| MachineError::Unavailable {
                    reason: "active table lock poisoned".to_string(),
                })?;
            match active.get_mut(id) {
                Some(meta) => {
                    // 只记首次请求：重复 cancel 不得重置宽限窗口，否则连续调用能把
                    // 兜底收敛无限推迟。
                    meta.cancel_requested_at.get_or_insert_with(Instant::now);
                    Some(meta.kind.clone())
                }
                // 非终态却已不在活跃表：转移正在进行，交给对方收敛即可。
                None => None,
            }
        };

        let Some(kind) = kind else { return Ok(()) };
        let machine = self.machine(&kind)?;
        let id = *id;
        // 不等待 impl：取消的生效边界属 impl，Hub 只在 grace 后兜底。
        tokio::spawn(async move {
            if let Err(err) = machine.cancel(&id).await {
                debug!(task_id = %id, error = %err, "machine cancel returned an error");
            }
        });
        Ok(())
    }

    /// 活跃槽 ∪ Store：活跃槽是权威，Store 可能还停在更早的状态，故先取活跃槽。
    pub async fn list(&self, filter: &TaskFilter) -> Result<Vec<TaskId>, MachineError> {
        let active_ids: Vec<TaskId> = {
            let active = self
                .inner
                .active
                .lock()
                .map_err(|_| MachineError::Unavailable {
                    reason: "active table lock poisoned".to_string(),
                })?;
            active.keys().copied().collect()
        };

        let mut matched: Vec<(DateTime<Utc>, TaskId)> = Vec::new();
        for id in active_ids {
            let snapshot = {
                let slot = self.inner.slot(&id).map_err(machine_error)?;
                let inner = slot.lock().map_err(machine_error)?;
                inner.snapshot.clone()
            };
            if matches_filter(&snapshot, filter) {
                matched.push((snapshot.created_at, id));
            }
        }
        matched.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.0.cmp(&b.1.0)));
        let mut ids: Vec<TaskId> = matched.into_iter().map(|(_, id)| id).collect();

        let stored = self
            .inner
            .store
            .list(filter)
            .map_err(|err| MachineError::Unavailable {
                reason: err.to_string(),
            })?;
        for id in stored {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    fn slots(&self) -> Result<MutexGuard<'_, HashMap<TaskId, Arc<Slot>>>, MachineError> {
        self.inner
            .slots
            .lock()
            .map_err(|_| MachineError::Unavailable {
                reason: "slot table lock poisoned".to_string(),
            })
    }

    fn machine(&self, kind: &str) -> Result<Arc<dyn Machine>, MachineError> {
        let machines = self
            .inner
            .machines
            .lock()
            .map_err(|_| MachineError::Unavailable {
                reason: "machine registry lock poisoned".to_string(),
            })?;
        machines
            .get(kind)
            .cloned()
            .ok_or_else(|| MachineError::UnknownKind {
                kind: kind.to_string(),
            })
    }
}

impl HubInner {
    /// 取 slot 时分两把锁（先表后槽），从不嵌套 —— 与摘除活跃表、扫描的取锁顺序一致。
    fn slot(&self, id: &TaskId) -> Result<Arc<Slot>, SinkError> {
        let slots = self.slots.lock().map_err(|_| SinkError::Closed)?;
        slots
            .get(id)
            .cloned()
            .ok_or(SinkError::NotFound { id: *id })
    }

    /// 唯一写入口：所有状态/进度/输出/产物变更都在「锁槽 → 改快照 → 推进 seq → 广播」
    /// 的临界区内完成，锁外只做摘活跃表与投递 Store，避免 Store 延迟传染给 impl。
    fn apply<F>(&self, id: &TaskId, op: F) -> Result<(), SinkError>
    where
        F: FnOnce(&mut SlotInner) -> Result<Applied, SinkError>,
    {
        let slot = self.slot(id)?;
        let applied = {
            let mut inner = slot.lock()?;
            op(&mut inner)?
        };
        if applied.terminal {
            self.deactivate(id);
        }
        if let Some(snapshot) = applied.store
            && self.store_tx.send((snapshot.id, snapshot)).is_err()
        {
            warn!(task_id = %id, "store writer is gone; snapshot not persisted");
        }
        Ok(())
    }

    fn deactivate(&self, id: &TaskId) {
        match self.active.lock() {
            Ok(mut active) => {
                active.remove(id);
            }
            Err(_) => warn!(task_id = %id, "active table lock poisoned; task stays active"),
        }
    }

    pub(crate) fn begin(&self, id: &TaskId) -> Result<(), SinkError> {
        let task_id = *id;
        self.apply(id, move |inner| {
            let from = inner.snapshot.state;
            // 重入声明的是「已在推进」这一事实，不是推进一步：报错会逼每个 impl 自己记状态。
            if from == TaskState::Working {
                return Ok(Applied::QUIET);
            }
            if from != TaskState::Submitted {
                return Err(SinkError::IllegalTransition {
                    from,
                    to: TaskState::Working,
                });
            }
            inner.snapshot.state = TaskState::Working;
            let detail = inner.snapshot.state_detail.clone();
            inner.emit(
                task_id,
                UpdateOrigin::Machine,
                UpdateBody::Status {
                    state: TaskState::Working,
                    detail,
                },
            );
            Ok(inner.state_change())
        })
    }

    pub(crate) fn transition(
        &self,
        id: &TaskId,
        to: TaskState,
        detail: TaggedJson,
    ) -> Result<(), SinkError> {
        let task_id = *id;
        self.apply(id, move |inner| {
            let from = inner.snapshot.state;
            if !can_transition(from, to) {
                return Err(SinkError::IllegalTransition { from, to });
            }
            inner.snapshot.state = to;
            inner.snapshot.state_detail = Some(detail);
            let detail = inner.snapshot.state_detail.clone();
            inner.emit(
                task_id,
                UpdateOrigin::Machine,
                UpdateBody::Status { state: to, detail },
            );
            Ok(inner.state_change())
        })
    }

    pub(crate) fn resume(&self, id: &TaskId) -> Result<(), SinkError> {
        let task_id = *id;
        self.apply(id, move |inner| {
            let from = inner.snapshot.state;
            if !matches!(from, TaskState::InputRequired | TaskState::AuthRequired) {
                return Err(SinkError::IllegalTransition {
                    from,
                    to: TaskState::Working,
                });
            }
            inner.snapshot.state = TaskState::Working;
            let detail = inner.snapshot.state_detail.clone();
            inner.emit(
                task_id,
                UpdateOrigin::Machine,
                UpdateBody::Status {
                    state: TaskState::Working,
                    detail,
                },
            );
            Ok(inner.state_change())
        })
    }

    pub(crate) fn progress(&self, id: &TaskId, progress: Progress) -> Result<(), SinkError> {
        let task_id = *id;
        let min_interval = self.config.emit_min_interval;
        self.apply(id, move |inner| {
            let state = inner.snapshot.state;
            ensure_open(state, state)?;
            inner.snapshot.progress = progress.clone();
            inner.snapshot.updated_at = Utc::now();
            // 窗口内只更新现值：被丢掉的中间值不损信息，快照持最新值。
            if inner.progress_emitted_at.elapsed() < min_interval {
                return Ok(Applied::QUIET);
            }
            inner.progress_emitted_at = Instant::now();
            inner.emit(
                task_id,
                UpdateOrigin::Machine,
                UpdateBody::Progress(progress),
            );
            Ok(Applied::QUIET)
        })
    }

    pub(crate) fn output(&self, id: &TaskId, chunk: OutputChunk) -> Result<(), SinkError> {
        let task_id = *id;
        let limit = self.config.output_tail_bytes;
        self.apply(id, move |inner| {
            let state = inner.snapshot.state;
            ensure_open(state, state)?;
            // 偏移不连续意味着有字节永远读不到：此时既不写入也不占 seq，由调用方决定
            // 是补发还是放弃。
            if chunk.offset != inner.tail_end_offset {
                return Err(SinkError::OffsetGap {
                    expected: inner.tail_end_offset,
                    got: chunk.offset,
                });
            }
            inner.output_tail.extend(chunk.data.iter().copied());
            inner.tail_end_offset += chunk.data.len() as u64;
            let overflow = inner.output_tail.len().saturating_sub(limit);
            if overflow > 0 {
                inner.output_tail.drain(..overflow);
                inner.tail_start_offset += overflow as u64;
            }
            // 裁剪与刷新不可分：窗口起点一旦前移，快照里的 tail 必须同步缩到同一窗口。
            inner.snapshot.output = OutputView {
                tail: inner.output_tail.iter().copied().collect(),
                start_offset: inner.tail_start_offset,
                end_offset: inner.tail_end_offset,
            };
            inner.emit(task_id, UpdateOrigin::Machine, UpdateBody::Output(chunk));
            Ok(Applied::QUIET)
        })
    }

    pub(crate) fn artifact(&self, id: &TaskId, artifact: Artifact) -> Result<(), SinkError> {
        let task_id = *id;
        let limit = self.config.artifact_max_bytes;
        self.apply(id, move |inner| {
            let from = inner.snapshot.state;
            ensure_open(from, from)?;
            let existing = inner
                .snapshot
                .artifacts
                .iter()
                .position(|known| known.id == artifact.id);
            let merged = merge_artifact(
                existing.and_then(|index| inner.snapshot.artifacts.get(index)),
                &artifact,
                limit,
                from,
            )?;
            match existing {
                Some(index) => inner.snapshot.artifacts[index] = merged,
                None => inner.snapshot.artifacts.push(merged),
            }
            inner.emit(
                task_id,
                UpdateOrigin::Machine,
                UpdateBody::Artifact(artifact),
            );
            Ok(Applied::QUIET)
        })
    }

    pub(crate) fn finish(
        &self,
        id: &TaskId,
        state: TaskState,
        outcome: TaskOutcome,
    ) -> Result<(), SinkError> {
        self.finish_with(id, state, outcome, UpdateOrigin::Machine)
    }

    /// impl 与 Hub 兜底共用同一入口：先到者获胜，后者被转移表吸收。
    fn finish_with(
        &self,
        id: &TaskId,
        state: TaskState,
        outcome: TaskOutcome,
        origin: UpdateOrigin,
    ) -> Result<(), SinkError> {
        let task_id = *id;
        self.apply(id, move |inner| {
            let from = inner.snapshot.state;
            if !state.is_terminal() || !can_transition(from, state) {
                return Err(SinkError::IllegalTransition { from, to: state });
            }
            inner.snapshot.state = state;
            inner.snapshot.outcome = Some(outcome);
            let detail = inner.snapshot.state_detail.clone();
            inner.emit(task_id, origin, UpdateBody::Status { state, detail });
            // 终态后不再有增量：drop sender 订阅者才读到 End。
            inner.tx = None;
            Ok(inner.state_change())
        })
    }

    /// executor future 结束（含 panic）是第四条收敛路径：已终态时转移表吸收，
    /// 非终态时补一个 `origin: Hub` 的 `Failed`。
    async fn on_executor_done(
        &self,
        id: TaskId,
        joined: Result<Result<(), MachineError>, JoinError>,
    ) {
        let (kind, message) = match joined {
            Ok(Ok(())) => (
                "abandoned".to_string(),
                "executor finished without a terminal state".to_string(),
            ),
            Ok(Err(MachineError::Unavailable { reason })) => ("unavailable".to_string(), reason),
            Ok(Err(err)) => ("impl_error".to_string(), err.to_string()),
            Err(join_error) => ("panic".to_string(), join_error.to_string()),
        };
        let outcome = TaskOutcome {
            output: None,
            error: Some(TaskError {
                kind,
                message,
                detail: None,
            }),
        };
        self.converge(&id, TaskState::Failed, outcome);
    }

    fn converge(&self, id: &TaskId, state: TaskState, outcome: TaskOutcome) {
        match self.finish_with(id, state, outcome, UpdateOrigin::Hub) {
            Ok(()) | Err(SinkError::IllegalTransition { .. }) => {}
            Err(err) => warn!(task_id = %id, error = %err, "hub convergence failed"),
        }
    }

    fn scan(&self) {
        let now = Utc::now();
        let tick = Instant::now();
        let due: Vec<(TaskId, TaskState, &'static str, &'static str)> = {
            let Ok(active) = self.active.lock() else {
                warn!("active table lock poisoned; skipping scan round");
                return;
            };
            active
                .iter()
                .filter_map(|(id, meta)| {
                    let canceled = meta
                        .cancel_requested_at
                        .is_some_and(|at| tick.duration_since(at) >= self.config.cancel_grace);
                    // 同轮都命中判 Canceled：用户意图优先于「超期」这一推测。
                    if canceled {
                        return Some((
                            *id,
                            TaskState::Canceled,
                            "canceled",
                            "task did not converge after cancel request",
                        ));
                    }
                    let expired = meta.deadline.is_some_and(|deadline| {
                        now.signed_duration_since(deadline)
                            .to_std()
                            .is_ok_and(|elapsed| elapsed >= self.config.terminal_grace)
                    });
                    expired.then_some((
                        *id,
                        TaskState::TimedOut,
                        "deadline",
                        "task did not converge after its deadline",
                    ))
                })
                .collect()
        };

        for (id, state, kind, message) in due {
            let outcome = TaskOutcome {
                output: None,
                error: Some(TaskError {
                    kind: kind.to_string(),
                    message: message.to_string(),
                    detail: None,
                }),
            };
            self.converge(&id, state, outcome);
        }
    }
}

/// 已退出/被丢弃的流再调 `next` 返回 `None`；`Lagged` 之后立即结束，避免客户端在
/// 半新半旧的游标上继续读。
pub struct ObsStream {
    first: Option<TaskSnapshot>,
    rx: Option<broadcast::Receiver<Arc<Update>>>,
    ended: bool,
}

impl ObsStream {
    pub async fn next(&mut self) -> Option<ObsEvent> {
        if let Some(snapshot) = self.first.take() {
            return Some(ObsEvent::Snapshot(snapshot));
        }
        if self.ended {
            return None;
        }
        let Some(rx) = self.rx.as_mut() else {
            // 订阅时已终态：只有首帧，没有增量可等。
            self.ended = true;
            return Some(ObsEvent::End);
        };
        match rx.recv().await {
            Ok(update) => Some(ObsEvent::Update((*update).clone())),
            Err(broadcast::error::RecvError::Lagged(dropped)) => {
                self.ended = true;
                Some(ObsEvent::Lagged { dropped })
            }
            Err(broadcast::error::RecvError::Closed) => {
                self.ended = true;
                Some(ObsEvent::End)
            }
        }
    }
}

fn spawn_store_writer(
    store: Arc<dyn TaskStore>,
    mut rx: mpsc::UnboundedReceiver<(TaskId, TaskSnapshot)>,
) {
    tokio::spawn(async move {
        while let Some((id, snapshot)) = rx.recv().await {
            // 写失败不撤销已接纳任务：可观测性降级比丢掉正在跑的任务代价低。
            if let Err(err) = store.put_snapshot(&snapshot) {
                warn!(task_id = %id, error = %err, "failed to persist task snapshot");
            }
        }
    });
}

/// 兜底扫描：只比较时间戳，每轮 O(活跃数)。持 `Weak` 以免 Hub 被信号量钉住。
fn spawn_deadline_scanner(hub: Weak<HubInner>) {
    tokio::spawn(async move {
        loop {
            // 0 间隔会让这一轮睡眠立即返回，把扫描变成空转烧核。
            let interval = match hub.upgrade() {
                Some(inner) => inner.config.scan_interval.max(Duration::from_millis(1)),
                None => return,
            };
            tokio::time::sleep(interval).await;
            match hub.upgrade() {
                Some(inner) => inner.scan(),
                None => return,
            }
        }
    });
}

fn ensure_open(from: TaskState, to: TaskState) -> Result<(), SinkError> {
    if from.is_terminal() {
        return Err(SinkError::IllegalTransition { from, to });
    }
    Ok(())
}

/// 快照里每个 `id` 恰好一条已合并值：`append` 追加到同 id 的已有字符串，否则整条替换。
/// 超上界返回错误而非截断 —— 截断等于把数据损坏写进权威副本。
fn merge_artifact(
    existing: Option<&Artifact>,
    incoming: &Artifact,
    limit: u64,
    from: TaskState,
) -> Result<Artifact, SinkError> {
    let payload: Value = if incoming.append {
        let piece = incoming
            .payload
            .payload
            .as_str()
            .ok_or(SinkError::IllegalTransition { from, to: from })?;
        let mut merged = match existing {
            Some(known) => known
                .payload
                .payload
                .as_str()
                .ok_or(SinkError::IllegalTransition { from, to: from })?
                .to_string(),
            None => String::new(),
        };
        merged.push_str(piece);
        Value::String(merged)
    } else {
        incoming.payload.payload.clone()
    };

    let offset = payload_size(&payload);
    if offset > limit {
        return Err(SinkError::TooLarge {
            artifact: incoming.id.clone(),
            limit,
        });
    }

    Ok(Artifact {
        id: incoming.id.clone(),
        kind: incoming.kind.clone(),
        offset,
        // 存的是完整值而非分片。
        append: false,
        last_chunk: incoming.last_chunk,
        payload: TaggedJson {
            tag: incoming.payload.tag.clone(),
            payload,
        },
    })
}

/// 字符串按原始字节计长；其它 JSON 值没有更贴切的字节语义，按序列化长度计。
fn payload_size(payload: &Value) -> u64 {
    match payload {
        Value::String(text) => text.len() as u64,
        other => match serde_json::to_vec(other) {
            Ok(bytes) => bytes.len() as u64,
            // 序列化失败只可能是结构本身不合法，保守按超限处理，避免无界写入。
            Err(_) => u64::MAX,
        },
    }
}

fn machine_error(err: SinkError) -> MachineError {
    match err {
        SinkError::NotFound { id } => MachineError::NotFound { id },
        SinkError::IllegalTransition { .. }
        | SinkError::TooLarge { .. }
        | SinkError::OffsetGap { .. } => MachineError::Illegal {
            reason: err.to_string(),
        },
        SinkError::Closed => MachineError::Unavailable {
            reason: err.to_string(),
        },
    }
}

/// 连续这么多次订阅都读不到终态就不再死等：通道有损，重复失效只能报不可得。
const AWAIT_TERMINAL_ATTEMPTS: usize = 3;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::echo::EchoMachine;
    use crate::machine::echo::doubles::{
        AbandoningMachine, ArtifactMachine, ChattyMachine, CountingMachine, DelayedFinishMachine,
        ErrMachine, GateMachine, GateMode, OutputMachine, PacedMachine, PanickingMachine,
        RecordingStore, RejectingMachine, ScriptedMachine, StuckMachine, SubmitFailure,
    };
    use crate::machine::model::{ArtifactId, TaskContext, TaskSource};
    use crate::machine::store::InMemoryStore;
    use std::collections::BTreeMap;
    use tokio::sync::Notify;

    const OVERALL_TIMEOUT: Duration = Duration::from_secs(2);

    // 时间参数取「远大于一个 tick、远小于人等」的量级：不缩短则既跑不动也判不出红。
    fn test_config() -> HubConfig {
        HubConfig {
            update_buffer: 64,
            emit_min_interval: Duration::ZERO,
            output_tail_bytes: 64 * 1024,
            artifact_max_bytes: 8 * 1024 * 1024,
            scan_interval: Duration::from_millis(10),
            terminal_grace: Duration::from_millis(50),
            cancel_grace: Duration::from_millis(50),
        }
    }

    fn task(text: &str) -> Task {
        Task {
            id: TaskId(Uuid::nil()),
            kind: String::new(),
            input: TaggedJson {
                tag: "text".to_string(),
                payload: Value::String(text.to_string()),
            },
            context: TaskContext {
                scope: None,
                data: None,
            },
            source: TaskSource::External {
                principal: "tester".to_string(),
            },
            deadline: None,
            labels: BTreeMap::new(),
            created_at: Utc::now(),
        }
    }

    fn no_filter() -> TaskFilter {
        TaskFilter {
            kind: None,
            state: None,
            labels: BTreeMap::new(),
            scope: None,
            created_after: None,
        }
    }

    /// 轮询到条件成立：替身由门闸驱动，到达时机确定，故只等条件、不等固定时长。
    async fn wait_until(
        hub: &TaskHub,
        id: &TaskId,
        what: &str,
        pred: impl Fn(&TaskSnapshot) -> bool,
    ) -> TaskSnapshot {
        let deadline = Instant::now() + OVERALL_TIMEOUT;
        loop {
            if let Ok(snapshot) = hub.snapshot(id).await
                && pred(&snapshot)
            {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "condition never held: {what}");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    /// 收一整条流到终帧，返回首帧快照与沿途的 `Update`；`Lagged` 记在返回值里。
    /// 超时即判红：缺 `End` 时挂着不返回，会把整套用例拖到外层被杀，断言消息全丢。
    async fn drain(stream: ObsStream) -> (TaskSnapshot, Vec<Update>, bool) {
        tokio::time::timeout(OVERALL_TIMEOUT, drain_unbounded(stream))
            .await
            .expect("the stream never reached its end frame")
    }

    async fn drain_unbounded(mut stream: ObsStream) -> (TaskSnapshot, Vec<Update>, bool) {
        let Some(ObsEvent::Snapshot(first)) = stream.next().await else {
            panic!("the first frame must be a snapshot");
        };
        let mut updates = Vec::new();
        let mut lagged = false;
        loop {
            match stream.next().await {
                Some(ObsEvent::Update(update)) => updates.push(update),
                Some(ObsEvent::Lagged { .. }) => {
                    lagged = true;
                    break;
                }
                Some(ObsEvent::End) | None => break,
                Some(ObsEvent::Snapshot(_)) => panic!("the snapshot frame must come exactly once"),
            }
        }
        (first, updates, lagged)
    }

    /// 终态归属只能看 `Update`：快照没有 `origin`，把归属判据落在快照上等于不判。
    fn assert_terminal_update(updates: &[Update], origin: UpdateOrigin, state: TaskState) {
        let last = updates.last().expect("the stream carried no update");
        assert_eq!(last.origin, origin, "terminal update origin");
        match &last.body {
            UpdateBody::Status { state: actual, .. } => {
                assert_eq!(*actual, state, "terminal update state");
            }
            other => panic!("the last update must be a status, got {other:?}"),
        }
    }

    /// 等 impl 侧的写入结论落定：终态后 Hub 不再广播，结论只在 impl 那里。
    async fn wait_finish_result(machine: &DelayedFinishMachine) -> Result<(), SinkError> {
        let deadline = Instant::now() + OVERALL_TIMEOUT;
        loop {
            if let Some(result) = machine.finish_result() {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "the delayed finish never reported back"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    #[tokio::test]
    async fn submit_returns_before_execution() {
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        let gate = Arc::new(Notify::new());
        hub.register(Arc::new(GateMachine::new(
            "gated",
            Arc::clone(&gate),
            GateMode::BeginAfterGate,
        )))
        .expect("register gated machine");

        // 门闸不放行：`submit` 若等待执行体就撞上这里的超时。
        let id = tokio::time::timeout(
            Duration::from_millis(100),
            hub.submit("gated", task("hello")),
        )
        .await
        .expect("submit must return while the executor is still blocked")
        .expect("submit accepts the task");

        let snapshot = hub.snapshot(&id).await.expect("task is on the shelves");
        assert_eq!(
            snapshot.last_seq, 0,
            "no update may be emitted before submit returns"
        );
        assert_eq!(snapshot.state, TaskState::Submitted);

        // 放行让执行体自己收尾，不必等运行时关闭时被强杀。
        gate.notify_one();
    }

    #[tokio::test]
    async fn submit_errors_leave_no_trace() {
        let store = Arc::new(InMemoryStore::new());
        let hub = TaskHub::new(test_config(), store.clone());
        hub.register(Arc::new(EchoMachine::new()))
            .expect("register echo machine");

        let unknown_kind = hub.submit("missing", task("hello")).await;
        assert!(
            matches!(unknown_kind, Err(MachineError::UnknownKind { .. })),
            "an unregistered kind must be rejected: {unknown_kind:?}"
        );

        let duplicate_kind = hub.register(Arc::new(EchoMachine::new()));
        assert!(
            matches!(duplicate_kind, Err(MachineError::Illegal { .. })),
            "registering the same kind twice must be rejected: {duplicate_kind:?}"
        );

        assert!(
            hub.list(&no_filter()).await.expect("hub list").is_empty(),
            "a rejected submission must never show up in hub list"
        );
        assert!(
            store.list(&no_filter()).expect("store list").is_empty(),
            "a rejected submission must never reach the store"
        );
    }

    #[tokio::test]
    async fn await_all_isolates_failure() {
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(CountingMachine::new("first", 1)))
            .expect("register first machine");
        hub.register(Arc::new(StuckMachine::new("stuck")))
            .expect("register stuck machine");
        hub.register(Arc::new(CountingMachine::new("last", 1)))
            .expect("register last machine");

        let first = hub.submit("first", task("a")).await.expect("submit first");
        let stuck = hub.submit("stuck", task("b")).await.expect("submit stuck");
        let last = hub.submit("last", task("c")).await.expect("submit last");

        let results = hub
            .await_all(&[first, stuck, last], Some(Duration::from_millis(50)))
            .await;

        assert_eq!(
            results.len(),
            3,
            "results must align with the requested ids"
        );
        assert_eq!(
            results[0]
                .as_ref()
                .expect("first reaches its terminal")
                .state,
            TaskState::Completed
        );
        assert!(
            matches!(results[1], Err(MachineError::Unavailable { .. })),
            "the stuck task must be reported on its own index: {:?}",
            results[1]
        );
        assert_eq!(
            results[2]
                .as_ref()
                .expect("last reaches its terminal")
                .state,
            TaskState::Completed
        );
        // 顺序即位置：两个都收敛成 `Completed` 时，只有 id 能判出结果被换过位。
        assert_eq!(
            results[0].as_ref().expect("first snapshot").id,
            first,
            "index 0 must carry the first id"
        );
        assert_eq!(
            results[2].as_ref().expect("last snapshot").id,
            last,
            "index 2 must carry the last id"
        );

        // ② 三个都在等超时：串行等会累加到 3 倍，故判据落在总耗时上。
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        let mut ids = Vec::new();
        for kind in ["stuck-a", "stuck-b", "stuck-c"] {
            hub.register(Arc::new(StuckMachine::new(kind)))
                .expect("register stuck machine");
            ids.push(hub.submit(kind, task(kind)).await.expect("submit"));
        }
        let started = Instant::now();
        let results = hub.await_all(&ids, Some(Duration::from_millis(50))).await;
        let elapsed = started.elapsed();
        assert!(
            results
                .iter()
                .all(|result| matches!(result, Err(MachineError::Unavailable { .. }))),
            "every stuck task must report on its own index: {results:?}"
        );
        assert!(
            elapsed < Duration::from_millis(100),
            "await_all must wait concurrently, three 50ms timeouts took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn begin_reentry_is_noop() {
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        let gate = Arc::new(Notify::new());
        hub.register(Arc::new(GateMachine::new(
            "gated",
            Arc::clone(&gate),
            GateMode::BeginAfterGate,
        )))
        .expect("register gated machine");
        let id = hub.submit("gated", task("hello")).await.expect("submit");
        gate.notify_one();
        let begun = wait_until(&hub, &id, "gated machine begins", |snapshot| {
            snapshot.state == TaskState::Working
        })
        .await;
        assert_eq!(begun.last_seq, 1, "begin is the only update so far");

        // 终态前的第二次 begin 声明的是「已在推进」这一事实，不是推进一步。
        let sink = TaskSink::new(Arc::downgrade(&hub.inner), id);
        assert_eq!(
            sink.begin().await,
            Ok(()),
            "beginning an already working task must be accepted"
        );
        let after = hub.snapshot(&id).await.expect("snapshot");
        assert_eq!(
            after.last_seq, begun.last_seq,
            "a reentrant begin must not emit an update"
        );
        assert_eq!(
            after, begun,
            "a reentrant begin must not touch the snapshot"
        );
    }

    /// 一条 executor 失败分支的完整断言：终态 `Update` 归 Hub、状态 `Failed`、错误种类
    /// 来自 `on_executor_done` 的分支映射。
    async fn assert_executor_failure_converges(
        machine: Arc<dyn Machine>,
        kind: &str,
        expected: &str,
    ) {
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        hub.register(machine).expect("register machine");
        let id = hub.submit(kind, task("boom")).await.expect("submit");

        let (_, updates, _) = drain(hub.subscribe(&id).expect("subscribe")).await;
        assert_terminal_update(&updates, UpdateOrigin::Hub, TaskState::Failed);

        let terminal = tokio::time::timeout(OVERALL_TIMEOUT, hub.await_terminal(&id, None))
            .await
            .expect("an ended executor future must converge without a deadline")
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::Failed, "state for {kind}");
        let error = terminal
            .outcome
            .expect("a failed task carries an outcome")
            .error
            .expect("a failed task carries an error");
        assert_eq!(error.kind, expected, "error kind for {kind}");
    }

    #[tokio::test]
    async fn executor_error_branches_converge() {
        for (kind, failure, expected) in [
            ("err-unavailable", SubmitFailure::Unavailable, "unavailable"),
            ("err-illegal", SubmitFailure::Illegal, "impl_error"),
        ] {
            assert_executor_failure_converges(
                Arc::new(ErrMachine::new(kind, failure)),
                kind,
                expected,
            )
            .await;
        }
        assert_executor_failure_converges(
            Arc::new(PanickingMachine::new("panicking")),
            "panicking",
            "panic",
        )
        .await;
    }

    /// 偏移跳空即中间字节永远读不到：拒收而不是接上，也不占 seq。
    #[tokio::test]
    async fn output_offset_gap_is_rejected() {
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(StuckMachine::new("stuck")))
            .expect("register stuck machine");
        let id = hub.submit("stuck", task("stuck")).await.expect("submit");
        let begun = wait_until(&hub, &id, "stuck machine begins", |snapshot| {
            snapshot.state == TaskState::Working
        })
        .await;

        let sink = TaskSink::new(Arc::downgrade(&hub.inner), id);
        let data = vec![b'a'; 4];
        sink.output(OutputChunk {
            stream: "stdout".to_string(),
            offset: 0,
            data: data.clone(),
        })
        .await
        .expect("a contiguous chunk is accepted");
        let accepted = hub.snapshot(&id).await.expect("snapshot");
        assert_eq!(accepted.output.end_offset, data.len() as u64);
        assert_eq!(accepted.last_seq, begun.last_seq + 1);

        let gap = sink
            .output(OutputChunk {
                stream: "stdout".to_string(),
                offset: 999,
                data: vec![b'b'; 4],
            })
            .await;
        assert_eq!(
            gap,
            Err(SinkError::OffsetGap {
                expected: data.len() as u64,
                got: 999
            }),
            "a chunk past a hole must be rejected"
        );

        let rejected = hub.snapshot(&id).await.expect("snapshot");
        assert_eq!(
            rejected.output.end_offset,
            data.len() as u64,
            "a rejected chunk must not move the end offset"
        );
        assert_eq!(
            rejected.output.tail, data,
            "a rejected chunk must not enter the tail"
        );
        assert_eq!(
            rejected.last_seq, accepted.last_seq,
            "a rejected chunk must not take a seq"
        );
    }

    /// 拒收不破坏状态：按 `OffsetGap.expected` 补发即可续写，seq 只为成功写入推进。
    #[tokio::test]
    async fn output_gap_then_resend_succeeds() {
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(StuckMachine::new("resumable")))
            .expect("register stuck machine");
        let id = hub
            .submit("resumable", task("resumable"))
            .await
            .expect("submit");
        let begun = wait_until(&hub, &id, "stuck machine begins", |snapshot| {
            snapshot.state == TaskState::Working
        })
        .await;

        let sink = TaskSink::new(Arc::downgrade(&hub.inner), id);
        let first = vec![b'a'; 4];
        sink.output(OutputChunk {
            stream: "stdout".to_string(),
            offset: 0,
            data: first.clone(),
        })
        .await
        .expect("a contiguous chunk is accepted");

        let hole = 999;
        let gap = sink
            .output(OutputChunk {
                stream: "stdout".to_string(),
                offset: hole,
                data: vec![b'b'; 4],
            })
            .await;
        assert_eq!(
            gap,
            Err(SinkError::OffsetGap {
                expected: first.len() as u64,
                got: hole
            }),
            "a chunk past a hole must be rejected"
        );

        let resumed = vec![b'c'; 6];
        sink.output(OutputChunk {
            stream: "stdout".to_string(),
            offset: first.len() as u64,
            data: resumed.clone(),
        })
        .await
        .expect("a resend at the expected offset must be accepted");

        let resumed_snapshot = hub.snapshot(&id).await.expect("snapshot");
        assert_eq!(
            resumed_snapshot.output.end_offset,
            (first.len() + resumed.len()) as u64,
            "the resent chunk must continue from the rejected chunk's expected offset"
        );
        assert_eq!(
            resumed_snapshot.output.tail,
            [first, resumed].concat(),
            "the rejected chunk must leave no bytes behind"
        );
        assert_eq!(
            resumed_snapshot.last_seq,
            begun.last_seq + 2,
            "only the two accepted chunks may take a seq"
        );
    }

    #[tokio::test]
    async fn first_frame_is_snapshot_then_dense_seq() {
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        let gate = Arc::new(Notify::new());
        hub.register(Arc::new(ScriptedMachine::new(
            "scripted",
            Arc::clone(&gate),
        )))
        .expect("register scripted machine");

        let id = hub.submit("scripted", task("hello")).await.expect("submit");
        wait_until(&hub, &id, "scripted machine begins", |snapshot| {
            snapshot.last_seq == 1
        })
        .await;

        // 剧本停在门闸上：此刻订阅恰好落在 begin 之后、增量之前。
        let stream = hub.subscribe(&id).expect("subscribe");
        gate.notify_one();

        let (first, updates, lagged) = drain(stream).await;
        assert!(!lagged, "a 5-update script cannot overflow the buffer");
        assert_eq!(first.last_seq, 1, "the snapshot frame carries the begin");
        assert_eq!(first.state, TaskState::Working);
        assert_eq!(
            updates.iter().map(|update| update.seq).collect::<Vec<_>>(),
            vec![2, 3, 4, 5, 6],
            "updates must continue the snapshot without holes"
        );
    }

    // 真并发：观察者与执行体在两条工作线程上同时动，订阅才可能与写入交叠。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_has_no_gap() {
        let mut config = test_config();
        config.update_buffer = 8;
        let hub = TaskHub::new(config, Arc::new(InMemoryStore::new()));

        // 超订下订阅方可能整段被饿死，固定轮数会让用例空转到只剩自检断言；
        // 按有效轮次收敛，上限只用于兜住「环境始终不配合」的无限跑。
        const ROUNDS: usize = 20;
        const MAX_ATTEMPTS: usize = 200;
        let mut checked = 0;
        let mut attempts = 0;
        while checked < ROUNDS && attempts < MAX_ATTEMPTS {
            attempts += 1;
            // 每轮一个独立的门闸：执行体停在「已 begin、增量未发」这一点上。
            let kind = format!("paced-{attempts}");
            let gate = Arc::new(Notify::new());
            hub.register(Arc::new(PacedMachine::new(&kind, Arc::clone(&gate), 24)))
                .expect("register paced machine");
            let id = hub.submit(&kind, task("round")).await.expect("submit");
            let began = wait_until(&hub, &id, "paced machine begins", |snapshot| {
                snapshot.last_seq == 1
            })
            .await;
            assert_eq!(began.state, TaskState::Working);

            // 先放行再订阅：执行体已在推进，订阅点落在流中途而不是流首。
            gate.notify_one();
            let stream = hub.subscribe(&id).expect("subscribe");
            let (first, updates, lagged) = drain(stream).await;
            // 落后的轮次只能证明快照可用，不能证明增量无空洞。
            if lagged {
                continue;
            }
            if updates.is_empty() {
                assert!(
                    first.state.is_terminal(),
                    "a stream without updates can only end at a terminal state"
                );
                continue;
            }
            assert_eq!(
                updates[0].seq,
                first.last_seq + 1,
                "the first update must continue the snapshot in round {attempts}"
            );
            for pair in updates.windows(2) {
                assert_eq!(
                    pair[1].seq,
                    pair[0].seq + 1,
                    "seq must stay dense in round {attempts}"
                );
            }
            checked += 1;
        }
        assert!(
            checked >= 1,
            "no round observed a live stream; the gap check was never exercised"
        );
    }

    #[tokio::test]
    async fn lagged_then_snapshot_recovers() {
        let mut config = test_config();
        config.update_buffer = 2;
        let hub = TaskHub::new(config, Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(CountingMachine::new("counting", 8)))
            .expect("register counting machine");

        let id = hub.submit("counting", task("count")).await.expect("submit");
        let mut stream = hub.subscribe(&id).expect("subscribe");

        // 订阅后完全不出队：两格缓冲留不住 begin + 8 条 progress + finish。
        let terminal = hub
            .await_terminal(&id, Some(OVERALL_TIMEOUT))
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::Completed);

        let Some(ObsEvent::Snapshot(_)) = stream.next().await else {
            panic!("the first frame must be a snapshot");
        };
        let Some(ObsEvent::Lagged { dropped }) = stream.next().await else {
            panic!("an unread overflow must surface as Lagged");
        };
        assert!(dropped > 0, "Lagged must report how much was dropped");
        assert!(
            stream.next().await.is_none(),
            "Lagged must end the stream instead of continuing on a half-stale cursor"
        );

        let snapshot = hub.snapshot(&id).await.expect("snapshot");
        assert_eq!(snapshot.state, TaskState::Completed);
        assert_eq!(
            snapshot.last_seq, 10,
            "begin + 8 progress + finish is what the stream actually carried"
        );
    }

    #[tokio::test]
    async fn progress_throttled_not_lagged() {
        let mut config = test_config();
        config.update_buffer = 2;
        config.emit_min_interval = Duration::from_millis(50);
        let hub = TaskHub::new(config, Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(CountingMachine::new("burst", 200)))
            .expect("register burst machine");

        let id = hub.submit("burst", task("burst")).await.expect("submit");
        let stream = hub.subscribe(&id).expect("subscribe");

        let terminal = hub
            .await_terminal(&id, Some(OVERALL_TIMEOUT))
            .await
            .expect("terminal snapshot");
        assert_eq!(
            terminal.progress.current,
            Some(200),
            "throttled progress must still update the snapshot value"
        );
        assert_eq!(terminal.state, TaskState::Completed);

        let (_, updates, lagged) = drain(stream).await;
        assert!(
            !lagged,
            "progress is the only increment here, so throttling must keep the buffer from overflowing"
        );
        assert!(
            updates.len() < 200,
            "200 progress calls must not produce 200 updates: got {}",
            updates.len()
        );
    }

    #[tokio::test]
    async fn output_tail_is_authoritative() {
        let mut config = test_config();
        config.update_buffer = 8;
        config.output_tail_bytes = 1024;
        let hub = TaskHub::new(config, Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(OutputMachine::new("noisy", 200)))
            .expect("register output machine");

        let id = hub.submit("noisy", task("noisy")).await.expect("submit");
        let stream = hub.subscribe(&id).expect("subscribe");

        // 订阅后一直不读：200 条 output 撑爆 8 格缓冲是设计内的正常路径。
        let terminal = hub
            .await_terminal(&id, Some(OVERALL_TIMEOUT))
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::Completed);

        let (_, _, lagged) = drain(stream).await;
        assert!(
            lagged,
            "a 200-chunk burst over an 8-slot buffer must overflow"
        );

        let snapshot = hub.snapshot(&id).await.expect("snapshot");
        let view = &snapshot.output;
        let chunk = OutputMachine::CHUNK_BYTES;
        assert_eq!(
            view.end_offset,
            (200 * chunk) as u64,
            "the tail end offset counts every chunk the machine sent"
        );
        assert_eq!(
            view.end_offset - view.start_offset,
            view.tail.len() as u64,
            "the window start and the tail must describe the same bytes"
        );
        assert!(
            view.tail.len() <= 1024,
            "the tail must stay within the configured bound: {} bytes",
            view.tail.len()
        );
        assert_eq!(
            view.start_offset % chunk as u64,
            0,
            "uniform chunks can only be trimmed on a chunk boundary"
        );

        let kept = view.tail.len() / chunk;
        let expected: Vec<u8> = (200 - kept..200).flat_map(OutputMachine::chunk).collect();
        assert_eq!(
            view.tail, expected,
            "the tail must be the concatenation of the last {kept} chunks"
        );

        let slid_out = (view.start_offset / chunk as u64) as usize;
        for index in 0..slid_out {
            assert!(
                !view
                    .tail
                    .windows(chunk)
                    .any(|window| window == OutputMachine::chunk(index)),
                "chunk {index} slid out of the window and must not reappear in the tail"
            );
        }

        let outcome = snapshot
            .outcome
            .expect("a terminal snapshot carries an outcome");
        assert_eq!(
            outcome.output,
            Some(OutputMachine::marker()),
            "dropped output chunks must not touch the terminal outcome"
        );
        assert_eq!(outcome.error, None);
    }

    #[tokio::test]
    async fn stuck_machine_converges() {
        // ① deadline 到期：执行体自己永不返回，终态只能由 Hub 兜底。
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(StuckMachine::new("stuck")))
            .expect("register stuck machine");
        let mut expiring = task("stuck");
        expiring.deadline = Some(Utc::now() + Duration::from_millis(50));

        let id = hub.submit("stuck", expiring).await.expect("submit");
        let (_, updates, lagged) = drain(hub.subscribe(&id).expect("subscribe")).await;
        assert!(
            !lagged,
            "begin plus one terminal cannot overflow the buffer"
        );
        assert_terminal_update(&updates, UpdateOrigin::Hub, TaskState::TimedOut);
        let terminal = hub
            .await_terminal(&id, Some(OVERALL_TIMEOUT))
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::TimedOut);

        // ② 无 deadline 但已 cancel：cancel_grace 后同样由 Hub 兜底。
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(StuckMachine::new("stoppable")))
            .expect("register stuck machine");
        let id = hub
            .submit("stoppable", task("stuck"))
            .await
            .expect("submit");
        let stream = hub.subscribe(&id).expect("subscribe");
        hub.cancel(&id).await.expect("cancel the stuck task");

        let (_, updates, lagged) = drain(stream).await;
        assert!(
            !lagged,
            "begin plus one terminal cannot overflow the buffer"
        );
        assert_terminal_update(&updates, UpdateOrigin::Hub, TaskState::Canceled);
        let terminal = hub
            .await_terminal(&id, Some(OVERALL_TIMEOUT))
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::Canceled);
    }

    #[tokio::test]
    async fn executor_gone_converges() {
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(AbandoningMachine::new("abandoning")))
            .expect("register abandoning machine");

        let id = hub
            .submit("abandoning", task("gone"))
            .await
            .expect("submit");
        let (_, updates, lagged) = drain(hub.subscribe(&id).expect("subscribe")).await;
        assert!(
            !lagged,
            "begin plus one terminal cannot overflow the buffer"
        );
        assert_terminal_update(&updates, UpdateOrigin::Hub, TaskState::Failed);

        let terminal = tokio::time::timeout(OVERALL_TIMEOUT, hub.await_terminal(&id, None))
            .await
            .expect("an ended executor future must converge without a deadline")
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::Failed);
        let outcome = terminal
            .outcome
            .expect("a terminal snapshot carries an outcome");
        assert_eq!(
            outcome.error.expect("a failed task carries an error").kind,
            "abandoned",
            "an impl that walks away is not reported as unavailable"
        );
    }

    #[tokio::test]
    async fn late_finish_after_close_is_illegal() {
        // ① 窗口内交付：Hub 不改判 impl 已发布的终态。
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        let within = Arc::new(DelayedFinishMachine::new(
            "within-grace",
            Duration::from_millis(20),
            TaskState::Completed,
        ));
        hub.register(within.clone())
            .expect("register delayed machine");
        let mut expiring = task("late");
        expiring.deadline = Some(Utc::now());
        let id = hub.submit("within-grace", expiring).await.expect("submit");

        let (_, updates, lagged) = drain(hub.subscribe(&id).expect("subscribe")).await;
        assert!(
            !lagged,
            "begin plus one terminal cannot overflow the buffer"
        );
        assert_terminal_update(&updates, UpdateOrigin::Machine, TaskState::Completed);
        assert_eq!(
            wait_finish_result(&within).await,
            Ok(()),
            "a finish inside the grace window must be accepted"
        );

        // ② 窗口外迟到：Hub 已写死 TimedOut，迟到的 Completed 被吸收。
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        let beyond = Arc::new(DelayedFinishMachine::new(
            "beyond-grace",
            Duration::from_millis(120),
            TaskState::Completed,
        ));
        hub.register(beyond.clone())
            .expect("register delayed machine");
        let mut expiring = task("late");
        expiring.deadline = Some(Utc::now());
        let id = hub.submit("beyond-grace", expiring).await.expect("submit");

        let (_, updates, lagged) = drain(hub.subscribe(&id).expect("subscribe")).await;
        assert!(
            !lagged,
            "begin plus one terminal cannot overflow the buffer"
        );
        assert_terminal_update(&updates, UpdateOrigin::Hub, TaskState::TimedOut);

        let result = wait_finish_result(&beyond).await;
        assert!(
            matches!(result, Err(SinkError::IllegalTransition { .. })),
            "a finish after the grace window must be rejected: {result:?}"
        );
        let terminal = hub
            .await_terminal(&id, Some(OVERALL_TIMEOUT))
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::TimedOut);
    }

    #[tokio::test]
    async fn sink_after_terminal_is_illegal() {
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(CountingMachine::new("finished", 1)))
            .expect("register counting machine");
        let id = hub.submit("finished", task("done")).await.expect("submit");
        let terminal = hub
            .await_terminal(&id, Some(OVERALL_TIMEOUT))
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::Completed);

        // 终态后 impl 手里仍有 sink：同一份 (Weak<HubInner>, TaskId) 即真实调用面。
        let sink = TaskSink::new(Arc::downgrade(&hub.inner), id);
        let results = [
            ("begin", sink.begin().await),
            (
                "progress",
                sink.progress(Progress {
                    phase: None,
                    current: Some(1),
                    total: Some(2),
                })
                .await,
            ),
            (
                "output",
                sink.output(OutputChunk {
                    stream: "stdout".to_string(),
                    offset: 0,
                    data: vec![b'x'],
                })
                .await,
            ),
            (
                "finish",
                sink.finish(
                    TaskState::Failed,
                    TaskOutcome {
                        output: None,
                        error: Some(TaskError {
                            kind: "late".to_string(),
                            message: "write after the terminal state".to_string(),
                            detail: None,
                        }),
                    },
                )
                .await,
            ),
        ];
        for (what, result) in results {
            assert!(
                matches!(
                    result,
                    Err(SinkError::IllegalTransition {
                        from: TaskState::Completed,
                        ..
                    })
                ),
                "{what} after a terminal state must be rejected: {result:?}"
            );
        }

        let after = hub.snapshot(&id).await.expect("snapshot");
        assert_eq!(
            after.last_seq, terminal.last_seq,
            "a rejected write must not advance the seq"
        );
        assert_eq!(
            after, terminal,
            "a rejected write must not touch the snapshot"
        );
    }

    #[tokio::test]
    async fn cancel_is_idempotent() {
        // ① 非终态连调两次：只记首次请求，终态只收敛一次。
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(StuckMachine::new("stoppable")))
            .expect("register stuck machine");
        let id = hub
            .submit("stoppable", task("stuck"))
            .await
            .expect("submit");
        let begun = wait_until(&hub, &id, "stuck machine begins", |snapshot| {
            snapshot.last_seq == 1
        })
        .await;
        let stream = hub.subscribe(&id).expect("subscribe");

        hub.cancel(&id).await.expect("first cancel");
        hub.cancel(&id).await.expect("repeated cancel");

        let terminal = hub
            .await_terminal(&id, Some(OVERALL_TIMEOUT))
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::Canceled);
        assert_eq!(
            terminal.last_seq,
            begun.last_seq + 1,
            "cancel and its convergence must advance the seq exactly once"
        );

        let (_, updates, lagged) = drain(stream).await;
        assert!(
            !lagged,
            "begin plus one terminal cannot overflow the buffer"
        );
        let canceled = updates
            .iter()
            .filter(|update| {
                matches!(
                    update.body,
                    UpdateBody::Status {
                        state: TaskState::Canceled,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            canceled, 1,
            "a canceled task must carry exactly one canceled status"
        );

        // ② 终态后 cancel：幂等成功，且不产生 seq、不广播。
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(CountingMachine::new("finished", 1)))
            .expect("register counting machine");
        let id = hub.submit("finished", task("done")).await.expect("submit");
        let terminal = hub
            .await_terminal(&id, Some(OVERALL_TIMEOUT))
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::Completed);

        let stream = hub.subscribe(&id).expect("subscribe");
        hub.cancel(&id).await.expect("cancel after the terminal");
        let (_, updates, lagged) = drain(stream).await;
        assert!(!lagged, "a terminal task has nothing left to lag behind");
        assert!(
            updates.is_empty(),
            "canceling a terminal task must not emit updates: {updates:?}"
        );
        let after = hub.snapshot(&id).await.expect("snapshot");
        assert_eq!(after.last_seq, terminal.last_seq);
        assert_eq!(after, terminal, "canceling a terminal task is a no-op");

        // ③ 重复 cancel 不得重置宽限窗：第二次若把起点推到 t1，收敛就会落在 350ms 之后。
        let mut config = test_config();
        config.cancel_grace = Duration::from_millis(200);
        let hub = TaskHub::new(config, Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(StuckMachine::new("slow-cancel")))
            .expect("register stuck machine");
        let id = hub
            .submit("slow-cancel", task("stuck"))
            .await
            .expect("submit");
        wait_until(&hub, &id, "stuck machine begins", |snapshot| {
            snapshot.state == TaskState::Working
        })
        .await;

        hub.cancel(&id).await.expect("first cancel");
        tokio::time::sleep(Duration::from_millis(150)).await;
        hub.cancel(&id).await.expect("repeated cancel");
        let terminal = hub
            .await_terminal(&id, Some(Duration::from_millis(100)))
            .await
            .expect("the repeated cancel must not push the grace window forward");
        assert_eq!(terminal.state, TaskState::Canceled);
    }

    /// `append` 分片：`offset` 是该片在完整值里的起点，`payload` 必须是字符串。
    fn appended_chunk(id: &str, offset: u64, last_chunk: bool, payload: &str) -> Artifact {
        Artifact {
            id: ArtifactId(id.to_string()),
            kind: "text".to_string(),
            offset,
            append: true,
            last_chunk,
            payload: TaggedJson {
                tag: "text".to_string(),
                payload: Value::String(payload.to_string()),
            },
        }
    }

    /// 轮询到终态快照落库：写经 latest-wins 异步队列，读得太早断的是时序而非持久化。
    async fn wait_for_store(store: &InMemoryStore, id: &TaskId) -> TaskSnapshot {
        let deadline = Instant::now() + OVERALL_TIMEOUT;
        loop {
            if let Ok(Some(snapshot)) = store.load_snapshot(id)
                && snapshot.state.is_terminal()
            {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "the terminal snapshot never reached the store"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    /// 轮询到写次数稳定：latest-wins 队列未排空时，读到的次数只是当时的快照。
    async fn settled_puts(store: &RecordingStore) -> usize {
        let mut last = store.puts();
        let mut stable = 0;
        while stable < 5 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let current = store.puts();
            stable = if current == last { stable + 1 } else { 0 };
            last = current;
        }
        last
    }

    /// 分片必须按 `id` 合并成一条完整值：否则不可替代的产物只能靠有损通道拼。
    #[tokio::test]
    async fn artifact_append_reassembles() {
        // ① 三片追加：快照里每个 id 恰好一条已合并值。
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(ArtifactMachine::new(
            "chunked",
            vec![
                appended_chunk("t", 0, false, "aaaaaaaaaa"),
                appended_chunk("t", 10, false, "bbbbbbbbbb"),
                appended_chunk("t", 20, true, "cccccccccc"),
            ],
        )))
        .expect("register chunked machine");

        let id = hub
            .submit("chunked", task("chunked"))
            .await
            .expect("submit");
        let terminal = hub
            .await_terminal(&id, Some(OVERALL_TIMEOUT))
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::Completed);
        let merged = terminal
            .artifacts
            .first()
            .expect("the merged artifact is in the snapshot");
        assert_eq!(
            terminal.artifacts.len(),
            1,
            "each artifact id must occupy exactly one entry: {:?}",
            terminal.artifacts
        );
        assert_eq!(merged.id, ArtifactId("t".to_string()));
        assert_eq!(
            merged.payload.payload,
            Value::String("aaaaaaaaaabbbbbbbbbbcccccccccc".to_string()),
            "the payload must be the three chunks joined in order"
        );
        assert_eq!(merged.offset, 30, "the merged offset is the total length");
        assert!(merged.last_chunk, "last_chunk follows the latest chunk");

        // ② 上界 16 字节：10B + 10B 的合并必然超界，第二片被拒且不增长。
        const LIMIT: u64 = 16;
        let mut config = test_config();
        config.artifact_max_bytes = LIMIT;
        let hub = TaskHub::new(config, Arc::new(InMemoryStore::new()));
        let bounded = Arc::new(ArtifactMachine::new(
            "bounded",
            vec![
                appended_chunk("t", 0, false, "aaaaaaaaaa"),
                appended_chunk("t", 10, true, "bbbbbbbbbb"),
            ],
        ));
        hub.register(bounded.clone())
            .expect("register bounded machine");

        let id = hub
            .submit("bounded", task("bounded"))
            .await
            .expect("submit");
        let terminal = hub
            .await_terminal(&id, Some(OVERALL_TIMEOUT))
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::Completed);

        let results = bounded.results();
        assert_eq!(results.len(), 2, "each chunk must report its own verdict");
        assert_eq!(results[0], Ok(()), "a chunk within the bound is accepted");
        assert!(
            matches!(results[1], Err(SinkError::TooLarge { .. })),
            "a chunk past the bound must be rejected: {:?}",
            results[1]
        );
        let merged = terminal
            .artifacts
            .first()
            .expect("the accepted chunk is in the snapshot");
        assert!(
            merged.offset <= LIMIT,
            "a rejected chunk must not grow the merged value: {} bytes",
            merged.offset
        );
        assert_eq!(
            merged.payload.payload,
            Value::String("aaaaaaaaaa".to_string()),
            "the rejected chunk must be dropped whole, never truncated"
        );
    }

    /// 准入结果必须在 ID 与流上都可见：拒收是终态，需要人介入不是 `Working`。
    #[tokio::test]
    async fn admission_outcomes_are_observable() {
        // ① impl 接单后拒收：带 id 的权威终态，不是调用失败。
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(RejectingMachine::new("rejecting")))
            .expect("register rejecting machine");
        let id = hub
            .submit("rejecting", task("hello"))
            .await
            .expect("a rejection must not fail the submission");

        let (_, updates, lagged) = drain(hub.subscribe(&id).expect("subscribe")).await;
        assert!(
            !lagged,
            "a rejection carried on a fresh stream cannot overflow the buffer"
        );
        assert_terminal_update(&updates, UpdateOrigin::Machine, TaskState::Rejected);

        let rejected = hub
            .snapshot(&id)
            .await
            .expect("a rejection carries a task id");
        assert_eq!(rejected.state, TaskState::Rejected);
        assert!(
            rejected.outcome.is_some(),
            "a rejection is terminal, so it must carry an outcome"
        );

        // ② 还没推进就要求人介入：`Submitted` 直达 `InputRequired`，不得伪造 `Working`。
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        let gate = Arc::new(Notify::new());
        hub.register(Arc::new(GateMachine::new(
            "gated-input",
            Arc::clone(&gate),
            GateMode::WaitInputBeforeBegin,
        )))
        .expect("register gated machine");
        let id = hub
            .submit("gated-input", task("hello"))
            .await
            .expect("submit");
        let mut stream = hub.subscribe(&id).expect("subscribe");
        gate.notify_one();

        let bodies = tokio::time::timeout(OVERALL_TIMEOUT, async move {
            let Some(ObsEvent::Snapshot(_)) = stream.next().await else {
                panic!("the first frame must be a snapshot");
            };
            let mut bodies = Vec::new();
            loop {
                match stream.next().await {
                    Some(ObsEvent::Update(update)) => {
                        let reached = matches!(
                            &update.body,
                            UpdateBody::Status {
                                state: TaskState::InputRequired,
                                ..
                            }
                        );
                        bodies.push(update.body);
                        if reached {
                            return bodies;
                        }
                    }
                    Some(ObsEvent::Lagged { .. }) | Some(ObsEvent::End) | None => {
                        panic!("the stream ended before the input request")
                    }
                    Some(ObsEvent::Snapshot(_)) => {
                        panic!("the snapshot frame must come exactly once")
                    }
                }
            }
        })
        .await
        .expect("the input request never arrived");

        assert!(
            !bodies.iter().any(|body| matches!(
                body,
                UpdateBody::Status {
                    state: TaskState::Working,
                    ..
                }
            )),
            "an impl that never advanced must not be reported as working: {bodies:?}"
        );
        let paused = hub.snapshot(&id).await.expect("snapshot");
        assert_eq!(paused.state, TaskState::InputRequired);
    }

    /// 只用合作型替身跑通全链：机制若依赖某个具体执行体，这条就编译不出。
    #[tokio::test]
    async fn echo_machine_end_to_end() {
        let hub = TaskHub::new(test_config(), Arc::new(InMemoryStore::new()));
        hub.register(Arc::new(EchoMachine::new()))
            .expect("register echo machine");

        let payload = serde_json::json!({"op": "resize", "size": {"w": 640, "h": 480}});
        let mut submitted = task("ignored");
        submitted.input = TaggedJson {
            tag: "weird".to_string(),
            payload: payload.clone(),
        };

        let id = hub.submit("echo", submitted).await.expect("submit");
        let (first, updates, lagged) = drain(hub.subscribe(&id).expect("subscribe")).await;
        assert!(
            !lagged,
            "begin, two progress, one artifact and one terminal fit the buffer"
        );
        for pair in updates.windows(2) {
            assert_eq!(
                pair[1].seq,
                pair[0].seq + 1,
                "seq must stay dense up to the terminal update"
            );
        }
        assert_terminal_update(&updates, UpdateOrigin::Machine, TaskState::Completed);
        assert_eq!(
            updates[0].seq,
            first.last_seq + 1,
            "the snapshot frame must be continued immediately by the first update"
        );

        let terminal = hub
            .await_terminal(&id, Some(OVERALL_TIMEOUT))
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::Completed);
        let output = terminal
            .outcome
            .expect("a terminal snapshot carries an outcome")
            .output
            .expect("the echo machine delivers the submitted input as its result");
        assert_eq!(
            output.tag, "weird",
            "the hub must not rename an unknown tag"
        );
        assert_eq!(
            output.payload, payload,
            "every field of the submitted payload must survive unchanged"
        );
        // `EchoMachine` 只经 `begin` 进入 `Working`，不声明状态载荷，故未知 tag 的
        // 透传落在结果与产物两面。
        let artifact = terminal
            .artifacts
            .iter()
            .find(|artifact| artifact.id == ArtifactId("echo".to_string()))
            .expect("the echo artifact");
        assert_eq!(artifact.payload.tag, "weird");
        assert_eq!(artifact.payload.payload, payload);
    }

    /// 换 Hub 不丢终态快照，也不复活任务：重启后的承诺只有「读得到」。
    #[tokio::test]
    async fn store_survives_hub_rebuild() {
        let store = Arc::new(InMemoryStore::new());
        let hub = TaskHub::new(test_config(), store.clone());
        hub.register(Arc::new(EchoMachine::new()))
            .expect("register echo machine");

        let id = hub.submit("echo", task("persisted")).await.expect("submit");
        let terminal = hub
            .await_terminal(&id, Some(OVERALL_TIMEOUT))
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::Completed);

        let persisted = wait_for_store(&store, &id).await;
        assert_eq!(persisted.state, TaskState::Completed);
        assert_eq!(
            persisted.last_seq, terminal.last_seq,
            "the stored copy is the snapshot at the terminal transition"
        );

        let rebuilt = TaskHub::new(test_config(), store.clone());
        let missing = rebuilt.snapshot(&id).await;
        assert!(
            matches!(missing, Err(MachineError::NotFound { .. })),
            "a fresh hub must not resurrect a task it never admitted: {missing:?}"
        );
    }

    /// Store 只落状态转移：热写若也落库，次数就会超过转移数。
    #[tokio::test]
    async fn recording_store_counts_state_transitions() {
        let store = Arc::new(RecordingStore::new());
        let hub = TaskHub::new(test_config(), store.clone());
        hub.register(Arc::new(ChattyMachine::new("chatty")))
            .expect("register chatty machine");

        let id = hub.submit("chatty", task("chatty")).await.expect("submit");
        let terminal = hub
            .await_terminal(&id, Some(OVERALL_TIMEOUT))
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::Completed);
        // 两类热写确实发过，否则「没落库」可以由「没发」解释。
        assert_eq!(
            terminal.progress.current,
            Some(ChattyMachine::PROGRESS_STEPS),
            "the chatty machine must have sent progress"
        );
        assert_eq!(
            terminal.output.end_offset,
            (ChattyMachine::OUTPUT_CHUNKS * ChattyMachine::CHUNK_BYTES) as u64,
            "the chatty machine must have sent output"
        );

        // 句柄诞生时先落一次 `Submitted` 快照，此后只有状态转移落库。
        const TRANSITIONS: usize = 2;
        let puts = settled_puts(&store).await;
        assert_eq!(
            puts,
            1 + TRANSITIONS,
            "the store sees the admitted snapshot plus one write per state transition"
        );
    }
}
