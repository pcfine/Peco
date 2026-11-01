use std::sync::{Arc, Weak};

use thiserror::Error;

use super::hub::HubInner;
use super::model::{Artifact, ArtifactId, Progress, TaggedJson, TaskId, TaskOutcome};
use super::state::{OutputChunk, TaskState};

/// impl 侧的写入口。持 `Weak<HubInner>`：Hub 停掉后 sink 自动失效，不留悬挂引用。
///
/// 状态机合法性只在 Hub 校验一次（状态表不复制进每个 impl），故所有方法都返回
/// `Result`，非法调用得到 `SinkError` 而不是 `panic`。sink 无 `Drop` 副作用。
#[derive(Clone)]
pub struct TaskSink {
    hub: Weak<HubInner>,
    id: TaskId,
}

impl TaskSink {
    pub(crate) fn new(hub: Weak<HubInner>, id: TaskId) -> Self {
        Self { hub, id }
    }

    pub async fn begin(&self) -> Result<(), SinkError> {
        self.hub()?.begin(&self.id)
    }

    pub async fn wait_input(&self, why: TaggedJson) -> Result<(), SinkError> {
        self.hub()?
            .transition(&self.id, TaskState::InputRequired, why)
    }

    pub async fn need_auth(&self, why: TaggedJson) -> Result<(), SinkError> {
        self.hub()?
            .transition(&self.id, TaskState::AuthRequired, why)
    }

    pub async fn resume(&self) -> Result<(), SinkError> {
        self.hub()?.resume(&self.id)
    }

    pub async fn progress(&self, progress: Progress) -> Result<(), SinkError> {
        self.hub()?.progress(&self.id, progress)
    }

    pub async fn output(&self, chunk: OutputChunk) -> Result<(), SinkError> {
        self.hub()?.output(&self.id, chunk)
    }

    pub async fn artifact(&self, artifact: Artifact) -> Result<(), SinkError> {
        self.hub()?.artifact(&self.id, artifact)
    }

    /// 终态由参数给出（`TaskOutcome` 不带 `state`），Hub 不校验 `error` 与 `state` 的组合
    /// 是否匹配 —— 那是 impl 的语义责任。
    pub async fn finish(&self, state: TaskState, outcome: TaskOutcome) -> Result<(), SinkError> {
        self.hub()?.finish(&self.id, state, outcome)
    }

    fn hub(&self) -> Result<Arc<HubInner>, SinkError> {
        self.hub.upgrade().ok_or(SinkError::Closed)
    }
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum SinkError {
    #[error("task not found: {id}")]
    NotFound { id: TaskId },
    #[error("illegal transition: {from:?} -> {to:?}")]
    IllegalTransition { from: TaskState, to: TaskState },
    #[error("output offset gap: expected {expected}, got {got}")]
    OffsetGap { expected: u64, got: u64 },
    #[error("merged artifact {artifact:?} exceeds limit of {limit} bytes")]
    TooLarge { artifact: ArtifactId, limit: u64 },
    #[error("hub is closed")]
    Closed,
}
