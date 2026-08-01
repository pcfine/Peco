// ============================================================================
// Assistant 模块 — 个人助理 API
// ============================================================================
//
// ⚠️ DEPRECATED since 0.2.0 — 本模块不再被任何路由使用。
//
// 保留原因：
// - `PersonalAssistantManager` 的 PPA 集成模式（DynamicContext + MemoryHook +
//   MessageFilter 三位一体）是设计参考，后续可能迁移到 peco 模块
// - `PersonalAssistantMessageFilter` 的区分当前轮/历史轮的策略比 personal_agent 更精细
// - `build_ppa_components()` 展示了如何组装 PPA 读/写路径
//
// 当前活跃的聊天入口：`crate::peco::handler`
//
// 提供：
// - [`PersonalAssistantManager`] — 个人助理生命周期管理器
// - [`PERSONAL_ASSISTANT_ID`] / [`PERSONAL_ASSISTANT_AGENT_NAME`] — 全局常量
//
// 与 Chat 模块的区别：
// - 单例 Agent + 单 Session（不区分"对话"）
// - 默认首页体验，用户登录即进入
// - 自动注入用户 Profile + 记忆上下文

#![allow(dead_code)]

pub mod manager;

// Re-exports
pub use manager::{
    PERSONAL_ASSISTANT_AGENT_NAME, PERSONAL_ASSISTANT_ID, PersonalAssistantManager,
    PersonalAssistantMessageFilter,
};
