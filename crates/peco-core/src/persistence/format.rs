// ============================================================================
// v3 磁盘格式定义
// ============================================================================

use model_provider::Usage;
use serde::{Deserialize, Serialize};

use crate::session::{AnnotatedMessage, PendingInput, SessionMeta};

/// 磁盘上存储的会话文件结构（v3）。
///
/// 仅在 turn 完成后落盘。不再包含 state（恒 Idle）和 staging（恒空）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionFile {
    /// 格式版本号（3 = 本设计）
    pub format_version: u32,
    /// 会话元数据（动态字段由 Persister 在写入时计算）
    pub meta: SessionMeta,
    /// 已确认的 turn 历史
    pub committed_turns: Vec<Vec<AnnotatedMessage>>,
    /// 单调计数器
    pub turn_index: usize,
    pub total_usage: Usage,
    pub next_message_id: u64,
    /// 排队中的用户输入（turn 完成时可能仍有未处理的输入）
    pub pending_inputs: Vec<PendingInput>,
}
