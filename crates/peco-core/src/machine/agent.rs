use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{Sleep, sleep};
use tracing::warn;

use crate::agent::{
    Agent, AgentLooper, LooperConfig, LooperEvent, LooperHandle, TurnFailureReason, TurnOutcome,
};
use crate::session::{NullSessionPersister, Session, SessionPersister};

use super::machine::{Machine, MachineDescriptor, MachineError};
use super::model::{
    Artifact, ArtifactId, OutputView, Progress, TaggedJson, Task, TaskError, TaskId, TaskOutcome,
    TaskSnapshot,
};
use super::sink::{SinkError, TaskSink};
use super::state::{OutputChunk, TaskState};

const PROMPT_TAG: &str = "prompt";
const TEXT_STREAM: &str = "text";

/// 取消信号缓冲够用即可：`Signal::Cancel` 幂等，堆满说明取消已在途中。
const CONTROL_BUFFER: usize = 4;

/// 会话描述只作人读标签，超出部分截断不损失任何判定信息。
const DESCRIPTION_CHARS: usize = 120;

/// 工具声明的产物在 `Artifact.payload` 里的 tag。
pub(crate) const ARTIFACT_DECL_TAG: &str = "artifact_decl";

/// 单任务可上浮的声明数上限：声明数本身无上界，跑飞的子 agent 输出上万行会把
/// 每个任务的快照撑大、每条各推一次 seq。
const MAX_ARTIFACTS_PER_TASK: usize = 64;

/// `AgentMachine` 的装配参数。
pub struct AgentMachineConfig {
    /// `TaskHub` 的路由键。默认 `"agent"`；同 Hub 里要跑多个 agent 时必须改名
    /// （如 `agent:<name>`），因为 `register` 对重复 kind 直接拒绝。
    pub kind: String,
    /// 会话落盘器。默认 `NullSessionPersister`，即关闭持久化。
    pub persister: Arc<dyn SessionPersister>,
    pub looper_config: LooperConfig,
    /// 取消后留给 looper 自行收敛的时长。
    ///
    /// 必须明显小于 `HubConfig::cancel_grace`：超时后 driver 自己写 `Canceled`，
    /// 一旦 Hub 的兜底先到，终态的 `origin` 就变成 `Hub` 而不是 `Machine`。
    pub cancel_grace: Duration,
}

impl Default for AgentMachineConfig {
    fn default() -> Self {
        Self {
            kind: "agent".to_string(),
            persister: Arc::new(NullSessionPersister),
            looper_config: LooperConfig::default(),
            cancel_grace: Duration::from_secs(1),
        }
    }
}

/// 把一次 `AgentLooper` 轮次包成 [`Machine`]。
///
/// 终态只认 `TurnComplete`：`TextDelta` 走有损通道（缓冲满即丢），只能当实时视图，
/// 故 `output` 与 `outcome.output` 是两件事，前者不承诺等于后者。
pub struct AgentMachine {
    agent: Arc<Agent>,
    config: AgentMachineConfig,
    /// 表与 driver 共享：终态回写发生在 driver 里，而 driver 只拿到这张表的 `Arc`。
    runs: Arc<RunTable>,
}

impl AgentMachine {
    pub fn new(agent: Arc<Agent>, config: AgentMachineConfig) -> Self {
        Self {
            agent,
            config,
            runs: Arc::new(RunTable::new()),
        }
    }

    #[cfg(test)]
    fn active_controls(&self) -> Vec<TaskId> {
        self.runs
            .lock()
            .iter()
            .filter(|(_, run)| run.control.is_some())
            .map(|(id, _)| *id)
            .collect()
    }

    fn control(&self, id: &TaskId) -> Option<mpsc::Sender<Signal>> {
        self.runs.lock().get(id).and_then(|run| run.control.clone())
    }

    /// 非权威快照：观察协议的唯一实现在 Hub，`progress` / `output` / `artifacts` /
    /// `last_seq` 都不反映真实观察面，只用于回答「本机上这个任务还在不在、走到哪」。
    ///
    /// Hub 兜底先落终态时，本表只置 `control = None`，不回写 `state`，故此时本快照
    /// 可能仍报 `Working`。
    fn snapshot_of(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
        let runs = self.runs.lock();
        let run = runs.get(id).ok_or(MachineError::NotFound { id: *id })?;
        Ok(TaskSnapshot {
            id: run.task.id,
            kind: run.task.kind.clone(),
            state: run.state,
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
            outcome: run.outcome.clone(),
            artifacts: Vec::new(),
            deadline: run.task.deadline,
            source: run.task.source.clone(),
            labels: run.task.labels.clone(),
            scope: run.task.context.scope.clone(),
            created_at: run.task.created_at,
            updated_at: run.updated_at,
            last_seq: 0,
        })
    }
}

#[async_trait]
impl Machine for AgentMachine {
    fn descriptor(&self) -> MachineDescriptor {
        MachineDescriptor {
            kind: self.config.kind.clone(),
            state_tag: "agent".to_string(),
            input_tag: PROMPT_TAG.to_string(),
            description: "runs one agent turn for the task prompt".to_string(),
        }
    }

    async fn submit(&self, task: Task, sink: TaskSink) -> Result<(), MachineError> {
        // 输入校验先于一切副作用：拒绝的输入不得拉起 looper、留下条目或花掉一次模型调用。
        let prompt = parse_prompt(&task.input)?;

        if let Err(err) = sink.begin().await {
            let reason = err.to_string();
            return Err(match err {
                SinkError::Closed => MachineError::Unavailable { reason },
                _ => MachineError::Illegal { reason },
            });
        }

        let id = task.id;
        let deadline = task.deadline;
        let session = Session::new(id.to_string(), session_description(&prompt));
        let handle = AgentLooper::spawn(
            Arc::clone(&self.agent),
            Box::new(session),
            self.config.looper_config.clone(),
            Arc::clone(&self.config.persister),
        );

        let (control, signals) = mpsc::channel(CONTROL_BUFFER);
        self.runs.lock().insert(
            id,
            Run {
                task,
                state: TaskState::Working,
                outcome: None,
                updated_at: Utc::now(),
                control: Some(control),
            },
        );

        if let Err(err) = handle.send_query(prompt).await {
            // driver 没起来：留着条目会让 cancel/snapshot 看见一个永远不会动的任务。
            self.runs.lock().remove(&id);
            let message = err.to_string();
            let _ = sink
                .finish(
                    TaskState::Failed,
                    error_outcome("looper_unavailable", message.clone()),
                )
                .await;
            return Err(MachineError::Unavailable { reason: message });
        }

        // 执行体与 `submit` 的 future 共寿命：Hub 在 future 结束时会给非终态任务补写
        // `Failed{kind:"abandoned"}`，把 driver 扔到独立任务里等于让仍在跑的任务被立刻判死。
        drive(
            id,
            sink,
            handle,
            signals,
            Arc::clone(&self.runs),
            self.config.cancel_grace,
            deadline,
        )
        .await;
        Ok(())
    }

