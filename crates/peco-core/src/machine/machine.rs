use async_trait::async_trait;
use thiserror::Error;

use super::model::{Task, TaskId, TaskSnapshot};
use super::sink::TaskSink;

/// 执行体的唯一抽象。Hub 只知道「接单 / 取消 / 自省」，执行差异全归 impl。
///
/// 包装对象不同（agent / shell / 纯函数 / 远端）不影响观察面：状态推进与增量产出
/// 一律经 [`TaskSink`] 写回 Hub，impl 无从把终态改回去。
#[async_trait]
pub trait Machine: Send + Sync + 'static {
    /// `kind` 是 `TaskHub::submit` 的路由键。
    fn descriptor(&self) -> MachineDescriptor;

    /// 返回的 future 寿命即执行体寿命：Hub 只 spawn、不 await（提交耗时与执行时长无关）。
    /// future 结束而快照仍非终态时，Hub 兜底落 `Failed`；impl 只想后台长驻时必须让
    /// future 与执行共寿命，否则该工作不再被观测。
    async fn submit(&self, task: Task, sink: TaskSink) -> Result<(), MachineError>;

    /// 请求取消：幂等；不保证立即生效，impl 须在观察到的安全边界收敛到 `Canceled`。
    async fn cancel(&self, id: &TaskId) -> Result<(), MachineError>;

    /// impl 自省用。观察协议的权威副本在 Hub，调用方不应以它做判定。
    async fn snapshot(&self, id: &TaskId) -> Result<TaskSnapshot, MachineError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineDescriptor {
    pub kind: String,
    pub state_tag: String,
    pub input_tag: String,
    pub description: String,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MachineError {
    /// Hub 层：无同 `kind` 的 Machine 注册，此时无快照、无 id。
    #[error("no machine registered for kind: {kind}")]
    UnknownKind { kind: String },
    #[error("task not found: {id}")]
    NotFound { id: TaskId },
    /// 实现层暂不可用、executor future 返回 `Err`、以及 `await_*` 超限都用它。
    #[error("machine is temporarily unavailable: {reason}")]
    Unavailable { reason: String },
    /// Hub 层入参拒绝（`kind` 重复注册等）：调用方给的东西本身不成立。
    #[error("illegal request: {reason}")]
    Illegal { reason: String },
}
