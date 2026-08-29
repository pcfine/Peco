// ============================================================================
// SessionSnapshot + TurnBoundaryToken
// ============================================================================
//
// SessionSnapshot 是 Session 的不可变持久化快照，仅在 turn 边界后生成。
// TurnBoundaryToken 保证 snapshot() 只能在 commit_turn()/rollback_turn() 后调用。

use model_provider::Usage;
use serde::{Deserialize, Serialize};

use super::types::{AnnotatedMessage, PendingInput};

/// Session 的持久化快照。
///
/// 仅在 turn 边界后生成（commit_turn 或 rollback_turn 完成后）。
/// 此时 staging 恒空、state 恒 Idle，因此快照不需要携带这两个字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// 已确认的 turn 历史
    pub committed_turns: Vec<Vec<AnnotatedMessage>>,

    // ── 单调计数器（恢复连续性必需）──
    /// 当前 turn 编号
    pub turn_index: usize,
    /// 聚合 token 用量
    pub total_usage: Usage,
    /// 下一个消息 ID
    pub next_message_id: u64,

    // ── 排队输入（turn 完成时可能仍有未处理的输入）──
    /// 排队中的用户输入
    pub pending_inputs: Vec<PendingInput>,

    // ── 上下文压缩（compaction 产物）──
    /// 钉扎在上下文最前的历史摘要。
    ///
    /// 不属于任何 committed turn — 物理修剪驱逐的轮次被摘要替换后，
    /// 摘要以独立的 pinned 消息存活，随快照持久化。
    /// `#[serde(default)]` 保证旧快照（无此字段）可反序列化。
    #[serde(default)]
    pub pinned_summary: Option<AnnotatedMessage>,
}

/// Turn 边界令牌。
///
/// 零大小类型，仅 `Session::commit_turn()` 和 `Session::rollback_turn()` 可构造。
/// `Session::snapshot()` 需要此令牌作为参数，编译期保证快照只在 turn 边界生成。
///
/// 编译后完全优化掉（零大小 + 内联）。
#[derive(Debug)]
pub struct TurnBoundaryToken(pub(crate) ());
