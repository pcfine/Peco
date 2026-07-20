// ============================================================================
// Personal Memory 模块 — PPA 个人记忆系统
// ============================================================================
//
// 从 peco-server 移入，纯逻辑组件，不依赖 HTTP/DB。
// 提供：
// - [`MemoryFact`], [`MemoryCategory`], [`Importance`] — 数据类型
// - [`PersonalMemoryStore`] — 记忆存储
// - [`StorageConfig`] — 存储配置

mod config;
mod store;
mod types;

pub use config::StorageConfig;
pub use store::PersonalMemoryStore;
pub use types::{
    Importance, MemoryCategory, MemoryFact, MemoryOperation, UserPreferences, UserProfile,
};
