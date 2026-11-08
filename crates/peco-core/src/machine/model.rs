use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::state::TaskState;

/// 由 Hub 单点发放：父子引用、Store 键、订阅键都以它为唯一标识。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub Uuid);

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Uuid> for TaskId {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

/// `dyn` 边界上关联类型不可用，impl 的具体状态/载荷只能擦成「tag + JSON」。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaggedJson {
    pub tag: String,
    pub payload: serde_json::Value,
}

/// 要做什么、在什么环境下做、谁提交的 —— 与执行体无关，故为纯数据。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub kind: String,
    pub input: TaggedJson,
    pub context: TaskContext,
    pub source: TaskSource,
    /// 绝对时间：相对时长在排队/重启后失真。
    pub deadline: Option<DateTime<Utc>>,
    pub labels: BTreeMap<String, String>,
    pub created_at: DateTime<Utc>,
}

impl Task {
    /// 父子关系只从 `source` 派生：同一事实存两处必然改一处漏一处。
    pub fn parent(&self) -> Option<TaskId> {
        match &self.source {
            TaskSource::Machine { parent } => Some(*parent),
            TaskSource::External { .. } | TaskSource::System { .. } => None,
        }
    }
}

/// `scope` 是隔离域（Hub 只存不解释）；`data` 是 impl 需要的结构化上下文。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskContext {
    pub scope: Option<String>,
    pub data: Option<TaggedJson>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TaskSource {
    External { principal: String },
    Machine { parent: TaskId },
    System { component: String },
}

/// 全部字段都能下推为列/索引（将来换 SQLite 后端不需要改调用点），
/// 故不接受闭包谓词；每个 `None` 表示不过滤，`labels` 全部匹配（AND）。
///
/// `Default` 即「不过滤」：调用方只关心其中一两个维度时，不必把其余维度
/// 逐个写一遍 None。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskFilter {
    pub kind: Option<String>,
    pub state: Option<TaskState>,
    pub labels: BTreeMap<String, String>,
    pub scope: Option<String>,
    pub created_after: Option<DateTime<Utc>>,
}

/// 观察与结果的唯一权威副本：事件只是通知，读者靠它补齐。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub id: TaskId,
    pub kind: String,
    pub state: TaskState,
    pub state_detail: Option<TaggedJson>,
    pub progress: Progress,
    pub output: OutputView,
    /// 仅 terminal 时非 `None`；终态只由 `state` 承载，避免两处判据分叉。
    pub outcome: Option<TaskOutcome>,
    /// 每个 `id` 恰好一条已合并的完整值。
    pub artifacts: Vec<Artifact>,
    pub deadline: Option<DateTime<Utc>>,
    pub source: TaskSource,
    pub labels: BTreeMap<String, String>,
    /// 与 `TaskContext::scope` 同值：`TaskFilter` 要按 scope 过滤，而快照是
    /// `list` 唯一能读到的副本。
    pub scope: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 已折进本快照的最大 seq。
    pub last_seq: u64,
}

/// `Output` 的有界权威副本：窗口之外的内容不承诺可补齐。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutputView {
    pub tail: Vec<u8>,
    pub start_offset: u64,
    pub end_offset: u64,
}

/// 不带 `state`：终态由 `TaskSnapshot.state` 单点承载。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskOutcome {
    pub output: Option<TaggedJson>,
    pub error: Option<TaskError>,
}

/// 可替代增量：latest-wins，快照持现值。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub phase: Option<String>,
    pub current: Option<u64>,
    pub total: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArtifactId(pub String);

/// `offset` + `append` + `last_chunk` 让分片能还原成完整值，而不依赖增量通道。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub kind: String,
    pub offset: u64,
    pub append: bool,
    pub last_chunk: bool,
    pub payload: TaggedJson,
}

/// `kind` 是开放字符串，Hub 不对它 `match`，impl 自定语义。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskError {
    pub kind: String,
    pub message: String,
    pub detail: Option<TaggedJson>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_with_source(source: TaskSource) -> Task {
        Task {
            id: TaskId(Uuid::nil()),
            kind: "echo".to_string(),
            input: TaggedJson {
                tag: "text".to_string(),
                payload: serde_json::Value::String("hi".to_string()),
            },
            context: TaskContext {
                scope: None,
                data: None,
            },
            source,
            deadline: None,
            labels: BTreeMap::new(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn parent_comes_from_machine_source_only() {
        let parent = TaskId(Uuid::from_u128(7));
        assert_eq!(
            task_with_source(TaskSource::Machine { parent }).parent(),
            Some(parent)
        );

        let external = task_with_source(TaskSource::External {
            principal: "user-a".to_string(),
        });
        assert_eq!(external.parent(), None);

        let system = task_with_source(TaskSource::System {
            component: "cron".to_string(),
        });
        assert_eq!(system.parent(), None);
    }
}
