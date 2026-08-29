// ============================================================================
// PecoConfig — Peco 对话配置
// ============================================================================
//
// 可选字段（compaction / environment / dynamic_context / hooks）由
// PecoManager 构造期填充 — handler 无需改动即可增减注入组件。

use std::sync::Arc;
use std::time::Duration;

use peco_core::agent::hooks::LooperHook;
use peco_core::agent::{CompactionPolicy, DynamicContext, LooperConfig, MessageFilter};

use super::memory::MemoryConfig;

/// Peco 对话配置。
#[derive(Clone)]
pub struct PecoConfig {
    /// 事件通道缓冲区大小
    pub event_buffer: usize,
    /// 每轮超时
    pub per_turn_timeout_secs: u64,
    /// 总超时
    pub total_timeout_secs: u64,
    /// 历史轮 verbatim 保留区 token 预算（不含当前轮与 pinned 摘要）。
    ///
    /// 口径：仅统计轮内 viewable 条目（User / Assistant 文本），
    /// 不含 tool 输出与 reasoning。与 [`Self::compaction_trigger_tokens`]
    /// 的「全量轮」口径不同 — 两个数值不可直接比较。
    ///
    /// 取代早期的 `max_history_messages` 消息条数窗口 — 中文场景下
    /// 按条数截断的 token 波动过大，预算控制必须基于校准 token 估算。
    pub history_token_budget: usize,
    /// 上下文滚动压缩触发阈值（估算 token）。
    ///
    /// 口径：pinned 摘要 + 全部 committed 轮的**全量** token
    /// （含 tool 输出与 reasoning）。与 [`Self::history_token_budget`]
    /// 的「仅 viewable 文本」口径不同。
    pub compaction_trigger_tokens: usize,
    /// 压缩后 verbatim 保留区目标 token。
    pub compaction_keep_recent_tokens: usize,
    /// 摘要模型名（Flash 档，低延迟低成本）。
    pub summarizer_model: String,
    /// 记忆双路径配置（写路径提取 hook + 读路径召回）。
    pub memory: MemoryConfig,

    // ── 以下由 PecoManager 构造期填充 ──────────────────────
    /// 上下文滚动压缩策略。由 `PecoManager` 基于主 Agent 的 provider 构建。
    pub compaction: Option<Arc<CompactionPolicy>>,
    /// 环境上下文（恒定前缀）：用户身份、工作空间路径、日期平台等。
    /// 由 `PecoManager` 在构造时经 `EnvironmentInfo::render()` 求值一次填入。
    pub environment: Option<String>,
    /// 动态上下文（读路径）：每次用户 query 前自动检索并注入。
    /// 记忆体系装配为 `MemoryRecallContext`（见 `super::memory`）。
    pub dynamic_context: Option<Arc<dyn DynamicContext>>,
    /// Looper 钩子（写路径）：每轮完成后触发记忆提取、token 用量记录等。
    /// 记忆体系装配为 `MemoryExtractionHook`（见 `super::memory`）。
    /// 钩子按注册顺序执行，相互独立。
    pub hooks: Vec<Arc<dyn LooperHook>>,
}

impl Default for PecoConfig {
    fn default() -> Self {
        Self {
            event_buffer: 256,
            per_turn_timeout_secs: 7200,
            total_timeout_secs: 7200,
            history_token_budget: 128_000,
            compaction_trigger_tokens: 256_000,
            compaction_keep_recent_tokens: 96_000,
            summarizer_model: "deepseek-v4-flash".to_string(),
            memory: MemoryConfig::default(),
            compaction: None,
            environment: None,
            dynamic_context: None,
            hooks: Vec::new(),
        }
    }
}

impl PecoConfig {
    /// 从 PecoConfig 构建 LooperConfig。
    pub fn to_looper_config(&self, message_filter: Arc<dyn MessageFilter>) -> LooperConfig {
        LooperConfig {
            event_buffer: self.event_buffer,
            per_turn_timeout: Some(Duration::from_secs(self.per_turn_timeout_secs)),
            total_timeout: Some(Duration::from_secs(self.total_timeout_secs)),
            persist_on_failure: true,
            environment: self.environment.clone(),
            dynamic_context: self.dynamic_context.clone(),
            hooks: self.hooks.clone(),
            message_filter: Some(message_filter),
            compaction: self.compaction.clone(),
            ..LooperConfig::default()
        }
    }
}
