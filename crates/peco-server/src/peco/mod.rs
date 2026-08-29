// ============================================================================
// Peco 模块 — 统一永续对话入口
// ============================================================================
//
// 替代 personal_agent 模块，成为唯一的永续对话 API。
// 预留 PPA 钩子注入点（DynamicContext + LooperHook），后续不经 handler 改动即可接入。

pub mod config;
pub mod environment;
pub mod filter;
pub mod handler;
pub mod manager;
pub mod memory;
pub mod session;
