// ============================================================================
// Personal Agent 模块 — 模板化个人助理 API
// ============================================================================
//
// ⚠️ DEPRECATED since 0.2.0 — 本模块已合并到 `crate::peco`。
//
// 保留原因：
// - `PersonalAgentMessageFilter` 逻辑已移入 peco 模块
// - `session.rs` 中的私有会话 ID 生成逻辑已移入 peco 模块
// - 作为从 peco-agents 模板加载 Agent 的参考实现
//
// 与 assistant 模块的区别：
//   - 无 PPA 组件（不自行管理记忆，由 @assistant → @memory 协作完成）
//   - Agent 来自 peco-agents 内置 personal 模板（通过 WorkSpace 初始化）
//   - Per-user perpetual session（session_id = {user_id}-private-session）
//   - 独立的 MessageFilter（当前轮完整，历史轮仅 User + 纯文本 Assistant）
//
// 与 chat 模块的区别：
//   - 无 conversation CRUD，固定单一 perpetual session
//   - 不复用 PPA filter 代码

#![allow(dead_code)]

pub mod config;
pub mod handler;
pub mod manager;
