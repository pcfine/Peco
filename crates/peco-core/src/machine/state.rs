use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::model::{Artifact, Progress, TaggedJson, TaskId, TaskSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Submitted,
    Working,
    InputRequired,
    AuthRequired,
    Completed,
    Failed,
    Canceled,
    TimedOut,
    Rejected,
}

impl TaskState {
    /// 对外的状态串：`Debug` 是内部表示，改它的措辞不该顺带改契约 ——
    /// `DispatchReceipt.state` 与台账都按这个名字读写。
    ///
    /// 与 serde 的 wire name 一致，改一处必须改另一处。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::Working => "working",
            Self::InputRequired => "input_required",
            Self::AuthRequired => "auth_required",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::TimedOut => "timed_out",
            Self::Rejected => "rejected",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Canceled | Self::TimedOut | Self::Rejected
        )
    }
}

/// 转移表与 `Update` 同放一处：增量形状由状态机决定，分家会让同一份知识落两处。
///
/// 合法性只在 Hub 侧判定一次，impl 无从把终态改回去（终态吸收）。
pub fn can_transition(from: TaskState, to: TaskState) -> bool {
    use TaskState::*;

    if from.is_terminal() {
        return false;
    }
    match from {
        // Submitted → Completed 不存在：未开始就没有可交付的结果。
        Submitted => matches!(
            to,
            Working | InputRequired | AuthRequired | Failed | Canceled | TimedOut | Rejected
        ),
        // Working → Working 是 `begin` 重入：声明「已在推进」，不是推进一步。
        Working => matches!(
            to,
            Working | InputRequired | AuthRequired | Completed | Failed | Canceled | TimedOut
        ),
        // 两个 interrupted 都可续喂回 Working。
        InputRequired | AuthRequired => matches!(to, Working | Failed | Canceled | TimedOut),
        Completed | Failed | Canceled | TimedOut | Rejected => false,
    }
}

/// 每任务从 1 单调 +1、无空洞。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Update {
    pub task_id: TaskId,
    pub seq: u64,
    pub at: DateTime<Utc>,
    /// 区分「实现自己收敛」与「Hub 兜底强制收敛」。
    pub origin: UpdateOrigin,
    pub body: UpdateBody,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateOrigin {
    Machine,
    Hub,
}

/// 信封封闭（四种）、载荷开放（`TaggedJson`）：消费者只需知道哪一类变了。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum UpdateBody {
    Status {
        state: TaskState,
        detail: Option<TaggedJson>,
    },
    Progress(Progress),
    Output(OutputChunk),
    Artifact(Artifact),
}

/// `stream` 只是标签：分属不同 stream 的片仍写进同一窗口。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutputChunk {
    pub stream: String,
    /// 单一全局偏移：该片在全流里的起始字节位置，由 Hub 校验连续性 —— 不等于已写入
    /// 总量即拒收，读者据此判断能否由快照补齐。
    pub offset: u64,
    pub data: Vec<u8>,
}

// 首帧快照只投递一次，且读方需要拥有它；只为压缩枚举体积就给每帧加一层
// 间接引用不划算。
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ObsEvent {
    /// 首帧，恰好一次。
    Snapshot(TaskSnapshot),
    Update(Update),
    /// 本地视图已失效，须重订阅取新快照。
    Lagged {
        dropped: u64,
    },
    /// 已无空洞地投递到终态。
    End,
}

#[cfg(test)]
mod tests {
    use super::*;
    use TaskState::*;

    const ALL_STATES: [TaskState; 9] = [
        Submitted,
        Working,
        InputRequired,
        AuthRequired,
        Completed,
        Failed,
        Canceled,
        TimedOut,
        Rejected,
    ];

    /// 状态机是这套机制的正确性根：允许的边逐条列出，其余 81 格一律拒绝。
    fn allowed_edges() -> Vec<(TaskState, TaskState)> {
        vec![
            (Submitted, Working),
            (Submitted, InputRequired),
            (Submitted, AuthRequired),
            (Submitted, Failed),
            (Submitted, Canceled),
            (Submitted, TimedOut),
            (Submitted, Rejected),
            (Working, Working),
            (Working, InputRequired),
            (Working, AuthRequired),
            (Working, Completed),
            (Working, Failed),
            (Working, Canceled),
            (Working, TimedOut),
            (InputRequired, Working),
            (InputRequired, Failed),
            (InputRequired, Canceled),
            (InputRequired, TimedOut),
            (AuthRequired, Working),
            (AuthRequired, Failed),
            (AuthRequired, Canceled),
            (AuthRequired, TimedOut),
        ]
    }

    #[test]
    fn transition_matrix_matches_table() {
        let allowed = allowed_edges();
        for from in ALL_STATES {
            for to in ALL_STATES {
                let expected = allowed.contains(&(from, to));
                assert_eq!(
                    can_transition(from, to),
                    expected,
                    "unexpected transition verdict: from={from:?} to={to:?}"
                );
            }
        }
    }

    /// 这九个串是对外契约（模型与台账都读它），逐条钉死以便改错时立刻红。
    #[test]
    fn as_str_is_the_wire_name_of_each_state() {
        let expected = [
            (Submitted, "submitted"),
            (Working, "working"),
            (InputRequired, "input_required"),
            (AuthRequired, "auth_required"),
            (Completed, "completed"),
            (Failed, "failed"),
            (Canceled, "canceled"),
            (TimedOut, "timed_out"),
            (Rejected, "rejected"),
        ];
        for (state, name) in expected {
            assert_eq!(state.as_str(), name, "wrong wire name: state={state:?}");
        }
    }

    /// serde 与 `as_str` 是同一份契约的两处实现：任一处漂移都会让下游看到两种拼法。
    #[test]
    fn serde_wire_name_matches_as_str_for_each_state() {
        for state in ALL_STATES {
            assert_eq!(
                serde_json::to_value(state).expect("state serializes"),
                serde_json::json!(state.as_str()),
                "serde name drifted from as_str: state={state:?}"
            );
        }
    }

    #[test]
    fn is_terminal_covers_each_variant() {
        let terminal = [Completed, Failed, Canceled, TimedOut, Rejected];
        for state in ALL_STATES {
            assert_eq!(
                state.is_terminal(),
                terminal.contains(&state),
                "wrong terminal verdict: state={state:?}"
            );
        }
    }
}
