// ============================================================================
// 会话管理模块
// ============================================================================
//
// 提供 AI Agent 会话的创建、加载和状态管理能力。
//
// 模块结构：
// - `types` — 核心类型定义（AnnotatedMessage, MessageSource, SessionState 等）
// - `buffer` — 分层消息缓冲区（CommittedBuffer + StagingBuffer）
// - `session` — Session struct（零锁单线程版本）
// - `snapshot` — SessionSnapshot + TurnBoundaryToken（持久化快照）
// - `metadata` — SessionMeta（持久化层概念，Session 本身不持有）
// - `error` — SessionError 错误类型

pub mod buffer;
pub mod error;
pub mod metadata;
pub mod session;
pub mod snapshot;
pub mod types;

// 公共 API 导出
pub use error::SessionError;
pub use metadata::SessionMeta;
pub use session::Session;
pub use snapshot::{SessionSnapshot, TurnBoundaryToken};
pub use types::{
    AnnotatedMessage, InputPriority, MessageId, MessageSource, PendingInput, SessionState,
};
