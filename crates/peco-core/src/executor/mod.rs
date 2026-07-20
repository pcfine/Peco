// ============================================================================
// executor — AgentExecutor trait + 类型定义
// ============================================================================
//
// AgentExecutor 是 AgentLooper 之上的外观层，提供多种执行模式。
// 内部只使用 AgentLooper::spawn() + LooperHandle，不直接调用 Agent::chat()。
//
// 模块结构:
// - mod.rs           本文件 — trait + 公共类型 + re-exports
// - single_turn.rs   SingleTurnExecutor
// - multi_turn.rs    MultiTurnExecutor
// - react.rs         ReActExecutor
// - tool.rs          AgentExecutorTool (impl ToolDyn)

mod multi_turn;
mod single_turn;
mod structured_output;
mod tool;

pub use multi_turn::MultiTurnExecutor;
pub use single_turn::SingleTurnExecutor;
pub use structured_output::StructuredOutputExecutor;
pub use tool::AgentExecutorTool;

use std::sync::Arc;

use async_trait::async_trait;
use model_provider::{Message, Usage};

use crate::agent::error::AgentError;

// ============================================================================
// ExecutorType
// ============================================================================

/// 执行器类型标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorType {
    /// 单轮问答，临时 Session
    SingleTurn,
    /// 多轮对话，复用 Session
    MultiTurn,
    /// 结构化输出（schema + 重试）
    StructuredOutput,
    /// 链式编排
    Chain,
    /// 路由分发
    Router,
    /// 并行执行
    Parallel,
}

// ============================================================================
// ExecutorInput / ExecutorOutput
// ============================================================================

/// 执行器输入。
#[derive(Debug, Clone)]
pub struct ExecutorInput {
    /// 用户 prompt 文本
    pub prompt: String,
    /// 注入到临时 Session 的上下文消息（不含 system prompt）
    pub context: Vec<Arc<Message>>,
    /// 结构化输出 schema（StructuredOutputExecutor 使用，Phase 2）
    pub output_schema: Option<serde_json::Value>,
}

impl ExecutorInput {
    /// 创建仅包含 prompt 的简单输入。
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            context: Vec::new(),
            output_schema: None,
        }
    }

    /// 创建带上下文消息的输入。
    pub fn with_context(prompt: impl Into<String>, context: Vec<Arc<Message>>) -> Self {
        Self {
            prompt: prompt.into(),
            context,
            output_schema: None,
        }
    }

    /// 创建带输出 schema 的输入，供 [`StructuredOutputExecutor`] 使用。
    pub fn with_schema(prompt: impl Into<String>, output_schema: serde_json::Value) -> Self {
        Self {
            prompt: prompt.into(),
            context: Vec::new(),
            output_schema: Some(output_schema),
        }
    }
}

/// 执行器输出。
#[derive(Debug, Clone)]
pub struct ExecutorOutput {
    /// 模型最终文本回复
    pub content: String,
    /// Token 用量（SimpleAgentLooper 路径为 Default）
    pub usage: Usage,
    /// 结构化数据（仅 StructuredOutputExecutor 填充，Phase 2）
    pub structured_data: Option<serde_json::Value>,
    /// 执行的 ReAct 轮数（SimpleAgentLooper 路径为 0）
    pub turns: usize,
    /// 是否成功完成
    pub success: bool,
}

// ============================================================================
// ExecutorError
// ============================================================================

/// AgentExecutor 错误类型。
///
/// 实现了 `std::error::Error + Send + Sync + 'static`，
/// 可被 `ToolError::ToolCallError` 包装。
#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    /// Agent 内部错误
    #[error("agent error: {0}")]
    Agent(#[from] AgentError),

    /// Schema 解析失败（StructuredOutputExecutor，Phase 2）
    #[error("schema parse failed after {retries} retries: {message}")]
    Schema { retries: usize, message: String },

    /// 超时
    #[error("timeout")]
    Timeout,

    /// 被取消
    #[error("cancelled")]
    Cancelled,

    /// 缺少 Session（MultiTurnExecutor 需要）
    #[error("session required for multi-turn execution")]
    SessionRequired,

    /// Looper 意外退出
    #[error("looper exited unexpectedly: {0}")]
    LooperExited(String),

    /// 链式步骤失败（ChainExecutor，Phase 2）
    #[error("step {step} failed: {error}")]
    ChainStep {
        step: usize,
        error: Box<ExecutorError>,
    },
}

// ============================================================================
// AgentExecutor trait
// ============================================================================

/// AgentExecutor — AgentLooper 之上的外观层。
///
/// Agent 和 Session 在 executor 构造时绑定，`execute()` 只需传入
/// [`ExecutorInput`]。编排型 executor（Chain、Router、Parallel）在
/// 构造时持有各自的 step agent。
///
/// 所有执行器内部只使用 [`AgentLooper::spawn()`] + [`LooperHandle`]。
///
/// [`AgentLooper::spawn()`]: crate::agent::AgentLooper::spawn
/// [`LooperHandle`]: crate::agent::LooperHandle
#[async_trait]
pub trait AgentExecutor: Send + Sync {
    /// 执行器名称。
    fn name(&self) -> &str;

    /// 执行器类型。
    fn executor_type(&self) -> ExecutorType;

    /// 执行任务并返回结果。
    ///
    /// # 内部流程
    ///
    /// 1. `AgentLooper::spawn(agent, session, config)` → `LooperHandle`
    /// 2. 通过 `LooperHandle` 驱动 looper
    /// 3. 收集事件直到完成
    /// 4. 组装 [`ExecutorOutput`]
    async fn execute(&self, input: ExecutorInput) -> Result<ExecutorOutput, ExecutorError>;
}
