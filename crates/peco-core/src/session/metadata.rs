// ============================================================================
// 会话元数据
// ============================================================================
//
// SessionMeta 是持久化层概念，用于列表展示和索引。
// Session 本身直接持有 id、description、created_at 字段，不持有 SessionMeta。
// 持久化时由 SessionPersister 从 Session + SessionSnapshot 计算动态字段。

use serde::{Deserialize, Serialize};

/// 单个会话的元数据，由持久化层管理和存储。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    /// 会话唯一标识符（UUID v4）。
    pub id: String,
    /// 会话描述（用户可读的标签）。
    pub description: String,
    /// 累计消耗的 token 数量 — 由 Persister 在 save 时从 SessionSnapshot 计算。
    pub tokens_used: u64,
    /// 已完成的 turn 数量 — 由 Persister 在 save 时从 SessionSnapshot 计算。
    pub completed_turns: usize,
    /// 创建时间（Unix 时间戳，秒）。
    pub created_at: u64,
    /// 最后更新时间（Unix 时间戳，秒）— 由 Persister 在 save 时设置。
    pub updated_at: u64,
}

impl SessionMeta {
    /// 创建新的 SessionMeta，自动填充 id 和时间戳。
    pub fn new(description: String) -> Self {
        let now = crate::session::types::unix_timestamp_secs();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            description,
            tokens_used: 0,
            completed_turns: 0,
            created_at: now,
            updated_at: now,
        }
    }
}
