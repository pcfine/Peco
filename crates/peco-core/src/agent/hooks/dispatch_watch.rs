// ============================================================================
// dispatch_watch — B 类观察：派发之后持续观察目标空间里的任务
// ============================================================================
//
// `dispatch_to_workspace` 只是提交：任务会在目标 workspace 的 Hub 里继续运行，
// 发起方 turn 结束后仍然活着。本 hook 在工具返回时解析回执，另起一个后台任务
// 订阅目标任务，把注册 / 增量 / 终态 / 失联翻译成 [`TaskObservationSink`] 调用。
//
// 生命周期与 turn 解耦：watcher 是独立 `tokio::spawn` 的任务，不随 turn 取消而
// 停止 —— 这正是「B 类观察」的语义。它只持 `Weak<WorkspaceHub>`，且在任何
// `stream.next().await` 期间都不持有 `Arc<WorkspaceHub>` / `Arc<TaskHub>`，
// 否则空间关闭时 Hub 会被观察者钉住，实例永远释放不了。

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use model_provider::ToolCall;
use tracing::warn;
use uuid::Uuid;

use super::LooperHook;
use crate::agent::MASTER_AGENT_NAME;
use crate::machine::{ObsEvent, ObsStream, TaskId, TaskSnapshot};
use crate::tools::{DispatchReceipt, WorkspaceHub, WorkspaceToolError};

/// 被观察的工具名，同时作为观察记录的 `origin_kind`。
pub(crate) const DISPATCH_TOOL_NAME: &str = "dispatch_to_workspace";
/// `Lagged` / 非终态 `End` 触发的重订阅上限；超过即认为任务已不可观察。
const MAX_RESUBSCRIBES: usize = 3;

// ============================================================================
// 观察载荷
// ============================================================================

/// 派发来源：谁、在哪个 workspace 发起的这次派发。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchOrigin {
    pub ws_id: String,
    pub principal: String,
}

/// 一次派发观察的身份：目标目录 + 任务 id 是重订阅与事后查询的唯一句柄。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchObservation {
    pub origin: DispatchOrigin,
    /// 目标 workspace 目录（派发回执里的 workspace，已规范化）。
    pub target_dir: PathBuf,
    pub task_id: TaskId,
    pub dispatch_id: Option<String>,
    /// 固定 "dispatch_to_workspace"；为将来 run_machine_task 观察留位。
    pub origin_kind: String,
    /// 目标 agent，dispatch 恒为 @master。
    pub agent_name: String,
}

// ============================================================================
// Sink
// ============================================================================

/// 观察结果接收方：core 只定义事件，落库 / 唤醒 / 审计由宿主实现。
///
/// 所有方法都是通知，失败不得反向中止 looper；实现应自行吞掉错误。
#[async_trait]
pub trait TaskObservationSink: Send + Sync {
    /// 首帧快照。实现应幂等（同一 task 可能因重订阅再次到达）。
    async fn on_registered(&self, _obs: &DispatchObservation, _snapshot: &TaskSnapshot) {}

    /// 状态以外的增量（Update）与重订阅事件。
    async fn on_event(&self, _obs: &DispatchObservation, _event: &ObsEvent) {}

    /// 终态快照（每个任务至多一次；重订阅后重复到达由 sink 幂等处理）。
    async fn on_terminal(&self, _obs: &DispatchObservation, _snapshot: &TaskSnapshot) {}

    /// 任务已不可观察：空间被关闭、Hub 消失、任务不存在、重订阅耗尽。
    async fn on_orphaned(&self, _obs: &DispatchObservation, _reason: &str) {}
}

// ============================================================================
// Hook
// ============================================================================

/// `on_after_tool` 观察者：只认 `dispatch_to_workspace` 的成功回执。
///
/// hook 本身立即返回；实际观察在独立任务里进行，任何解析失败都静默忽略，
/// 绝不对 looper 造成影响。
pub struct DispatchWatchHook {
    hub: Weak<WorkspaceHub>,
    origin: DispatchOrigin,
    sink: Arc<dyn TaskObservationSink>,
}

