/// 内置总管 Agent 的保留名。
///
/// 每个 workspace 都必须安装该 Agent；名字只在 crate 内部使用，
/// 外部通过 [`crate::workspace::WorkSpace::get_master_agent`] 获取实例。
pub(crate) const MASTER_AGENT_NAME: &str = "@master";

#[allow(clippy::module_inception)]
pub(crate) mod agent;
pub mod agent_config;
pub(crate) mod agent_looper;
pub(crate) mod agent_manager;
pub mod compaction;
pub(crate) mod context;
pub(crate) mod dynamic_context;
pub(crate) mod error;
pub mod hooks;
pub(crate) mod simple_looper;
mod stream;
#[cfg(test)]
pub(crate) mod test_support;

pub use agent::{Agent, MessageFilter, build_provider_with_user, supports_image_input_for};
pub use agent_config::{
    AgentIdentity, AgentProfile, AssembleAgentMdParams, LlmConfig, ModelConfig, ModelConfigBuilder,
    assemble_agent_md, parse_agent_md, resolve_api_key, split_frontmatter,
};
pub use agent_looper::{
    AgentLooper, LooperConfig, LooperEvent, LooperHandle, LooperState, RetryNoticeReason,
    TurnFailureReason, TurnOutcome, UserMsg,
};
pub use agent_manager::AgentManager;
pub use agent_manager::AgentMeta;
pub use compaction::{
    BoundaryReduction, CompactionOutcome, CompactionPolicy, ModelSummarizer, SUMMARY_CLOSE,
    SUMMARY_OPEN, SUMMARY_TOKEN_CAP, StripEvaluation, StripOutcome, StripPolicy,
    TRANSCRIPT_ITEM_MAX_CHARS, TRANSCRIPT_TOOL_OUTPUT_MIN_CAP, TurnSummarizer,
    strip_summary_wrapper,
};
pub use context::{
    estimate_item_tokens, estimate_str_tokens, estimate_viewable_item_tokens, is_viewable_item,
};
pub use dynamic_context::DynamicContext;
pub use error::AgentError;
pub use hooks::{HookAction, LooperHook, TokenBudgetHook, ToolAllowlistHook, ToolHookAction};
pub use simple_looper::{SimpleAgentLooper, SimpleLooperHandle, SimpleLooperOutput};
pub use stream::{ModelStream, ModelStreamEvent};