    async fn cancel(&self, id: &TaskId) -> Result<(), MachineError> {
        if let Some(control) = self.control(id) {
            // 投不到就算了：通道满说明取消已在途中，通道关闭说明 driver 已退出。
            let _ = control.try_send(Signal::Cancel);
        }
        // 未知 id 与已终态都算成功：任务的所有权在 Hub，这里只负责转发。
        Ok(())
    }

    async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
        self.snapshot_of(id)
    }
}

struct RunTable(Mutex<HashMap<TaskId, Run>>);

impl RunTable {
    fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
    }

    /// 锁中毒不致命：表里只有普通数据，沿用中毒前的状态比让整台 machine 失效更接近真实。
    ///
    /// 与 Hub 的 `map_err` 不同：那边的中毒可能让任务状态失真，宁可报错；本表只放
    /// 调度用的普通数据，恢复的代价明显更小。
    fn lock(&self) -> MutexGuard<'_, HashMap<TaskId, Run>> {
        self.0.lock().unwrap_or_else(|poisoned| {
            warn!("run table lock poisoned; continuing with recovered state");
            poisoned.into_inner()
        })
    }
}

struct Run {
    task: Task,
    state: TaskState,
    outcome: Option<TaskOutcome>,
    updated_at: DateTime<Utc>,
    /// `None` 表示 driver 已退出：终态已写定或启动失败，不再接收取消。
    control: Option<mpsc::Sender<Signal>>,
}

enum Signal {
    Cancel,
}

/// 一帧增量的落写结局。
enum DeltaWrite {
    Written,
    /// 校正偏移后仍不连续：只能收敛到失败，否则后面每帧都会撞同一个缺口。
    Gap,
    /// 终态已被别的路径写定，或 Hub 已关闭：不抢写，只清理。
    Blocked,
}

async fn drive(
    id: TaskId,
    sink: TaskSink,
    handle: LooperHandle,
    mut control: mpsc::Receiver<Signal>,
    runs: Arc<RunTable>,
    cancel_grace: Duration,
    deadline: Option<DateTime<Utc>>,
) {
    // 过期判定在入口一次做定：`sleep(0)` 的首个 poll 仍是 `Pending`，与已就绪事件
    // 同时在册时从不先手，交给 `select!` 就等于让已过期的任务先白跑一轮。
    if deadline.is_some_and(|at| at <= Utc::now()) {
        // 先停住可能已开始的模型调用，再写终态。
        let _ = handle.cancel().await;
        let (state, outcome) = deadline_verdict();
        settle(&runs, &sink, &id, state, outcome).await;
        return;
    }

    let mut offset = 0u64;
    let mut declared = 0usize;
    let mut deadline_sleep = deadline.map(|at| Box::pin(sleep(remaining_until(at))));
    let mut cancel_sleep: Option<Pin<Box<Sleep>>> = None;

    // 三路都可能同时就绪，故不加 biased：让公平轮询避免增量洪流饿死 deadline。
    let verdict = loop {
        tokio::select! {
            event = handle.recv_event() => {
                match event {
                    None => {
                        break Some(error_verdict(
                            TaskState::Failed,
                            "looper_closed",
                            "looper event channel closed".to_string(),
                        ));
                    }
                    Some(LooperEvent::TextDelta { delta }) => {
                        if delta.is_empty() {
                            continue;
                        }
                        match write_delta(&sink, &mut offset, delta.as_bytes()).await {
                            DeltaWrite::Written => {}
                            DeltaWrite::Gap => {
                                break Some(error_verdict(
                                    TaskState::Failed,
                                    "output_gap",
                                    "output offset never matched the hub window".to_string(),
                                ));
                            }
                            DeltaWrite::Blocked => break None,
                        }
                    }
                    Some(LooperEvent::TurnComplete { outcome, .. }) => {
                        break Some(from_turn(outcome));
                    }
                    Some(LooperEvent::Shutdown { reason, .. }) => {
                        break Some(error_verdict(TaskState::Failed, "looper_shutdown", reason));
                    }
                    Some(LooperEvent::ToolResult { artifacts, .. }) => {
                        // 声明已从工具结果正文里剥离：不在这一帧写进 sink 就永久丢失。
                        // 写失败按「已丢」处理，不改变本轮收敛。
                        for decl in &artifacts {
                            if declared >= MAX_ARTIFACTS_PER_TASK {
                                warn!(
                                    task_id = %id,
                                    "artifact declarations beyond the per-task cap are dropped"
                                );
                                break;
                            }
                            let Some(artifact) = artifact_from_decl(decl) else {
                                continue;
                            };
                            if let Err(err) = sink.artifact(artifact).await {
                                warn!(
                                    task_id = %id,
                                    error = %err,
                                    "artifact declaration dropped; the hub refused it"
                                );
                            }
                            declared += 1;
                        }
                    }
                    // 推理增量 / 工具调用 / 用量都不进产出。
                    Some(_) => {}
                }
            }
            Some(Signal::Cancel) = control.recv() => {
                // 先武装宽限窗再转发取消：转发要等 looper 排空消息，它若被在途步进
                // 卡住，兜底计时不该跟着推迟。
                // 只记首次：重复取消不得延长宽限窗，否则连续的 cancel 能把收敛无限推迟。
                if cancel_sleep.is_none() {
                    cancel_sleep = Some(Box::pin(sleep(cancel_grace)));
                }
                let _ = handle.cancel().await;
            }
            () = wait_armed(&mut deadline_sleep) => {
                // 先停住花费中的模型调用再写终态：写早了 looper 还在继续产 token。
                let _ = handle.cancel().await;
                break Some(deadline_verdict());
            }
            () = wait_armed(&mut cancel_sleep) => {
                break Some(error_verdict(
                    TaskState::Canceled,
                    "canceled",
                    "cancel did not converge within grace".to_string(),
                ));
            }
        }
    };

    match verdict {
        Some((state, outcome)) => settle(&runs, &sink, &id, state, outcome).await,
        None => retire(&runs, &id),
    }
}