impl DispatchWatchHook {
    pub fn new(
        hub: Weak<WorkspaceHub>,
        origin: DispatchOrigin,
        sink: Arc<dyn TaskObservationSink>,
    ) -> Self {
        Self { hub, origin, sink }
    }
}

#[async_trait]
impl LooperHook for DispatchWatchHook {
    async fn on_after_tool(
        &self,
        _turn_index: usize,
        tool_call: &ToolCall,
        result: &str,
        is_error: bool,
    ) {
        if is_error || tool_call.function.name != DISPATCH_TOOL_NAME {
            return;
        }
        let Ok(receipt) = serde_json::from_str::<DispatchReceipt>(result) else {
            return;
        };
        let Ok(task_id) = Uuid::parse_str(&receipt.task_id) else {
            return;
        };
        let target = receipt.workspace.trim();
        if target.is_empty() {
            return;
        }
        let target_dir = std::fs::canonicalize(target).unwrap_or_else(|_| PathBuf::from(target));

        let observation = DispatchObservation {
            origin: self.origin.clone(),
            target_dir,
            task_id: TaskId(task_id),
            dispatch_id: (!receipt.dispatch_id.is_empty()).then_some(receipt.dispatch_id),
            origin_kind: DISPATCH_TOOL_NAME.to_string(),
            agent_name: MASTER_AGENT_NAME.to_string(),
        };

        // hook 在 looper 的 async context 内被调用；没有运行时（极端测试上下文）
        // 时宁可放弃观察，也不能 panic 掉整个 looper。
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            warn!(
                task_id = %observation.task_id,
                "no tokio runtime; dispatch observation skipped"
            );
            return;
        };
        runtime.spawn(watch(self.hub.clone(), observation, Arc::clone(&self.sink)));
    }
}

// ============================================================================
// Watcher
// ============================================================================

async fn watch(
    hub: Weak<WorkspaceHub>,
    obs: DispatchObservation,
    sink: Arc<dyn TaskObservationSink>,
) {
    let mut resubscribes = 0usize;

    'observe: loop {
        // 块作用域只包住订阅动作：`ObsStream` 不持有 Hub 强引用，
        // `stream.next().await` 期间不会有观察者把空间钉住。
        let mut stream = match subscribe(&hub, &obs).await {
            Ok(stream) => stream,
            Err(err) => {
                warn!(
                    task_id = %obs.task_id,
                    error = %err,
                    "dispatch observation could not subscribe"
                );
                sink.on_orphaned(&obs, &orphan_reason(&obs, &err)).await;
                return;
            }
        };

        while let Some(event) = stream.next().await {
            match event {
                ObsEvent::Snapshot(snapshot) => {
                    sink.on_registered(&obs, &snapshot).await;
                    if snapshot.state.is_terminal() {
                        sink.on_terminal(&obs, &snapshot).await;
                        return;
                    }
                }
                ObsEvent::Update(update) => {
                    sink.on_event(&obs, &ObsEvent::Update(update)).await;
                }
                ObsEvent::Lagged { dropped } => {
                    warn!(
                        task_id = %obs.task_id,
                        dropped,
                        "dispatch observation lagged; resubscribing"
                    );
                    resubscribes += 1;
                    if resubscribes > MAX_RESUBSCRIBES {
                        sink.on_orphaned(&obs, &exhausted_reason(&obs)).await;
                        return;
                    }
                    continue 'observe;
                }
                ObsEvent::End => break,
            }
        }
        drop(stream);

        // `End` / `None`：终态只认快照，有损通道的结束帧不是结论。
        match snapshot_now(&hub, &obs).await {
            Ok(snapshot) if snapshot.state.is_terminal() => {
                sink.on_terminal(&obs, &snapshot).await;
                return;
            }
            Ok(_) => {
                resubscribes += 1;
                if resubscribes > MAX_RESUBSCRIBES {
                    sink.on_orphaned(&obs, &exhausted_reason(&obs)).await;
                    return;
                }
            }
            Err(err) => {
                sink.on_orphaned(&obs, &orphan_reason(&obs, &err)).await;
                return;
            }
        }
    }
}

