use async_trait::async_trait;

use super::machine::{Machine, MachineDescriptor, MachineError};
use super::model::{Artifact, ArtifactId, Progress, Task, TaskId, TaskOutcome, TaskSnapshot};
use super::sink::{SinkError, TaskSink};
use super::state::TaskState;

/// 合作型替身：不碰任何外部执行体，只把「接单 → 推进 → 交付」走一遍。
/// 它的存在是「机制不依赖具体执行体」的可判定证据。
pub struct EchoMachine;

impl EchoMachine {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EchoMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Machine for EchoMachine {
    fn descriptor(&self) -> MachineDescriptor {
        MachineDescriptor {
            kind: "echo".to_string(),
            state_tag: "echo".to_string(),
            input_tag: "text".to_string(),
            description: "echoes the task input back as its output".to_string(),
        }
    }

    async fn submit(&self, task: Task, sink: TaskSink) -> Result<(), MachineError> {
        sink.begin().await.map_err(unavailable)?;

        // 进度步数可由提交方的 labels 调整，缺省两步足够看出节流与快照现值。
        let steps = task
            .labels
            .get("steps")
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(2);
        for step in 1..=steps {
            let progress = Progress {
                phase: Some("echo".to_string()),
                current: Some(step),
                total: Some(steps),
            };
            sink.progress(progress).await.map_err(unavailable)?;
        }

        let input = task.input;
        sink.artifact(Artifact {
            id: ArtifactId("echo".to_string()),
            kind: "text".to_string(),
            // 单片即完整值，起始偏移恒为 0。
            offset: 0,
            append: false,
            last_chunk: true,
            payload: input.clone(),
        })
        .await
        .map_err(unavailable)?;

        sink.finish(
            TaskState::Completed,
            TaskOutcome {
                output: Some(input),
                error: None,
            },
        )
        .await
        .map_err(unavailable)
    }

    async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
        Ok(())
    }

    async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
        Err(MachineError::NotFound { id: *id })
    }
}

fn unavailable(err: SinkError) -> MachineError {
    MachineError::Unavailable {
        reason: err.to_string(),
    }
}

/// 测试替身集合：每个替身只服务一条用例的可判红点。
#[cfg(test)]
pub(crate) mod doubles {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use serde_json::Value;
    use tokio::sync::Notify;

    use crate::machine::machine::{Machine, MachineDescriptor, MachineError};
    use crate::machine::model::{
        Artifact, ArtifactId, Progress, TaggedJson, Task, TaskError, TaskFilter, TaskId,
        TaskOutcome, TaskSnapshot,
    };
    use crate::machine::sink::{SinkError, TaskSink};
    use crate::machine::state::{OutputChunk, TaskState};
    use crate::machine::store::{InMemoryStore, StoreError, TaskStore};

    use super::unavailable;

    fn descriptor(kind: &str) -> MachineDescriptor {
        MachineDescriptor {
            kind: kind.to_string(),
            state_tag: kind.to_string(),
            input_tag: "text".to_string(),
            description: format!("test double: {kind}"),
        }
    }

    fn text(payload: &str) -> TaggedJson {
        TaggedJson {
            tag: "text".to_string(),
            payload: Value::String(payload.to_string()),
        }
    }

    /// 接单后永不返回：Hub 只能靠 deadline / cancel 兜底，等 future 结束就会永久
    /// 挂在非终态。
    pub(crate) struct StuckMachine {
        kind: String,
    }

    impl StuckMachine {
        pub(crate) fn new(kind: &str) -> Self {
            Self {
                kind: kind.to_string(),
            }
        }
    }

    #[async_trait]
    impl Machine for StuckMachine {
        fn descriptor(&self) -> MachineDescriptor {
            descriptor(&self.kind)
        }

        async fn submit(&self, _task: Task, sink: TaskSink) -> Result<(), MachineError> {
            sink.begin().await.map_err(unavailable)?;
            std::future::pending::<()>().await;
            Ok(())
        }

        async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
            Ok(())
        }

