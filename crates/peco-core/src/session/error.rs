// ============================================================================
// 会话管理错误类型
// ============================================================================

use super::types::SessionState;

/// 会话持久化和操作过程中的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// 文件 I/O 错误（读取、写入、删除会话文件时发生）。
    #[error("session I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON 序列化 / 反序列化错误。
    #[error("session serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// 指定 ID 的会话未在磁盘上找到。
    #[error("session not found: {0}")]
    NotFound(String),

    /// 无效的会话 ID（空字符串、包含路径分隔符、路径穿越等）。
    #[error("invalid session ID: {0}")]
    InvalidId(String),

    /// 非法的状态转换。
    #[error("invalid state transition: cannot {action} while in {current_state:?} state")]
    InvalidStateTransition {
        /// 当前状态
        current_state: SessionState,
        /// 尝试的操作
        action: String,
    },

    /// 不支持的持久化格式版本。
    #[error("unsupported session format version: {0}")]
    UnsupportedFormatVersion(u32),

    /// 未知的持久化格式。
    #[error("unknown session format")]
    UnknownFormat,

    /// Turn 索引越界。
    #[error("turn index out of bounds: requested {requested}, valid range 0..{max}")]
    TurnOutOfBounds {
        /// 请求的 turn 索引
        requested: usize,
        /// 有效范围上限
        max: usize,
    },
}