/// upgrade 后立刻订阅并返回流；`Arc<WorkspaceHub>` 在本函数返回时即释放。
async fn subscribe(
    hub: &Weak<WorkspaceHub>,
    obs: &DispatchObservation,
) -> Result<ObsStream, WorkspaceToolError> {
    let hub = hub.upgrade().ok_or(WorkspaceToolError::HubGone)?;
    hub.subscribe_task(&obs.target_dir, obs.task_id).await
}

/// 同上：快照读取不跨 `next().await`，只做一次短命 upgrade。
async fn snapshot_now(
    hub: &Weak<WorkspaceHub>,
    obs: &DispatchObservation,
) -> Result<TaskSnapshot, WorkspaceToolError> {
    let hub = hub.upgrade().ok_or(WorkspaceToolError::HubGone)?;
    hub.task_snapshot(&obs.target_dir, obs.task_id).await
}

fn orphan_reason(obs: &DispatchObservation, err: &WorkspaceToolError) -> String {
    format!(
        "cannot observe task {} in '{}': {}",
        obs.task_id,
        obs.target_dir.display(),
        err
    )
}

fn exhausted_reason(obs: &DispatchObservation) -> String {
    format!(
        "task {} in '{}' stayed unobservable after {MAX_RESUBSCRIBES} resubscriptions",
        obs.task_id,
        obs.target_dir.display()
    )
}