/// 一任务一次终态写：`Ok` 才回写本机表；被拒说明终态已被别的路径写定，重试只会撞同一堵墙。
async fn settle(
    runs: &RunTable,
    sink: &TaskSink,
    id: &TaskId,
    state: TaskState,
    outcome: TaskOutcome,
) {
    match sink.finish(state, outcome.clone()).await {
        Ok(()) => {
            let mut runs = runs.lock();
            if let Some(run) = runs.get_mut(id) {
                run.state = state;
                run.outcome = Some(outcome);
                run.updated_at = Utc::now();
                run.control = None;
            }
        }
        // 先到者胜：Hub 兜底已写定终态或 Hub 已关闭，重试只会撞同一堵墙。
        Err(SinkError::IllegalTransition { .. } | SinkError::Closed) => retire(runs, id),
        Err(err) => {
            warn!(task_id = %id, error = %err, "settle rejected with an unexpected error");
            retire(runs, id);
        }
    }
}

fn retire(runs: &RunTable, id: &TaskId) {
    if let Some(run) = runs.lock().get_mut(id) {
        run.control = None;
    }
}

/// 偏移由 Hub 校验：`OffsetGap` 已经给出真实缺口，按它校正一次即可，再撞就放弃。
async fn write_delta(sink: &TaskSink, offset: &mut u64, data: &[u8]) -> DeltaWrite {
    let chunk = |at: u64| OutputChunk {
        stream: TEXT_STREAM.to_string(),
        offset: at,
        data: data.to_vec(),
    };
    match sink.output(chunk(*offset)).await {
        Ok(()) => {
            *offset += data.len() as u64;
            DeltaWrite::Written
        }
        Err(SinkError::OffsetGap { expected, .. }) => {
            *offset = expected;
            match sink.output(chunk(expected)).await {
                Ok(()) => {
                    *offset += data.len() as u64;
                    DeltaWrite::Written
                }
                Err(_) => DeltaWrite::Gap,
            }
        }
        Err(_) => DeltaWrite::Blocked,
    }
}

/// `None` 时永不就绪：把「deadline 缺失」「宽限窗尚未起算」表达成同一条 select 分支，
/// 免得为未武装的睡眠写一支永不触发的占位臂。
async fn wait_armed(sleep: &mut Option<Pin<Box<Sleep>>>) {
    match sleep {
        Some(armed) => armed.as_mut().await,
        None => std::future::pending().await,
    }
}

/// 已过期取 0：绝对时间已经过去时 `to_std` 拿不到 `Duration`，只能折成 0。
/// 但 `sleep(0)` 的首个 poll 仍是 `Pending`（实测约 1ms 后才完成），与已就绪事件
/// 同时在册时从不先手 —— 过期场景靠 `drive` 入口的判定收敛，这里只求不 panic。
fn remaining_until(at: DateTime<Utc>) -> Duration {
    (at - Utc::now()).to_std().unwrap_or(Duration::ZERO)
}

fn parse_prompt(input: &TaggedJson) -> Result<String, MachineError> {
    if input.tag == PROMPT_TAG
        && let Some(prompt) = input.payload.as_str()
    {
        return Ok(prompt.to_string());
    }
    Err(MachineError::Illegal {
        reason: format!(
            "input must carry tag \"{PROMPT_TAG}\" with a string payload, got tag \"{}\"",
            input.tag
        ),
    })
}

/// 一条声明整条覆盖同 id 的已有值（`append: false`）：`append: true` 只接受字符串
/// payload，对象会被 Hub 按非法转移拒掉，而调用点是尽力而为，产物会静默消失。
fn artifact_from_decl(decl: &crate::artifacts::ArtifactDecl) -> Option<Artifact> {
    if decl.path.is_empty() {
        // 多条空 path 会塌成同一条产物，各自覆盖对方。
        warn!("artifact declaration without a path is dropped");
        return None;
    }
    let payload = match serde_json::to_value(decl) {
        Ok(payload) => payload,
        // 结构上不会发生：字段全是 String/Option<String>。
        Err(err) => {
            warn!(error = %err, "artifact declaration is not serializable; dropped");
            return None;
        }
    };
    Some(Artifact {
        id: ArtifactId(decl.path.clone()),
        kind: ARTIFACT_DECL_TAG.to_string(),
        // 单条即完整值：offset 由 Hub 按 payload 长度重算，不入参。
        offset: 0,
        append: false,
        last_chunk: true,
        payload: TaggedJson {
            tag: ARTIFACT_DECL_TAG.to_string(),
            payload,
        },
    })
}

fn session_description(prompt: &str) -> String {
    prompt.chars().take(DESCRIPTION_CHARS).collect()
}

/// `TurnComplete` 是唯一可信的终态来源：`Failed{Cancelled}` 是取消落地的正规路径，
/// 语义上属 `Canceled` 而不是 `Failed`。
fn from_turn(outcome: TurnOutcome) -> (TaskState, TaskOutcome) {
    match outcome {
        TurnOutcome::Success { text } => (
            TaskState::Completed,
            TaskOutcome {
                output: Some(TaggedJson {
                    tag: TEXT_STREAM.to_string(),
                    payload: Value::String(text),
                }),
                error: None,
            },
        ),
        TurnOutcome::Failed { reason, .. } => failure_verdict(reason),
    }
}

