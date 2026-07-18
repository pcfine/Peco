// ============================================================================
// PPA (Peco Personal Assistant) — 私人助理模块
// ============================================================================
//
// 模块结构:
//   config.rs    — PpaConfig 配置结构
//   types.rs     — MemoryFact, UserProfile, QueryType 等数据类型
//   store.rs     — PersonalMemoryStore (知识库 CRUD 封装)
//   classifier.rs    — QueryClassifier (规则引擎查询分类)
//   analyzer.rs      — MemoryAnalyzer (LLM 驱动记忆提取)
//   dynamic_context.rs — PpaDynamicContext (读路径: DynamicContext)
//   hook.rs           — PpaMemoryHook (写路径: LooperHook)
//   tools.rs          — remember / recall / forget 工具

pub mod analyzer;
pub mod classifier;
pub mod config;
pub mod dynamic_context;
pub mod hook;
pub mod store;
pub mod tools;
pub mod types;

// Re-exports
pub use config::PpaConfig;
pub use dynamic_context::PpaDynamicContext;
pub use hook::PpaMemoryHook;
pub use store::PersonalMemoryStore;
pub use tools::{ForgetTool, RecallTool, RememberTool};