// ============================================================================
// 测试 — MachineRuntime + EchoMachine，零真实模型调用
// ============================================================================

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use chrono::Utc;
    use serde_json::json;
    use tokio::sync::{Notify, mpsc};

    use super::*;
    use crate::agent::AgentError;
    use crate::machine::echo::doubles::ScriptedMachine;
    use crate::machine::{
        EchoMachine, HubConfig, MachineRuntime, TaggedJson, Task, TaskContext, TaskSource,
        TaskState,
    };
    use crate::tools::WorkspaceHandle;
    use crate::workspace::WorkSpace;

    const BUDGET: Duration = Duration::from_secs(3);

    fn test_runtime() -> Arc<MachineRuntime> {
        Arc::new(MachineRuntime::with_hub_config(HubConfig {
            emit_min_interval: Duration::ZERO,
            scan_interval: Duration::from_millis(10),
            terminal_grace: Duration::from_millis(50),
            cancel_grace: Duration::from_millis(50),
            ..HubConfig::default()
        }))
    }

    fn task(text: &str) -> Task {
        Task {
            // Hub 会覆盖 id 与 kind。
            id: TaskId(Uuid::nil()),
            kind: String::new(),
            input: TaggedJson {
                tag: "text".to_string(),
                payload: json!(text),
            },
            context: TaskContext {
                scope: None,
                data: None,
            },
            source: TaskSource::External {
                principal: "alice".to_string(),
            },
            deadline: None,
            labels: BTreeMap::new(),
            created_at: Utc::now(),
        }
    }

    /// 最小 handle：观察只用到 `machine_runtime`，其余能力保持不可达。
    struct FakeHandle {
        runtime: Arc<MachineRuntime>,
    }

    impl WorkspaceHandle for FakeHandle {
        fn workspace(&self) -> Arc<WorkSpace> {
            unimplemented!("observation tests need no real WorkSpace")
        }

        fn user_id(&self) -> &str {
            "alice"
        }

        fn master_agent(&self) -> Result<Arc<crate::agent::Agent>, AgentError> {
            Err(AgentError::Config(
                "fake handle has no loaded master".to_string(),
            ))
        }

        fn machine_runtime(&self) -> Weak<MachineRuntime> {
            Arc::downgrade(&self.runtime)
        }
    }

    fn open_hub(dir: &std::path::Path, runtime: Arc<MachineRuntime>) -> Arc<WorkspaceHub> {
        let hub = Arc::new(WorkspaceHub::new());
        hub.insert_for_test(dir, Arc::new(FakeHandle { runtime }));
        hub
    }

    async fn submit(runtime: &Arc<MachineRuntime>, kind: &str, text: &str) -> TaskId {
        runtime
            .hub()
            .expect("hub is available inside a runtime")
            .submit(kind, task(text))
            .await
            .expect("submit")
    }

    fn receipt(workspace: &std::path::Path, task_id: TaskId) -> DispatchReceipt {
        DispatchReceipt {
            dispatch_id: "dispatch-1".to_string(),
            task_id: task_id.to_string(),
            workspace: workspace.display().to_string(),
            state: "Submitted".to_string(),
            output: None,
            timed_out: false,
        }
    }

    fn hook(hub: &Arc<WorkspaceHub>, sink: Arc<dyn TaskObservationSink>) -> DispatchWatchHook {
        DispatchWatchHook::new(
            Arc::downgrade(hub),
            DispatchOrigin {
                ws_id: "ws_origin".to_string(),
                principal: "alice".to_string(),
            },
            sink,
        )
    }

    fn dispatch_call() -> ToolCall {
        ToolCall::new("call-1", DISPATCH_TOOL_NAME, "{}")
    }

    #[derive(Debug)]
    enum Recorded {
        Registered(DispatchObservation, TaskSnapshot),
        Terminal(TaskSnapshot),
        Orphaned(DispatchObservation, String),
        Event,
    }

    struct RecordingSink {
        tx: mpsc::UnboundedSender<Recorded>,
    }

    #[async_trait]
    impl TaskObservationSink for RecordingSink {
        async fn on_registered(&self, obs: &DispatchObservation, snapshot: &TaskSnapshot) {
            let _ = self
                .tx
                .send(Recorded::Registered(obs.clone(), snapshot.clone()));
        }

        async fn on_event(&self, _obs: &DispatchObservation, _event: &ObsEvent) {
            let _ = self.tx.send(Recorded::Event);
        }

        async fn on_terminal(&self, _obs: &DispatchObservation, snapshot: &TaskSnapshot) {
            let _ = self.tx.send(Recorded::Terminal(snapshot.clone()));
        }

        async fn on_orphaned(&self, obs: &DispatchObservation, reason: &str) {
            let _ = self
                .tx
                .send(Recorded::Orphaned(obs.clone(), reason.to_string()));
        }
    }

    fn sink() -> (
        Arc<dyn TaskObservationSink>,
        mpsc::UnboundedReceiver<Recorded>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Arc::new(RecordingSink { tx }), rx)
    }

    async fn next(rx: &mut mpsc::UnboundedReceiver<Recorded>) -> Recorded {
        tokio::time::timeout(BUDGET, rx.recv())
            .await
            .expect("the sink must be notified within the budget")
            .expect("the sink channel stays open for the test")
    }

    /// 增量（`Update`）是允许穿插的：终态断言只跳过它们，不要求相邻。
    async fn next_terminal(rx: &mut mpsc::UnboundedReceiver<Recorded>) -> TaskSnapshot {
        loop {
            match next(rx).await {
                Recorded::Event => continue,
                Recorded::Terminal(snapshot) => return snapshot,
                other => panic!("expected on_terminal, got {other:?}"),
            }
        }
    }

    /// watcher 结束后 sink 的发送端必须全部释放：通道关闭是「任务真的退出」的证据。
    async fn assert_watcher_ended(rx: &mut mpsc::UnboundedReceiver<Recorded>) {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(None) => {}
            other => panic!("the watcher must end after reporting orphaned, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_finished_dispatch_reports_registered_then_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = test_runtime();
        runtime
            .hub()
            .expect("hub")
            .register(Arc::new(EchoMachine::new()))
            .expect("register echo");
        let id = submit(&runtime, "echo", "hello").await;
        let hub = open_hub(dir.path(), runtime);

        let (sink, mut rx) = sink();
        let payload = serde_json::to_string(&receipt(dir.path(), id)).unwrap();
        hook(&hub, sink)
            .on_after_tool(0, &dispatch_call(), &payload, false)
            .await;

        let first = next(&mut rx).await;
        let Recorded::Registered(obs, snapshot) = first else {
            panic!("the first notification must be on_registered, got {first:?}");
        };
        assert_eq!(obs.origin.ws_id, "ws_origin");
        assert_eq!(obs.origin.principal, "alice");
        assert_eq!(obs.task_id, id);
        assert_eq!(obs.dispatch_id.as_deref(), Some("dispatch-1"));
        assert_eq!(obs.origin_kind, DISPATCH_TOOL_NAME);
        assert_eq!(obs.agent_name, MASTER_AGENT_NAME);
        assert_eq!(
            obs.target_dir,
            std::fs::canonicalize(dir.path()).unwrap(),
            "the receipt workspace must be normalized"
        );
        assert!(
            matches!(
                snapshot.state,
                TaskState::Submitted | TaskState::Working | TaskState::Completed
            ),
            "the echo task must be in a live or terminal state, got {:?}",
            snapshot.state
        );

        let terminal = next_terminal(&mut rx).await;
        assert_eq!(terminal.state, TaskState::Completed);
        assert_eq!(
            terminal
                .outcome
                .and_then(|outcome| outcome.output)
                .and_then(|output| output.payload.as_str().map(str::to_string))
                .as_deref(),
            Some("hello"),
            "the terminal output must be readable from the sink"
        );
    }

    #[tokio::test]
    async fn a_running_dispatch_registers_first_and_terminates_later() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = test_runtime();
        let gate = Arc::new(Notify::new());
        runtime
            .hub()
            .expect("hub")
            .register(Arc::new(ScriptedMachine::new(
                "scripted",
                Arc::clone(&gate),
            )))
            .expect("register scripted");
        let id = submit(&runtime, "scripted", "gated").await;
        wait_for_working(&runtime, &id).await;
        let hub = open_hub(dir.path(), runtime);

        let (sink, mut rx) = sink();
        let payload = serde_json::to_string(&receipt(dir.path(), id)).unwrap();
        hook(&hub, sink)
            .on_after_tool(0, &dispatch_call(), &payload, false)
            .await;

        let Recorded::Registered(_, snapshot) = next(&mut rx).await else {
            panic!("a running task must still register first");
        };
        assert!(
            !snapshot.state.is_terminal(),
            "the task is gated, so the first snapshot cannot be terminal"
        );

        gate.notify_one();
        let terminal = next_terminal(&mut rx).await;
        assert_eq!(terminal.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn non_dispatch_tools_errors_and_bad_payloads_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = test_runtime();
        runtime
            .hub()
            .expect("hub")
            .register(Arc::new(EchoMachine::new()))
            .expect("register echo");
        let id = submit(&runtime, "echo", "hello").await;
        let hub = open_hub(dir.path(), runtime);

        struct CountingSink(AtomicUsize);

        #[async_trait]
        impl TaskObservationSink for CountingSink {
            async fn on_registered(&self, _: &DispatchObservation, _: &TaskSnapshot) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            async fn on_event(&self, _: &DispatchObservation, _: &ObsEvent) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            async fn on_terminal(&self, _: &DispatchObservation, _: &TaskSnapshot) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            async fn on_orphaned(&self, _: &DispatchObservation, _: &str) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let sink = Arc::new(CountingSink(AtomicUsize::new(0)));
        let hook = hook(&hub, sink.clone());
        let payload = serde_json::to_string(&receipt(dir.path(), id)).unwrap();

        // 非 dispatch 工具名。
        let other = ToolCall::new("c", "run_machine_task", "{}");
        hook.on_after_tool(0, &other, &payload, false).await;
        // 工具报错。
        hook.on_after_tool(0, &dispatch_call(), &payload, true)
            .await;
        // 坏 JSON / 缺 task_id。
        hook.on_after_tool(0, &dispatch_call(), "not json", false)
            .await;
        hook.on_after_tool(0, &dispatch_call(), r#"{"dispatch_id":"d"}"#, false)
            .await;
        // task_id 不是 uuid。
        let bad_id = DispatchReceipt {
            dispatch_id: "d".to_string(),
            task_id: "not-a-uuid".to_string(),
            workspace: dir.path().display().to_string(),
            state: "Submitted".to_string(),
            output: None,
            timed_out: false,
        };
        hook.on_after_tool(
            0,
            &dispatch_call(),
            &serde_json::to_string(&bad_id).unwrap(),
            false,
        )
        .await;
        // 空 workspace。
        let empty_ws = DispatchReceipt {
            dispatch_id: "d".to_string(),
            task_id: id.to_string(),
            workspace: "  ".to_string(),
            state: "Submitted".to_string(),
            output: None,
            timed_out: false,
        };
        hook.on_after_tool(
            0,
            &dispatch_call(),
            &serde_json::to_string(&empty_ws).unwrap(),
            false,
        )
        .await;

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            sink.0.load(Ordering::SeqCst),
            0,
            "ignored results must not reach the sink"
        );
    }

    #[tokio::test]
    async fn an_unopened_space_is_orphaned() {
        let dir = tempfile::tempdir().unwrap();
        let hub = Arc::new(WorkspaceHub::new());
        let id = TaskId(Uuid::new_v4());

        let (sink, mut rx) = sink();
        let payload = serde_json::to_string(&receipt(dir.path(), id)).unwrap();
        hook(&hub, sink)
            .on_after_tool(0, &dispatch_call(), &payload, false)
            .await;

        let Recorded::Orphaned(obs, reason) = next(&mut rx).await else {
            panic!("an unopened space must orphan the watcher");
        };
        assert_eq!(obs.task_id, id);
        assert!(
            reason.contains(&id.to_string()) && reason.contains("not open"),
            "the reason must name the task and the space: {reason}"
        );
        assert_watcher_ended(&mut rx).await;
    }

    #[tokio::test]
    async fn an_unknown_task_id_is_orphaned() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = test_runtime();
        runtime
            .hub()
            .expect("hub")
            .register(Arc::new(EchoMachine::new()))
            .expect("register echo");
        let hub = open_hub(dir.path(), runtime);
        let unknown = TaskId(Uuid::new_v4());

        let (sink, mut rx) = sink();
        let payload = serde_json::to_string(&receipt(dir.path(), unknown)).unwrap();
        hook(&hub, sink)
            .on_after_tool(0, &dispatch_call(), &payload, false)
            .await;

        let Recorded::Orphaned(obs, reason) = next(&mut rx).await else {
            panic!("an unknown task id must orphan the watcher");
        };
        assert_eq!(obs.task_id, unknown);
        assert!(
            reason.contains(&unknown.to_string()) && reason.contains("not found"),
            "the reason must name the task and say it is unknown: {reason}"
        );
        assert_watcher_ended(&mut rx).await;
    }

    #[tokio::test]
    async fn closing_the_space_converges_to_orphaned() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = test_runtime();
        let gate = Arc::new(Notify::new());
        runtime
            .hub()
            .expect("hub")
            .register(Arc::new(ScriptedMachine::new(
                "scripted",
                Arc::clone(&gate),
            )))
            .expect("register scripted");
        let id = submit(&runtime, "scripted", "gated").await;
        wait_for_working(&runtime, &id).await;
        // Hub 是 runtime 唯一强引用持有者：drop 后 close 才能让流真正结束。
        let hub = open_hub(dir.path(), runtime);

        let (sink, mut rx) = sink();
        let payload = serde_json::to_string(&receipt(dir.path(), id)).unwrap();
        hook(&hub, sink)
            .on_after_tool(0, &dispatch_call(), &payload, false)
            .await;
        let Recorded::Registered(_, _) = next(&mut rx).await else {
            panic!("the gated task must register first");
        };

        hub.begin_close(dir.path()).await.expect("begin close");
        hub.finish_close(dir.path()).await;

        let Recorded::Orphaned(obs, reason) = next(&mut rx).await else {
            panic!("a closed space must orphan the watcher");
        };
        assert_eq!(obs.task_id, id);
        assert!(
            reason.contains(&id.to_string()) && reason.contains("not open"),
            "the reason must name the task and the space: {reason}"
        );
    }

    async fn wait_for_working(runtime: &Arc<MachineRuntime>, id: &TaskId) {
        let hub = runtime.hub().expect("hub");
        let deadline = tokio::time::Instant::now() + BUDGET;
        loop {
            if let Ok(snapshot) = hub.snapshot(id).await
                && snapshot.state == TaskState::Working
            {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the gated task never reached Working"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
}
