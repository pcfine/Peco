// ============================================================================
// 持久化模块
// ============================================================================
//
// 独立的会话持久化层。Session 本身不感知持久化 — AgentLooper 在 turn 边界
// 调用 SessionPersister::save() 触发落盘。

pub mod file;
pub(crate) mod format;
pub mod traits;

pub use file::FileSessionPersister;
pub use traits::{NullSessionPersister, PersistError, PersistResult, SessionPersister};