fn failure_verdict(reason: TurnFailureReason) -> (TaskState, TaskOutcome) {
    let (state, kind, message, detail) = match reason {
        TurnFailureReason::Cancelled => (
            TaskState::Canceled,
            "canceled".to_string(),
            "turn canceled".to_string(),
            None,
        ),
        TurnFailureReason::MaxIterationsExceeded => (
            TaskState::Failed,
            "max_iterations".to_string(),
            "max iterations exceeded".to_string(),
            None,
        ),
        TurnFailureReason::HookAbort(why) => {
            (TaskState::Failed, "hook_abort".to_string(), why, None)
        }
        TurnFailureReason::RateLimited { attempts, message } => (
            TaskState::Failed,
            "rate_limited".to_string(),
            message,
            Some(attempts_detail(attempts)),
        ),
        TurnFailureReason::ModelUnavailable { attempts, message } => (
            TaskState::Failed,
            "model_unavailable".to_string(),
            message,
            Some(attempts_detail(attempts)),
        ),
        TurnFailureReason::AuthError { message } => {
            (TaskState::Failed, "auth_error".to_string(), message, None)
        }
        TurnFailureReason::QuotaExhausted { message } => (
            TaskState::Failed,
            "quota_exhausted".to_string(),
            message,
            None,
        ),
        // 原因集合是开放的：新增变体不该让本文件编译失败，兜底仍报 Failed。
        other => (
            TaskState::Failed,
            "turn_failed".to_string(),
            format!("{other:?}"),
            None,
        ),
    };
    (
        state,
        TaskOutcome {
            output: None,
            error: Some(TaskError {
                kind,
                message,
                detail,
            }),
        },
    )
}

fn attempts_detail(attempts: u32) -> TaggedJson {
    TaggedJson {
        tag: "attempts".to_string(),
        payload: Value::from(attempts),
    }
}

fn error_verdict(state: TaskState, kind: &str, message: String) -> (TaskState, TaskOutcome) {
    (state, error_outcome(kind, message))
}

/// deadline 终态的唯一来源：入口过期判定与循环内到点必须是同一个 kind 与文案。
fn deadline_verdict() -> (TaskState, TaskOutcome) {
    error_verdict(
        TaskState::TimedOut,
        "deadline",
        "task deadline exceeded".to_string(),
    )
}

