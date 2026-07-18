// ============================================================================
// SingleTurnExecutor — 单轮问答（含 ReAct tool calling）
// ============================================================================
//
// 内部使用 [`SimpleAgentLooper`]，一次调用完成。
// 自动处理 tool calling — 模型无 tool 需求时直接返回文本，
// 有 tool 需求时自动进入 ReAct 循环。
//
// 不保留历史，每次调用完全独立。
//
// 如需跨轮复用 Session，请使用 [`MultiTurnExecutor`]。
// 如需在 tool 中调用子 agent，请使用 [`AgentExecutorTool`]。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::{AgentExecutor, ExecutorError, ExecutorInput, ExecutorOutput, ExecutorType};
use crate::agent::agent::Agent;
use crate::agent::simple_looper::SimpleAgentLooper;

/// 单轮问答 / ReAct 执行器。
///
/// 每次 `execute()` 独立运行，使用 [`SimpleAgentLooper`] 完成
/// 一轮完整的模型交互（含 tool calling）。
///
/// # 示例
///
/// ```ignore
/// let agent = Arc::new(Agent::from_file("agents/assistant.md").await?);
/// let executor = SingleTurnExecutor::new(agent)
///     .with_timeout(Duration::from_secs(120));
/// let output = executor.execute(ExecutorInput::new("What is Rust?")).await?;
/// println!("{}", output.content);
/// ```
pub struct SingleTurnExecutor {
    agent: Arc<Agent>,
    max_turns: Option<usize>,
    timeout: Option<Duration>,
}

impl SingleTurnExecutor {
    /// 创建新的 SingleTurnExecutor。
    pub fn new(agent: Arc<Agent>) -> Self {
        Self {
            agent,
            max_turns: None,
            timeout: None,
        }
    }

    /// 设置最大 ReAct 轮数。
    ///
    /// 默认使用 agent profile 中的 max_turns 配置。
    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = Some(max_turns);
        self
    }

    /// 设置总超时时间。
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

#[async_trait]
impl AgentExecutor for SingleTurnExecutor {
    fn name(&self) -> &str {
        "single_turn"
    }

    fn executor_type(&self) -> ExecutorType {
        ExecutorType::SingleTurn
    }

    async fn execute(&self, input: ExecutorInput) -> Result<ExecutorOutput, ExecutorError> {
        let handle = SimpleAgentLooper::spawn(
            self.agent.clone(),
            input.prompt.clone(),
            self.max_turns,
        );

        let content = if let Some(timeout) = self.timeout {
            tokio::time::timeout(timeout, handle.wait())
                .await
                .map_err(|_| ExecutorError::Timeout)?
        } else {
            handle.wait().await
        }?;

        Ok(ExecutorOutput {
            content,
            usage: Default::default(),
            structured_data: None,
            turns: 0,
            success: true,
        })
    }
}
