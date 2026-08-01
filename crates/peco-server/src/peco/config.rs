// ============================================================================
// PecoConfig — Peco 对话配置（可扩展，预留 PPA 钩子注入点）
// ============================================================================
//
// 首版所有可选字段为 None，后续接入 PPA 时只需填充对应字段。

use std::sync::Arc;
use std::time::Duration;

use peco_core::agent::hooks::LooperHook;
use peco_core::agent::{DynamicContext, LooperConfig, MessageFilter};

/// Peco 对话配置。
///
/// 首版所有可选字段为 None，后续接入 PPA 时只需填充对应字段。
#[derive(Clone)]
pub struct PecoConfig {
    /// 事件通道缓冲区大小
    pub event_buffer: usize,
    /// 每轮超时
    pub per_turn_timeout_secs: u64,
    /// 总超时
    pub total_timeout_secs: u64,
    /// 历史消息滑动窗口大小
    pub max_history_messages: usize,

    // ── 以下为 PPA / 可观测性钩子预留 ──────────────────────
    /// 动态上下文（读路径）：每次用户 query 前自动检索并注入。
    /// 后续接入 PPA 时设为 `Some(Arc::new(PpaDynamicContext::new(...)))`。
    pub dynamic_context: Option<Arc<dyn DynamicContext>>,
    /// Looper 钩子（写路径）：每轮完成后触发记忆提取、token 用量记录等。
    /// 钩子按注册顺序执行，相互独立。
    pub hooks: Vec<Arc<dyn LooperHook>>,
}

impl Default for PecoConfig {
    fn default() -> Self {
        Self {
            event_buffer: 256,
            per_turn_timeout_secs: 300,
            total_timeout_secs: 1800,
            max_history_messages: 10,
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
            dynamic_context: self.dynamic_context.clone(),
            hooks: self.hooks.clone(),
            message_filter: Some(message_filter),
            ..LooperConfig::default()
        }
    }
}
