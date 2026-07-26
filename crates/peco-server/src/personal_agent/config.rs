// ============================================================================
// PersonalAgentConfig — 个人助理模块最小配置
// ============================================================================
//
// 与 PpaConfig 不同：无 DynamicContext、无 MemoryHook 配置。
// 记忆由 @assistant → @memory 协作管理，不需要独立 LLM 分析。

/// 个人助理 Agent 配置。
///
/// 固定使用 `personal` 模板中的 `@assistant` 和 `@memory` agent，
/// 因此只需声明 agent 名称常量。
#[derive(Debug, Clone)]
pub struct PersonalAgentConfig {
    /// 主助理 Agent 名称（模板中固定为 "@assistant"）。
    pub assistant_agent_name: String,
    /// 记忆子 Agent 名称（模板中固定为 "@memory"）。
    pub memory_agent_name: String,
}

impl Default for PersonalAgentConfig {
    fn default() -> Self {
        Self {
            assistant_agent_name: "@assistant".to_string(),
            memory_agent_name: "@memory".to_string(),
        }
    }
}
