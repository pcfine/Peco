// ============================================================================
// Assistant 模块 — 个人助理 API
// ============================================================================
//
// 提供：
// - [`PersonalAssistantManager`] — 个人助理生命周期管理器
// - [`PERSONAL_ASSISTANT_ID`] / [`PERSONAL_ASSISTANT_AGENT_NAME`] — 全局常量
//
// 与 Chat 模块的区别：
// - 单例 Agent + 单 Session（不区分"对话"）
// - 默认首页体验，用户登录即进入
// - 自动注入用户 Profile + 记忆上下文

pub mod manager;

// Re-exports
pub use manager::{
    PersonalAssistantManager, PersonalAssistantMessageFilter,
    PERSONAL_ASSISTANT_AGENT_NAME, PERSONAL_ASSISTANT_ID,
};
