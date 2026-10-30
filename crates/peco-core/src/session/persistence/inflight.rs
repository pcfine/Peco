// ============================================================================
// InflightCheckpoint — 在途轮检查点
// ============================================================================
//
// 进程崩溃时，内存里「已落地但未 commit」的那一轮会整体消失。检查点把它
// 落一次盘，重启后由宿主层水化进 Session 并冻结入史（见 `Session::hydrate_inflight`）。
//
// 只在**一批工具刚落地**的时刻写（`finish_tool_execution`）——那是本设计里
// 副作用已经发生、最不该丢的时刻。随后的模型调用不产生副作用，不必再写。

use serde::{Deserialize, Serialize};

use crate::session::AnnotatedMessage;

/// 崩溃恢复轮进入历史时使用的中断原因。
///
/// 进程内中断（取消 / 超时 / 失败）走 `plan_failure`，用的是各自的真实原因；
/// 本常量只用于「检查点还在、但收尾从没跑完」这一种情形。
pub const INFLIGHT_CRASH_REASON: &str = "crashed";

/// 在途轮检查点。
///
/// `staged` 是 `Session` 中 staging 的全量克隆（user_input 在前），与
/// `StagingBuffer::take_all()` 同序 —— 水化时按同一约定还原。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InflightCheckpoint {
    /// 会话 ID（= conversation_id）。
    pub session_id: String,
    /// 写入时的 `Session::turn_index`。
    ///
    /// 水化时据此判陈旧：收尾成功但删除失败的残留行，其轮次编号会小于
    /// 会话当前值，灌进去会与既有历史错位。
    pub turn_index: usize,
    /// 水化时写进历史的中断原因（人类可读）。
    pub reason: String,
    /// staging 的全部消息（user_input + 已落地产物），按序。
    pub staged: Vec<AnnotatedMessage>,
}