fn error_outcome(kind: &str, message: String) -> TaskOutcome {
    TaskOutcome {
        output: None,
        error: Some(TaskError {
            kind: kind.to_string(),
            message,
            detail: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use model_provider::{
        BlockType, Content, ContentBlock, FinishReason, GenerateRequest, GenerateResult,
        GenerateStream, ModelProvider, ProviderError, StreamChunk, Usage,
    };
    use uuid::Uuid;

    use crate::agent::test_support::{
        GateTool, ScriptedBatchProvider, StaticTool, completed, scripted_agent,
        scripted_agent_with_tools, tool_call_response,
    };
    use crate::agent::{AgentProfile, ModelConfigBuilder};
    use crate::machine::hub::{HubConfig, ObsStream, TaskHub};
    use crate::machine::model::{TaskContext, TaskSource};
    use crate::machine::state::{ObsEvent, Update, UpdateBody, UpdateOrigin};
    use crate::machine::store::InMemoryStore;

    use super::*;

    const BUDGET: Duration = Duration::from_secs(2);

    /// 每条用例整体限时：收敛失效时判红，而不是把测试挂在那里。
    async fn within_budget<F: Future<Output = ()>>(body: F) {
        tokio::time::timeout(BUDGET, body)
            .await
            .expect("the test body must finish within the budget");
    }

    // 扫描间隔远大于一个 tick、宽限窗远小于人等：Hub 的兜底必须够不着，
    // 否则本机不收敛也会被兜底掩盖成通过。
    fn hub_config() -> HubConfig {
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

    fn hub_for(machine: Arc<dyn Machine>, config: HubConfig) -> Arc<TaskHub> {
        let hub = TaskHub::new(config, Arc::new(InMemoryStore::new()));
        hub.register(machine)
            .expect("register the machine under test");
        hub
    }

    fn text(payload: &str) -> TaggedJson {
        TaggedJson {
            tag: TEXT_STREAM.to_string(),
            payload: Value::String(payload.to_string()),
        }
    }

    fn prompt(payload: &str) -> TaggedJson {
        TaggedJson {
            tag: PROMPT_TAG.to_string(),
            payload: Value::String(payload.to_string()),
        }
    }

    fn task_with(input: TaggedJson, deadline: Option<DateTime<Utc>>) -> Task {
        Task {
            // Hub 会覆盖 id 与 kind，这里只填出合法值。
            id: TaskId(Uuid::nil()),
            kind: String::new(),
            input,
            context: TaskContext {
                scope: None,
                data: None,
            },
            source: TaskSource::External {
                principal: "tester".to_string(),
            },
            deadline,
            labels: BTreeMap::new(),
            created_at: Utc::now(),
        }
    }

    fn machine_for(agent: Arc<Agent>, config: AgentMachineConfig) -> Arc<AgentMachine> {
        Arc::new(AgentMachine::new(agent, config))
    }

    fn scripted_machine(
        responses: Vec<GenerateResult>,
    ) -> (Arc<AgentMachine>, Arc<ScriptedBatchProvider>) {
        let (agent, provider) = scripted_agent(responses);
        (machine_for(agent, AgentMachineConfig::default()), provider)
    }

    /// 只在本模块用的流式 provider：按固定片段吐 `TextDelta`。
    ///
    /// `agent::test_support` 的桩件只有 batch 版，而 batch 路径不发增量 —— 复用它
    /// 就永远走不到 `write_delta` 与 Hub 的偏移窗口。
    struct FixedStreamProvider {
        fragments: Vec<&'static str>,
        calls: AtomicUsize,
    }

    impl FixedStreamProvider {
        fn new(fragments: Vec<&'static str>) -> Self {
            Self {
                fragments,
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        /// 首尾的块闭合与 `Finish` 都不能省：缺 `Finish` 会被 assembler 判成流被掐断。
        fn script(&self) -> Vec<StreamChunk> {
            let mut chunks = vec![StreamChunk::BlockStart {
                index: 0,
                block_type: BlockType::Text,
            }];
            chunks.extend(
                self.fragments
                    .iter()
                    .map(|fragment| StreamChunk::TextDelta {
                        index: 0,
                        delta: (*fragment).to_string(),
                    }),
            );
            chunks.push(StreamChunk::BlockEnd {
                index: 0,
                block: ContentBlock::Text {
                    text: self.fragments.concat(),
                },
            });
            chunks.push(StreamChunk::Usage {
                usage: Usage::default(),
            });
            chunks.push(StreamChunk::Finish {
                reason: FinishReason::Stop,
            });
            chunks
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for FixedStreamProvider {
        fn name(&self) -> &str {
            "fixed-stream"
        }

        async fn generate_full(
            &self,
            _request: &GenerateRequest,
        ) -> Result<GenerateResult, ProviderError> {
            Err(ProviderError::Request(
                "this provider only scripts the streaming path".to_string(),
            ))
        }

        async fn generate_stream(
            &self,
            _request: &GenerateRequest,
        ) -> Result<GenerateStream, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(GenerateStream::new(Box::pin(futures::stream::iter(
                self.script().into_iter().map(Ok),
            ))))
        }
    }

    /// `stream(true)` 是唯一会发 `TextDelta` 的路径，故这条用例必须自建 agent。
    fn streaming_machine(
        fragments: Vec<&'static str>,
    ) -> (Arc<AgentMachine>, Arc<FixedStreamProvider>) {
        let profile: AgentProfile = serde_yaml::from_str(
            "agent:\n  name: t\n  description: d\nllm:\n  provider: p\n  model: m\n",
        )
        .expect("the inline profile parses");
        let executor: Arc<dyn crate::tools::ToolExecutor> =
            Arc::new(crate::tools::DefaultToolsExecutor::new(Vec::new()));
        let provider = Arc::new(FixedStreamProvider::new(fragments));
        let agent = Arc::new(Agent::from_parts(
            std::path::PathBuf::from("/tmp/agent.md"),
            profile,
            "sys".to_string(),
            Arc::clone(&provider) as Arc<dyn ModelProvider>,
            ModelConfigBuilder::new().stream(true).build(),
            Arc::clone(&executor),
            Arc::new(crate::mcp::McpManager::empty(executor)),
            None,
        ));
        (machine_for(agent, AgentMachineConfig::default()), provider)
    }

    async fn submit(
        hub: &TaskHub,
        machine: &AgentMachine,
        input: TaggedJson,
        deadline: Option<DateTime<Utc>>,
    ) -> Result<TaskId, MachineError> {
        hub.submit(&machine.descriptor().kind, task_with(input, deadline))
            .await
    }

    /// 终态只从快照读：`await_terminal` 已处理有损通道下的重订阅。
    async fn terminal(hub: &TaskHub, id: &TaskId) -> TaskSnapshot {
        hub.await_terminal(id, None)
            .await
            .expect("the task reaches a terminal state")
    }

    /// 读到终帧为止的全部 `Update`：终态之后 Hub 只发 `End`，读不到就是没收敛。
    async fn updates_to_end(mut stream: ObsStream) -> Vec<Update> {
        let mut updates = Vec::new();
        while let Some(event) = stream.next().await {
            match event {
                ObsEvent::Snapshot(_) => {}
                ObsEvent::Update(update) => updates.push(update),
                ObsEvent::Lagged { .. } | ObsEvent::End => break,
            }
        }
        updates
    }

    fn is_terminal_status(update: &Update) -> bool {
        matches!(&update.body, UpdateBody::Status { state, .. } if state.is_terminal())
    }

    fn terminal_update(updates: &[Update]) -> &Update {
        updates
            .iter()
            .rev()
            .find(|update| is_terminal_status(update))
            .expect("the stream carried no terminal status update")
    }

    fn state_of(update: &Update) -> TaskState {
        match &update.body {
            UpdateBody::Status { state, .. } => *state,
            other => panic!("expected a status update, got {other:?}"),
        }
    }

    fn error_kind(snapshot: &TaskSnapshot) -> &str {
        snapshot
            .outcome
            .as_ref()
            .expect("a terminal snapshot carries an outcome")
            .error
            .as_ref()
            .expect("a failed snapshot carries an error")
            .kind
            .as_str()
    }

    /// T1：成功轮的文本只来自 `TurnComplete`，不是拼出来的增量流。
    #[tokio::test]
    async fn turn_complete_settles_completed() {
        within_budget(async {
            let (machine, _provider) = scripted_machine(vec![completed("hello")]);
            let hub = hub_for(machine.clone(), hub_config());
            let id = submit(&hub, &machine, prompt("hi"), None)
                .await
                .expect("the task is accepted");

            let snapshot = terminal(&hub, &id).await;
            assert_eq!(snapshot.state, TaskState::Completed);
            let outcome = snapshot
                .outcome
                .expect("a terminal snapshot carries an outcome");
            assert_eq!(outcome.output, Some(text("hello")));
            assert!(outcome.error.is_none());
            assert!(snapshot.updated_at >= snapshot.created_at);
            // batch 路径整段文本只在 `TurnComplete` 交付，looper 一个 delta 都不发，
            // 故本机从未写过 output chunk：窗口必须停在原点、不留半句增量。写成非零
            // 就说明实现自己从终态文本伪造了增量。
            assert!(
                snapshot.output.tail.is_empty(),
                "the batch path must not fabricate output chunks"
            );
            assert_eq!(snapshot.output.start_offset, 0);
            assert_eq!(snapshot.output.end_offset, 0);
        })
        .await;
    }

    /// 追加用例（T1–T8 之外）：流式路径的增量必须真落到 Hub 的产出窗口里。
    #[tokio::test]
    async fn streaming_deltas_reach_the_hub_tail() {
        within_budget(async {
            let (machine, provider) = streaming_machine(vec!["he", "llo"]);
            let hub = hub_for(machine.clone(), hub_config());
            let id = submit(&hub, &machine, prompt("hi"), None)
                .await
                .expect("the task is accepted");

            // 增量由本机在写终态之前落写，故终态快照里的窗口已经定型。
            let snapshot = terminal(&hub, &id).await;
            assert_eq!(snapshot.state, TaskState::Completed);
            assert_eq!(
                snapshot.output.tail.as_slice(),
                b"hello",
                "the deltas must land in the hub window in order"
            );
            assert_eq!(snapshot.output.start_offset, 0);
            assert_eq!(
                snapshot.output.end_offset, 5,
                "the window must have advanced by the bytes actually written"
            );
            let outcome = snapshot
                .outcome
                .expect("a terminal snapshot carries an outcome");
            assert_eq!(outcome.output, Some(text("hello")));
            assert_eq!(provider.calls(), 1, "one turn makes exactly one model call");
        })
        .await;
    }

    /// T2：一轮里工具调用不产生终态；终态文本是最终回答，不是工具输出。
    #[tokio::test]
    async fn tool_round_trip_settles_exactly_once() {
        within_budget(async {
            let gate = GateTool::new("echo", Content::Text("x".to_string()));
            let entered = Arc::clone(&gate.entered);
            let release = Arc::clone(&gate.release);
            let (agent, _provider) = scripted_agent_with_tools(
                vec![
                    tool_call_response("c1", "echo", serde_json::json!({})),
                    completed("done"),
                ],
                vec![Box::new(gate)],
            );
            let machine = machine_for(agent, AgentMachineConfig::default());
            let hub = hub_for(machine.clone(), hub_config());
            let id = submit(&hub, &machine, prompt("hi"), None)
                .await
                .expect("the task is accepted");

            // 订阅必须落在终态之前：脚本化 provider 一轮可能几微秒跑完，门闸卡住工具
            // 才能保证「已订阅、未终态」这个窗口存在，否则终态更新会漏在订阅之前。
            let stream = hub.subscribe(&id).expect("subscribe");
            entered.notified().await;
            release.notify_one();

            let updates = updates_to_end(stream).await;
            let terminal: Vec<&Update> = updates.iter().filter(|u| is_terminal_status(u)).collect();
            assert_eq!(terminal.len(), 1, "one task writes its terminal state once");
            assert_eq!(state_of(terminal[0]), TaskState::Completed);
            assert_eq!(
                terminal[0].origin,
                UpdateOrigin::Machine,
                "the machine must settle its terminal state itself, not the hub fallback"
            );

            let snapshot = hub.snapshot(&id).await.expect("the snapshot is readable");
            assert_eq!(snapshot.state, TaskState::Completed);
            let outcome = snapshot
                .outcome
                .expect("a terminal snapshot carries an outcome");
            assert_eq!(
                outcome.output.expect("successful outcome carries output"),
                text("done")
            );
        })
        .await;
    }

    /// T3：取消在 machine 自己的宽限窗内收敛，终态归属 `Machine` 而不是 Hub 兜底。
    #[tokio::test]
    async fn cancel_converges_inside_the_machine_grace() {
        within_budget(async {
            let gate = GateTool::new("echo", Content::Text("x".to_string()));
            let entered = Arc::clone(&gate.entered);
            let (agent, _provider) = scripted_agent_with_tools(
                vec![
                    tool_call_response("c1", "echo", serde_json::json!({})),
                    completed("done"),
                ],
                vec![Box::new(gate)],
            );
            // 宽限窗必须明显小于两处：Hub 的 cancel_grace（否则终态由 Hub 兜底写出），
            // 以及 looper 的 200ms 步进边界（它在边界上自己就能收敛，宽松的阈值会让
            // 「有没有宽限兜底」在耗时上不可分辨，断言也就不会红）。
            let machine = machine_for(
                agent,
                AgentMachineConfig {
                    cancel_grace: Duration::from_millis(50),
                    ..AgentMachineConfig::default()
                },
            );
            let hub = hub_for(
                machine.clone(),
                HubConfig {
                    cancel_grace: Duration::from_millis(500),
                    ..hub_config()
                },
            );
            let id = submit(&hub, &machine, prompt("hi"), None)
                .await
                .expect("the task is accepted");

            let stream = hub.subscribe(&id).expect("subscribe");
            entered.notified().await;
            let started = Instant::now();
            hub.cancel(&id).await.expect("cancel is accepted");
            let updates = updates_to_end(stream).await;
            let elapsed = started.elapsed();

            let last = terminal_update(&updates);
            assert_eq!(
                last.origin,
                UpdateOrigin::Machine,
                "cancel must converge inside the machine"
            );
            assert_eq!(
                state_of(last),
                TaskState::Canceled,
                "a cancelled turn is Canceled, not Failed"
            );
            assert!(
                elapsed < Duration::from_millis(150),
                "cancel converged in {elapsed:?}"
            );

            let snapshot = hub.snapshot(&id).await.expect("the snapshot is readable");
            assert_eq!(error_kind(&snapshot), "canceled");
        })
        .await;
    }

    /// T4：deadline 由 driver 自己收敛，且真的停住了在途轮。
    #[tokio::test]
    async fn deadline_converges_before_the_hub_grace() {
        within_budget(async {
            let gate = GateTool::new("echo", Content::Text("x".to_string()));
            let release = Arc::clone(&gate.release);
            let (agent, provider) = scripted_agent_with_tools(
                vec![
                    tool_call_response("c1", "echo", serde_json::json!({})),
                    completed("done"),
                ],
                vec![Box::new(gate)],
            );
            let machine = machine_for(agent, AgentMachineConfig::default());
            // terminal_grace 拉到 5s：Hub 的兜底够不着，只有 driver 自己收敛才算过。
            let hub = hub_for(
                machine.clone(),
                HubConfig {
                    terminal_grace: Duration::from_secs(5),
                    ..hub_config()
                },
            );
            let started = Instant::now();
            let id = submit(
                &hub,
                &machine,
                prompt("hi"),
                Some(Utc::now() + chrono::TimeDelta::milliseconds(50)),
            )
            .await
            .expect("the task is accepted");

            let stream = hub.subscribe(&id).expect("subscribe");
            let updates = updates_to_end(stream).await;
            let elapsed = started.elapsed();

            let last = terminal_update(&updates);
            assert_eq!(
                last.origin,
                UpdateOrigin::Machine,
                "the deadline must be settled by the machine"
            );
            assert_eq!(state_of(last), TaskState::TimedOut);
            assert!(
                elapsed < Duration::from_secs(1),
                "deadline converged in {elapsed:?}"
            );

            let snapshot = hub.snapshot(&id).await.expect("the snapshot is readable");
            assert_eq!(error_kind(&snapshot), "deadline");

            // deadline 分支的显式 cancel 与 drop 安全网语义等价（都置同一取消闩），
            // 故单独删任一路不可观测；本条钉的是「终态之后不再有模型调用」，两路同时
            // 失效时才变红。
            release.notify_one();
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert_eq!(
                provider.budgets().len(),
                1,
                "the deadline must stop the in-flight turn, not only write a terminal state"
            );
        })
        .await;
    }

    /// 追加用例（T1–T8 之外）：提交时 deadline 已经过期，必须走 `drive` 的入口判定。
    ///
    /// 删掉入口短路后，瞬时轮（脚本化 provider 几微秒跑完）会先完成并落 `Completed`，
    /// 故本条钉的是入口分支本身，而不是同一终态的耗时差异。
    #[tokio::test]
    async fn already_expired_deadline_settles_timed_out() {
        within_budget(async {
            let (machine, _provider) = scripted_machine(vec![completed("hello")]);
            let hub = hub_for(machine.clone(), hub_config());
            let id = submit(
                &hub,
                &machine,
                prompt("hi"),
                Some(Utc::now() - chrono::TimeDelta::milliseconds(1)),
            )
            .await
            .expect("the task is accepted");

            let snapshot = terminal(&hub, &id).await;
            assert_eq!(snapshot.state, TaskState::TimedOut);
            assert_eq!(error_kind(&snapshot), "deadline");
        })
        .await;
    }

    /// T5：输入契约非法时零残留 —— 没有条目、没有模型调用。
    #[tokio::test]
    async fn illegal_input_leaves_no_trace() {
        within_budget(async {
            let (machine, provider) = scripted_machine(vec![completed("hello")]);
            let hub = hub_for(machine.clone(), hub_config());
            // 拿一个合法 id，但输入不是 prompt。
            let id = submit(&hub, &machine, text("hi"), None)
                .await
                .expect("the hub accepts the task before the machine looks at it");

            let snapshot = terminal(&hub, &id).await;
            assert_eq!(snapshot.state, TaskState::Failed);
            let error = snapshot
                .outcome
                .expect("a terminal snapshot carries an outcome")
                .error
                .expect("a failed snapshot carries an error");
            assert_eq!(error.kind, "impl_error");
            assert!(
                error.message.contains("must carry tag"),
                "the reason states the input contract: {}",
                error.message
            );

            assert_eq!(
                machine.snapshot(&id).await,
                Err(MachineError::NotFound { id }),
                "a rejected submission must not leave a run entry"
            );
            assert!(
                provider.budgets().is_empty(),
                "no model call may be issued for a rejected input"
            );
        })
        .await;
    }

    /// T6：终态后的取消是幂等成功，不产生新的 seq。
    #[tokio::test]
    async fn cancel_after_terminal_is_idempotent() {
        within_budget(async {
            let (machine, _provider) = scripted_machine(vec![completed("hello")]);
            let hub = hub_for(machine.clone(), hub_config());
            let id = submit(&hub, &machine, prompt("hi"), None)
                .await
                .expect("the task is accepted");
            terminal(&hub, &id).await;

            // seq 只在 Hub 侧推进：machine 的快照把它固定填 0，读那边恒真。
            let before = hub.snapshot(&id).await.expect("the snapshot is readable");
            assert!(
                before.last_seq > 0,
                "the hub snapshot must carry a live seq counter"
            );
            machine
                .cancel(&id)
                .await
                .expect("cancel after a terminal state stays Ok");
            hub.cancel(&id)
                .await
                .expect("hub cancel after a terminal state stays Ok");
            let after = hub.snapshot(&id).await.expect("the snapshot is readable");

            assert_eq!(
                after.last_seq, before.last_seq,
                "cancel after a terminal state must not emit an update"
            );
            assert_eq!(after.state, TaskState::Completed);
        })
        .await;
    }

    /// 追加用例（T1–T8 之外）：重复取消不得延长宽限窗。
    #[tokio::test]
    async fn repeated_cancel_does_not_extend_the_grace() {
        within_budget(async {
            let gate = GateTool::new("echo", Content::Text("x".to_string()));
            let entered = Arc::clone(&gate.entered);
            let (agent, _provider) = scripted_agent_with_tools(
                vec![
                    tool_call_response("c1", "echo", serde_json::json!({})),
                    completed("done"),
                ],
                vec![Box::new(gate)],
            );
            // 本机宽限 100ms、Hub 兜底 500ms：第二次取消若重新武装，终态会被推到
            // ~160ms，只有「只认首次」的实现才收敛在 130ms 内。
            let machine = machine_for(
                agent,
                AgentMachineConfig {
                    cancel_grace: Duration::from_millis(100),
                    ..AgentMachineConfig::default()
                },
            );
            let hub = hub_for(
                machine.clone(),
                HubConfig {
                    cancel_grace: Duration::from_millis(500),
                    ..hub_config()
                },
            );
            let id = submit(&hub, &machine, prompt("hi"), None)
                .await
                .expect("the task is accepted");

            let stream = hub.subscribe(&id).expect("subscribe");
            entered.notified().await;
            let started = Instant::now();
            hub.cancel(&id).await.expect("the first cancel is accepted");
            tokio::time::sleep(Duration::from_millis(60)).await;
            hub.cancel(&id)
                .await
                .expect("the repeated cancel is accepted");
            let updates = updates_to_end(stream).await;
            let elapsed = started.elapsed();

            let last = terminal_update(&updates);
            assert_eq!(
                last.origin,
                UpdateOrigin::Machine,
                "the machine's own grace must settle the task"
            );
            assert_eq!(state_of(last), TaskState::Canceled);
            assert!(
                elapsed < Duration::from_millis(130),
                "the repeated cancel restarted the grace window: converged in {elapsed:?}"
            );
        })
        .await;
    }

    /// T7：没提交过的 id 是 `NotFound`，不是伪造的空快照。
    #[tokio::test]
    async fn snapshot_of_unknown_id_is_not_found() {
        within_budget(async {
            let (machine, _provider) = scripted_machine(vec![completed("hello")]);
            let unknown = TaskId(Uuid::new_v4());
            assert_eq!(
                machine.snapshot(&unknown).await,
                Err(MachineError::NotFound { id: unknown })
            );
        })
        .await;
    }

    /// 追加用例（T1–T8 之外）：`TurnComplete{Failed{Cancelled}}` 必须落成 `Canceled`。
    ///
    /// T3 的宽限窗总是先于 looper 的 ≤200ms 步进边界落地，因此这条映射在 T3 里
    /// 永远走不到；这里把宽限窗放大、并在取消后放行工具，让 looper 自己收尾，专测它。
    #[tokio::test]
    async fn cancelled_turn_maps_to_canceled() {
        within_budget(async {
            let gate = GateTool::new("echo", Content::Text("x".to_string()));
            let entered = Arc::clone(&gate.entered);
            let release = Arc::clone(&gate.release);
            let (agent, _provider) = scripted_agent_with_tools(
                vec![
                    tool_call_response("c1", "echo", serde_json::json!({})),
                    completed("done"),
                ],
                vec![Box::new(gate)],
            );
            let machine = machine_for(agent, AgentMachineConfig::default());
            let hub = hub_for(machine.clone(), hub_config());
            let id = submit(&hub, &machine, prompt("hi"), None)
                .await
                .expect("the task is accepted");

            let stream = hub.subscribe(&id).expect("subscribe");
            entered.notified().await;
            let started = Instant::now();
            hub.cancel(&id).await.expect("cancel is accepted");
            // 放行工具：looper 在步进边界消费取消，用 `TurnComplete{Cancelled}` 收尾。
            release.notify_one();

            let updates = updates_to_end(stream).await;
            let elapsed = started.elapsed();
            let last = terminal_update(&updates);
            assert_eq!(last.origin, UpdateOrigin::Machine);
            assert_eq!(
                state_of(last),
                TaskState::Canceled,
                "a cancelled turn is Canceled, not Failed"
            );
            assert!(
                elapsed < Duration::from_millis(500),
                "the cancellation should come from the turn itself, took {elapsed:?}"
            );

            let snapshot = hub.snapshot(&id).await.expect("the snapshot is readable");
            assert_eq!(error_kind(&snapshot), "canceled");
        })
        .await;
    }

    /// T8：driver 退出后控制口必须摘掉，且不再产生模型调用。
    #[tokio::test]
    async fn driver_retires_its_control_channel() {
        within_budget(async {
            let (machine, provider) = scripted_machine(vec![completed("hello")]);
            let hub = hub_for(machine.clone(), hub_config());
            let id = submit(&hub, &machine, prompt("hi"), None)
                .await
                .expect("the task is accepted");
            terminal(&hub, &id).await;

            // 终态先落 Hub，machine 摘控制口在 `finish` 返回之后：留出这段间隙。
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(
                !machine.active_controls().contains(&id),
                "a run whose driver exited must not look alive"
            );
            assert_eq!(
                provider.budgets().len(),
                1,
                "a retired driver must not issue further model calls"
            );
        })
        .await;
    }

    /// 默认 kind 不变：既有提交路径用的就是这个字面量，改了会让所有已注册的
    /// Hub 路由键一起漂移。
    #[test]
    fn default_descriptor_kind_is_agent() {
        let (agent, _provider) = scripted_agent(vec![]);
        assert_eq!(
            AgentMachine::new(Arc::clone(&agent), AgentMachineConfig::default())
                .descriptor()
                .kind,
            "agent"
        );

        let named = AgentMachineConfig {
            kind: "agent:t".to_string(),
            ..AgentMachineConfig::default()
        };
        assert_eq!(
            AgentMachine::new(agent, named).descriptor().kind,
            "agent:t",
            "the descriptor must read the configured kind"
        );
    }

    /// 声明已从工具结果正文里剥离：不写进 sink 就永久丢失，故这里钉住「声明进了
    /// Hub 的快照，且能按 id 还原成原声明」。
    #[tokio::test]
    async fn tool_artifact_declarations_reach_the_hub_snapshot() {
        within_budget(async {
            let output = "ok\n@@PECO_ARTIFACT {\"path\":\"/ws/output/a.jpg\",\"title\":\"fox\"}\n\
                          @@PECO_ARTIFACT {\"path\":\"\"}\n";
            let tool = StaticTool::new("emit", Content::Text(output.to_string()));
            let (agent, _provider) = scripted_agent_with_tools(
                vec![
                    tool_call_response("c1", "emit", serde_json::json!({})),
                    completed("done"),
                ],
                vec![Box::new(tool)],
            );
            let machine = machine_for(agent, AgentMachineConfig::default());
            let hub = hub_for(machine.clone(), hub_config());
            let id = submit(&hub, &machine, prompt("hi"), None)
                .await
                .expect("the task is accepted");

            let snapshot = terminal(&hub, &id).await;
            assert_eq!(snapshot.state, TaskState::Completed);
            assert_eq!(
                snapshot.artifacts.len(),
                1,
                "a declaration without a path must be dropped"
            );

            let artifact = &snapshot.artifacts[0];
            assert_eq!(artifact.id, ArtifactId("/ws/output/a.jpg".to_string()));
            assert_eq!(artifact.kind, ARTIFACT_DECL_TAG);
            assert_eq!(artifact.payload.tag, ARTIFACT_DECL_TAG);
            assert!(artifact.last_chunk, "one declaration is a complete value");
            assert!(
                artifact.offset > 0,
                "the hub recomputes the offset from the payload"
            );

            let restored: crate::artifacts::ArtifactDecl =
                serde_json::from_value(artifact.payload.payload.clone())
                    .expect("the payload round-trips to the declaration");
            assert_eq!(restored.path, "/ws/output/a.jpg");
            assert_eq!(restored.title.as_deref(), Some("fox"));
        })
        .await;
    }

    /// 声明数没有天然上界：跑飞的子 agent 可以一次吐出上万行，故按任务截断。
    #[tokio::test]
    async fn artifact_declarations_are_capped_per_task() {
        within_budget(async {
            let overflow = MAX_ARTIFACTS_PER_TASK + 1;
            let mut output = String::from("ok\n");
            for index in 0..overflow {
                output.push_str(&format!(
                    "@@PECO_ARTIFACT {{\"path\":\"/tmp/{index}.jpg\"}}\n"
                ));
            }
            let tool = StaticTool::new("emit", Content::Text(output));
            let (agent, _provider) = scripted_agent_with_tools(
                vec![
                    tool_call_response("c1", "emit", serde_json::Value::Null),
                    completed("done"),
                ],
                vec![Box::new(tool)],
            );
            let machine = machine_for(agent, AgentMachineConfig::default());
            let hub = hub_for(machine.clone(), hub_config());
            let id = submit(&hub, &machine, prompt("hi"), None)
                .await
                .expect("the task is accepted");

            let snapshot = terminal(&hub, &id).await;
            assert_eq!(snapshot.state, TaskState::Completed);
            assert_eq!(
                snapshot.artifacts.len(),
                MAX_ARTIFACTS_PER_TASK,
                "declarations beyond the per-task cap must be dropped"
            );
        })
        .await;
    }
}
