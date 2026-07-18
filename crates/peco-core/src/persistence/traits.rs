// ============================================================================
// SessionPersister trait + 辅助类型
// ============================================================================

use std::path::PathBuf;

use async_trait::async_trait;

use crate::session::{SessionMeta, SessionSnapshot};

// ============================================================================
// PersistResult / PersistError
// ============================================================================

/// 持久化操作的结果。
#[derive(Debug)]
pub struct PersistResult {
    pub bytes_written: u64,
    pub path: PathBuf,
}

/// 持久化操作错误。
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("persistence I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("persistence serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("unsupported session format version: {0}")]
    UnsupportedFormatVersion(u32),

    #[error("unknown session format")]
    UnknownFormat,

    #[error("invalid session ID: {0}")]
    InvalidId(String),
}

// ============================================================================
// SessionPersister trait
// ============================================================================

/// 会话持久化的异步抽象。
///
/// 实现者负责：
/// - 将 SessionSnapshot 序列化到存储后端
/// - 从存储后端读取并重建 SessionSnapshot + SessionMeta
/// - 管理存储生命周期（列出、删除）
///
/// Persister 在 `save()` 时自行计算 `SessionMeta` 的动态字段
/// （`updated_at`、`tokens_used`、`completed_turns`），
/// 因此 Session 本身不需要持有 SessionMeta。
#[async_trait]
pub trait SessionPersister: Send + Sync {
    /// 保存会话快照。
    ///
    /// Persister 从参数中构造 `SessionMeta` 的动态字段。
    async fn save(
        &self,
        snapshot: &SessionSnapshot,
        session_id: &str,
        description: &str,
        created_at: u64,
    ) -> Result<PersistResult, PersistError>;

    /// 按 ID 加载会话快照。
    ///
    /// 返回 `Ok(None)` 表示该 ID 的会话不存在。
    async fn load(
        &self,
        session_id: &str,
    ) -> Result<Option<(SessionSnapshot, SessionMeta)>, PersistError>;

    /// 删除会话。
    async fn delete(&self, session_id: &str) -> Result<(), PersistError>;

    /// 列出全部会话元数据，按更新时间降序。
    async fn list(&self) -> Result<Vec<SessionMeta>, PersistError>;
}

// ============================================================================
// NullSessionPersister
// ============================================================================

/// 空持久化实现 — 所有操作均为 no-op。
///
/// 用于临时/一次性会话（ReActExecutor、SingleTurnExecutor、sub_agent）。
pub struct NullSessionPersister;

#[async_trait]
impl SessionPersister for NullSessionPersister {
    async fn save(
        &self,
        _snapshot: &SessionSnapshot,
        _session_id: &str,
        _description: &str,
        _created_at: u64,
    ) -> Result<PersistResult, PersistError> {
        Ok(PersistResult {
            bytes_written: 0,
            path: PathBuf::new(),
        })
    }

    async fn load(
        &self,
        _session_id: &str,
    ) -> Result<Option<(SessionSnapshot, SessionMeta)>, PersistError> {
        Ok(None)
    }

    async fn delete(&self, _session_id: &str) -> Result<(), PersistError> {
        Ok(())
    }

    async fn list(&self) -> Result<Vec<SessionMeta>, PersistError> {
        Ok(Vec::new())
    }
}
