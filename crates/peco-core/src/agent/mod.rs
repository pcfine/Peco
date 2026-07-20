#[allow(clippy::module_inception)]
pub(crate) mod agent;
pub mod agent_config;
pub(crate) mod agent_looper;
pub(crate) mod context;
pub(crate) mod dynamic_context;
pub(crate) mod error;
pub mod hooks;
pub(crate) mod simple_looper;
mod stream;

pub use agent::{Agent, MessageFilter};
pub use agent_config::{
    AgentIdentity, AgentProfile, AssembleAgentMdParams, LlmConfig, ModelConfig, ModelConfigBuilder,
    assemble_agent_md, parse_agent_md, resolve_api_key, split_frontmatter,
};
pub use agent_looper::{
    AgentLooper, LooperConfig, LooperEvent, LooperHandle, OuterState, ReActState,
    TurnFailureReason, TurnOutcome, UserMsg,
};
pub use dynamic_context::DynamicContext;
pub use error::AgentError;
pub use hooks::{HookAction, LooperHook, TokenBudgetHook, ToolAllowlistHook, ToolHookAction};
pub use simple_looper::{SimpleAgentLooper, SimpleLooperHandle};
pub use stream::{ModelStream, ModelStreamEvent};