        async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
            Err(MachineError::NotFound { id: *id })
        }
    }

    /// 接单后立刻返回、不 spawn 也不 `finish`：验证「executor future 结束」这条
    /// 独立的收敛路径。
    pub(crate) struct AbandoningMachine {
        kind: String,
    }

    impl AbandoningMachine {
        pub(crate) fn new(kind: &str) -> Self {
            Self {
                kind: kind.to_string(),
            }
        }
    }

    #[async_trait]
    impl Machine for AbandoningMachine {
        fn descriptor(&self) -> MachineDescriptor {
            descriptor(&self.kind)
        }

        async fn submit(&self, _task: Task, sink: TaskSink) -> Result<(), MachineError> {
            sink.begin().await.map_err(unavailable)?;
            Ok(())
        }

        async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
            Ok(())
        }

        async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
            Err(MachineError::NotFound { id: *id })
        }
    }

    /// `submit` 报错：executor 正常结束但带着错误，收敛路径与 panic 不同。
    #[derive(Clone, Copy)]
    pub(crate) enum SubmitFailure {
        Unavailable,
        Illegal,
    }

    pub(crate) struct ErrMachine {
        kind: String,
        failure: SubmitFailure,
    }

    impl ErrMachine {
        pub(crate) fn new(kind: &str, failure: SubmitFailure) -> Self {
            Self {
                kind: kind.to_string(),
                failure,
            }
        }
    }

    #[async_trait]
    impl Machine for ErrMachine {
        fn descriptor(&self) -> MachineDescriptor {
            descriptor(&self.kind)
        }

        async fn submit(&self, _task: Task, sink: TaskSink) -> Result<(), MachineError> {
            sink.begin().await.map_err(unavailable)?;
            Err(match self.failure {
                SubmitFailure::Unavailable => MachineError::Unavailable {
                    reason: "test double is unavailable".to_string(),
                },
                SubmitFailure::Illegal => MachineError::Illegal {
                    reason: "test double rejects the request".to_string(),
                },
            })
        }

        async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
            Ok(())
        }

        async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
            Err(MachineError::NotFound { id: *id })
        }
    }

    /// `submit` 里 panic：只有 Hub 外层 spawn 观测 `JoinError` 才能收敛。
    pub(crate) struct PanickingMachine {
        kind: String,
    }

    impl PanickingMachine {
        pub(crate) fn new(kind: &str) -> Self {
            Self {
                kind: kind.to_string(),
            }
        }
    }

    #[async_trait]
    impl Machine for PanickingMachine {
        fn descriptor(&self) -> MachineDescriptor {
            descriptor(&self.kind)
        }

        async fn submit(&self, _task: Task, sink: TaskSink) -> Result<(), MachineError> {
            sink.begin().await.map_err(unavailable)?;
            panic!("boom")
        }

        async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
            Ok(())
        }

        async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
            Err(MachineError::NotFound { id: *id })
        }
    }

    /// 已拿到句柄再拒收：拒收是带 id 的权威终态，不是调用失败。
    pub(crate) struct RejectingMachine {
        kind: String,
    }

    impl RejectingMachine {
        pub(crate) fn new(kind: &str) -> Self {
            Self {
                kind: kind.to_string(),
            }
        }
    }

    #[async_trait]
    impl Machine for RejectingMachine {
        fn descriptor(&self) -> MachineDescriptor {
            descriptor(&self.kind)
        }

        async fn submit(&self, _task: Task, sink: TaskSink) -> Result<(), MachineError> {
            let outcome = TaskOutcome {
                output: None,
                error: Some(TaskError {
                    kind: "admission".to_string(),
                    message: "test double rejects the input".to_string(),
                    detail: None,
                }),
            };
            sink.finish(TaskState::Rejected, outcome)
                .await
                .map_err(unavailable)?;
            Ok(())
        }

        async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
            Ok(())
        }

        async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
            Err(MachineError::NotFound { id: *id })
        }
    }

    /// 逐片写入并记下每片的结果：合并后的完整值只能从快照读，单片是否被接受
    /// 只能从这串返回值读，`TooLarge` 之后 impl 也不该拿到改写过的数据。
    pub(crate) struct ArtifactMachine {
        kind: String,
        chunks: Vec<Artifact>,
        results: Mutex<Vec<Result<(), SinkError>>>,
    }

    impl ArtifactMachine {
        pub(crate) fn new(kind: &str, chunks: Vec<Artifact>) -> Self {
            Self {
                kind: kind.to_string(),
                chunks,
                results: Mutex::new(Vec::new()),
            }
        }

        pub(crate) fn results(&self) -> Vec<Result<(), SinkError>> {
            self.results.lock().expect("artifact result slot").clone()
        }
    }

    #[async_trait]
    impl Machine for ArtifactMachine {
        fn descriptor(&self) -> MachineDescriptor {
            descriptor(&self.kind)
        }

        async fn submit(&self, _task: Task, sink: TaskSink) -> Result<(), MachineError> {
            sink.begin().await.map_err(unavailable)?;
            // 单片被拒也要走完剧本：被拒的是这一片，不是整个任务。
            for chunk in &self.chunks {
                let result = sink.artifact(chunk.clone()).await;
                self.results
                    .lock()
                    .expect("artifact result slot")
                    .push(result);
            }
            let outcome = TaskOutcome {
                output: Some(text("artifact-complete")),
                error: None,
            };
            sink.finish(TaskState::Completed, outcome)
                .await
                .map_err(unavailable)
        }

        async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
            Ok(())
        }

        async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
            Err(MachineError::NotFound { id: *id })
        }
    }

    #[derive(Clone, Copy)]
    pub(crate) enum GateMode {
        /// 门闸放行后才 `begin`：用来证明 `submit` 不 await 执行体。
        BeginAfterGate,
        /// 还没推进就要求人介入：`Submitted` 直达 `InputRequired`。
        WaitInputBeforeBegin,
    }

    /// 首行停在门闸上：门闸不放行时 `submit` 若等待执行体就永远返回不了。
    pub(crate) struct GateMachine {
        kind: String,
        gate: Arc<Notify>,
        mode: GateMode,
    }

    impl GateMachine {
        pub(crate) fn new(kind: &str, gate: Arc<Notify>, mode: GateMode) -> Self {
            Self {
                kind: kind.to_string(),
                gate,
                mode,
            }
        }
    }

    #[async_trait]
    impl Machine for GateMachine {
        fn descriptor(&self) -> MachineDescriptor {
            descriptor(&self.kind)
        }

        async fn submit(&self, _task: Task, sink: TaskSink) -> Result<(), MachineError> {
            self.gate.notified().await;
            match self.mode {
                GateMode::BeginAfterGate => sink.begin().await.map_err(unavailable)?,
                GateMode::WaitInputBeforeBegin => sink
                    .wait_input(text("needs input"))
                    .await
                    .map_err(unavailable)?,
            }
            // 停在非终态，让测试观察 Hub 侧兜底而不是「future 结束」这条路。
            std::future::pending::<()>().await;
            Ok(())
        }

        async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
            Ok(())
        }

        async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
            Err(MachineError::NotFound { id: *id })
        }
    }

    /// 固定剧本：`begin` 后停在门闸上，放行后 3 条 `progress` + 1 条 `artifact` + 完成。
    /// 门闸给出「已 begin、增量未发」的窗口，订阅点才能落在两者之间。
    pub(crate) struct ScriptedMachine {
        kind: String,
        gate: Arc<Notify>,
    }

    impl ScriptedMachine {
        pub(crate) fn new(kind: &str, gate: Arc<Notify>) -> Self {
            Self {
                kind: kind.to_string(),
                gate,
            }
        }
    }

    #[async_trait]
    impl Machine for ScriptedMachine {
        fn descriptor(&self) -> MachineDescriptor {
            descriptor(&self.kind)
        }

        async fn submit(&self, _task: Task, sink: TaskSink) -> Result<(), MachineError> {
            sink.begin().await.map_err(unavailable)?;
            self.gate.notified().await;
            for step in 1..=3 {
                let progress = Progress {
                    phase: Some("scripted".to_string()),
                    current: Some(step),
                    total: Some(3),
                };
                sink.progress(progress).await.map_err(unavailable)?;
            }
            sink.artifact(Artifact {
                id: ArtifactId("scripted".to_string()),
                kind: "text".to_string(),
                offset: 0,
                append: false,
                last_chunk: true,
                payload: text("scripted artifact"),
            })
            .await
            .map_err(unavailable)?;
            let outcome = TaskOutcome {
                output: Some(text("scripted output")),
                error: None,
            };
            sink.finish(TaskState::Completed, outcome)
                .await
                .map_err(unavailable)
        }

        async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
            Ok(())
        }

        async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
            Err(MachineError::NotFound { id: *id })
        }
    }

    /// 等外部时序点到了才 `finish`：用来分辨「grace 窗口内交付」与「窗口外迟到」。
    /// 迟到的写入会被 Hub 吸收，只有 impl 自己看得到拒绝，故把结论记下来给用例断言。
    pub(crate) struct DelayedFinishMachine {
        kind: String,
        delay: Duration,
        at: TaskState,
        finish_result: Mutex<Option<Result<(), SinkError>>>,
    }

    impl DelayedFinishMachine {
        pub(crate) fn new(kind: &str, delay: Duration, at: TaskState) -> Self {
            Self {
                kind: kind.to_string(),
                delay,
                at,
                finish_result: Mutex::new(None),
            }
        }

        /// `None` = 还没走到 `finish`；`Some(Ok)` = 终态被接受。
        pub(crate) fn finish_result(&self) -> Option<Result<(), SinkError>> {
            self.finish_result
                .lock()
                .expect("finish result slot")
                .clone()
        }
    }

    #[async_trait]
    impl Machine for DelayedFinishMachine {
        fn descriptor(&self) -> MachineDescriptor {
            descriptor(&self.kind)
        }

        async fn submit(&self, _task: Task, sink: TaskSink) -> Result<(), MachineError> {
            sink.begin().await.map_err(unavailable)?;
            tokio::time::sleep(self.delay).await;
            let outcome = TaskOutcome {
                output: None,
                error: None,
            };
            let result = sink.finish(self.at, outcome).await;
            *self.finish_result.lock().expect("finish result slot") = Some(result);
            Ok(())
        }

        async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
            Ok(())
        }

        async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
            Err(MachineError::NotFound { id: *id })
        }
    }

    /// 在不让出的循环里连发 `count` 条 `progress` 后完成：进度是唯一增量，
    /// 因此「是否溢出缓冲」只由节流决定。
    pub(crate) struct CountingMachine {
        kind: String,
        count: u64,
    }

    impl CountingMachine {
        pub(crate) fn new(kind: &str, count: u64) -> Self {
            Self {
                kind: kind.to_string(),
                count,
            }
        }
    }

    #[async_trait]
    impl Machine for CountingMachine {
        fn descriptor(&self) -> MachineDescriptor {
            descriptor(&self.kind)
        }

        async fn submit(&self, _task: Task, sink: TaskSink) -> Result<(), MachineError> {
            sink.begin().await.map_err(unavailable)?;
            for current in 1..=self.count {
                let progress = Progress {
                    phase: Some("counting".to_string()),
                    current: Some(current),
                    total: Some(self.count),
                };
                sink.progress(progress).await.map_err(unavailable)?;
            }
            let outcome = TaskOutcome {
                output: None,
                error: None,
            };
            sink.finish(TaskState::Completed, outcome)
                .await
                .map_err(unavailable)
        }

        async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
            Ok(())
        }

        async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
            Err(MachineError::NotFound { id: *id })
        }
    }

    /// 两种热写都发一遍：落库次数若与状态转移数不等，只能是落了不该落的东西。
    pub(crate) struct ChattyMachine {
        kind: String,
    }

    impl ChattyMachine {
        pub(crate) const PROGRESS_STEPS: u64 = 2;
        pub(crate) const OUTPUT_CHUNKS: usize = 2;
        pub(crate) const CHUNK_BYTES: usize = 8;

        pub(crate) fn new(kind: &str) -> Self {
            Self {
                kind: kind.to_string(),
            }
        }
    }

    #[async_trait]
    impl Machine for ChattyMachine {
        fn descriptor(&self) -> MachineDescriptor {
            descriptor(&self.kind)
        }

        async fn submit(&self, _task: Task, sink: TaskSink) -> Result<(), MachineError> {
            sink.begin().await.map_err(unavailable)?;
            for step in 1..=Self::PROGRESS_STEPS {
                let progress = Progress {
                    phase: Some("chatty".to_string()),
                    current: Some(step),
                    total: Some(Self::PROGRESS_STEPS),
                };
                sink.progress(progress).await.map_err(unavailable)?;
            }
            for index in 0..Self::OUTPUT_CHUNKS {
                let chunk = OutputChunk {
                    stream: "stdout".to_string(),
                    offset: (index * Self::CHUNK_BYTES) as u64,
                    data: vec![b'x'; Self::CHUNK_BYTES],
                };
                sink.output(chunk).await.map_err(unavailable)?;
            }
            let outcome = TaskOutcome {
                output: Some(text("chatty-complete")),
                error: None,
            };
            sink.finish(TaskState::Completed, outcome)
                .await
                .map_err(unavailable)
        }

        async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
            Ok(())
        }

        async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
            Err(MachineError::NotFound { id: *id })
        }
    }

    /// `begin` 后停在门闸上，放行后每条 `progress` 之间让出一次：观察者能等到
    /// 「已推进、未发完」的时刻再订阅，且读得动整段流。
    pub(crate) struct PacedMachine {
        kind: String,
        gate: Arc<Notify>,
        count: u64,
    }

    impl PacedMachine {
        pub(crate) fn new(kind: &str, gate: Arc<Notify>, count: u64) -> Self {
            Self {
                kind: kind.to_string(),
                gate,
                count,
            }
        }
    }

    #[async_trait]
    impl Machine for PacedMachine {
        fn descriptor(&self) -> MachineDescriptor {
            descriptor(&self.kind)
        }

        async fn submit(&self, _task: Task, sink: TaskSink) -> Result<(), MachineError> {
            sink.begin().await.map_err(unavailable)?;
            self.gate.notified().await;
            for current in 1..=self.count {
                let progress = Progress {
                    phase: Some("paced".to_string()),
                    current: Some(current),
                    total: Some(self.count),
                };
                sink.progress(progress).await.map_err(unavailable)?;
                tokio::task::yield_now().await;
            }
            let outcome = TaskOutcome {
                output: None,
                error: None,
            };
            sink.finish(TaskState::Completed, outcome)
                .await
                .map_err(unavailable)
        }

        async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
            Ok(())
        }

        async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
            Err(MachineError::NotFound { id: *id })
        }
    }

    /// 在不让出的循环里连发 `count` 条等长 `output` 后完成：撑爆广播缓冲是正常路径，
    /// 内容是否还在只能问快照的 `output_tail`。
    pub(crate) struct OutputMachine {
        kind: String,
        count: usize,
    }

    impl OutputMachine {
        /// 每条输出等长，裁剪只会落在 chunk 边界上，字节比较才可判。
        pub(crate) const CHUNK_BYTES: usize = 16;

        pub(crate) fn new(kind: &str, count: usize) -> Self {
            Self {
                kind: kind.to_string(),
                count,
            }
        }

        /// 方括号包裹左零补齐的序号：定长、互不相同，且任何非对齐窗口都不可能
        /// 与本条内容相等（非对齐窗口以数字开头，本条以 `[` 开头）。
        pub(crate) fn chunk(index: usize) -> Vec<u8> {
            format!("[{index:014}]").into_bytes()
        }

        pub(crate) fn marker() -> TaggedJson {
            text("output-complete")
        }
    }

    #[async_trait]
    impl Machine for OutputMachine {
        fn descriptor(&self) -> MachineDescriptor {
            descriptor(&self.kind)
        }

        async fn submit(&self, _task: Task, sink: TaskSink) -> Result<(), MachineError> {
            sink.begin().await.map_err(unavailable)?;
            for index in 0..self.count {
                let chunk = OutputChunk {
                    stream: "stdout".to_string(),
                    offset: (index * Self::CHUNK_BYTES) as u64,
                    data: Self::chunk(index),
                };
                sink.output(chunk).await.map_err(unavailable)?;
            }
            let outcome = TaskOutcome {
                output: Some(Self::marker()),
                error: None,
            };
            sink.finish(TaskState::Completed, outcome)
                .await
                .map_err(unavailable)
        }

        async fn cancel(&self, _id: &TaskId) -> Result<(), MachineError> {
            Ok(())
        }

        async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError> {
            Err(MachineError::NotFound { id: *id })
        }
    }

    /// 包一层计数：写次数等于状态转移数，是「`progress`/`output` 不落库」的可判证据。
    pub(crate) struct RecordingStore {
        inner: InMemoryStore,
        puts: AtomicUsize,
    }

    impl RecordingStore {
        pub(crate) fn new() -> Self {
            Self {
                inner: InMemoryStore::new(),
                puts: AtomicUsize::new(0),
            }
        }

        pub(crate) fn puts(&self) -> usize {
            self.puts.load(Ordering::SeqCst)
        }
    }

    impl TaskStore for RecordingStore {
        fn put_snapshot(&self, snap: &TaskSnapshot) -> Result<(), StoreError> {
            self.puts.fetch_add(1, Ordering::SeqCst);
            self.inner.put_snapshot(snap)
        }

        fn load_snapshot(&self, id: &TaskId) -> Result<Option<TaskSnapshot>, StoreError> {
            self.inner.load_snapshot(id)
        }

        fn list(&self, filter: &TaskFilter) -> Result<Vec<TaskId>, StoreError> {
            self.inner.list(filter)
        }
    }
}
