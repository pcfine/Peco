//! 知识库管理模块 — AI Agent 的统一知识管理入口。
//!
//! 提供面向用户的人性化知识库操作：
//! - 创建/删除/列表知识库
//! - 自动增量同步（扫描 docs/ 目录 → 对比哈希 → 更新数据库）
//! - 多维度搜索（BM25 + 向量 + 图谱）
//! - Agent 工具暴露

pub mod config;
pub mod error;
pub mod hash_manifest;
pub mod manager;
pub mod sync;

pub use config::KnowledgeConfig;
pub use error::KnowledgeModuleError;
pub use manager::KnowledgeManager;
pub use sync::SyncReport;
