// ============================================================================
// SimpleAgentLooper — batch-only ReAct executor for single-shot sub-agent tasks
// ============================================================================
//
// Unlike [`AgentLooper`], this is intentionally minimal:
// - No user input channel (single prompt → single result)
// - No streaming (batch mode only)
// - No hooks
// - No event broadcasting
// - No session persistence
// - No pause/resume
//
// The only shared state is a cancel flag (Arc<AtomicBool>), checked at each
// loop iteration boundary.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use model_provider::{
    Content, ContentBlock, FinishReason, InputItem, ResponseStatus, Role, ToolCall,
};

use super::agent::{Agent, override_output_budget};
use super::error::AgentError;
use crate::artifacts::ArtifactDecl;
use crate::tools::ToolExecutor;

/// Concurrent tool execution result: (index, tool_call, output).
type ToolExecResult = (usize, ToolCall, Result<Content, String>);
type ToolExecHandle = tokio::task::JoinHandle<ToolExecResult>;
type SimpleTaskHandle = tokio::task::JoinHandle<Result<SimpleLooperOutput, AgentError>>;
type SharedSimpleTask = Arc<tokio::sync::Mutex<Option<SimpleTaskHandle>>>;

/// 子 agent 截断重试时的输出预算抬升目标（`None` → 本值）。
///
/// 与 [`AgentLooper`](super::agent_looper::AgentLooper) 的
/// `LooperConfig::retry_output_budget` 默认值一致：deepseek 服务端默认输出上限
/// 为 4096，推理 token 与可见输出**共用**这一预算 —— 重负载任务（如 `@memory`
/// 的 `[ORGANIZE]` 扫描上百条文档）会把预算全耗在 reasoning 上，可见输出被挤成
/// 空。抬到本值后模型有足够空间产出最终回答（实测同一模型可输出 > 7900 tokens）。
const SUB_AGENT_TRUNCATION_RETRY_BUDGET: u32 = 32_768;

// ============================================================================
// SimpleLooperOutput — 正文 + 沿途收集的产物声明
// ============================================================================

/// 一次 `SimpleAgentLooper` 运行的产出。
///
/// 工具输出里的 `@@PECO_ARTIFACT` 声明已从文本剥离，只留在 `artifacts` 里：
/// 丢弃它等于吞掉产物，而宿主层（peco-server 的 delegate/parallel 分支）正是
/// 靠这些声明才拿得到落库候选。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SimpleLooperOutput {
    /// 最终助手正文（已剥离声明行）。
    pub text: String,
    /// 本次运行中工具输出声明的产物，按收集顺序。
    pub artifacts: Vec<ArtifactDecl>,
}

impl SimpleLooperOutput {
    /// 无落库能力的消费方统一走这里（core 内唯一丢弃点）。
    ///
    /// 归一到一个出口而不是各调用点各判一次：新增消费方时漏判就会静默吞掉
    /// 声明，而这里至少留下一条 warn 供追查。
    pub(crate) fn into_text(self, consumer: &'static str) -> String {
        if !self.artifacts.is_empty() {
            tracing::warn!(
                consumer,
                count = self.artifacts.len(),
                "Artifact declarations dropped: consumer cannot persist artifacts"
            );
        }
        self.text
    }
}

// ============================================================================
// SimpleAgentLooper — internal
// ============================================================================

/// Minimal, batch-only ReAct executor for single-shot sub-agent tasks.
///
/// Created via [`SimpleAgentLooper::spawn`], which returns a
/// [`SimpleLooperHandle`] for cancel + wait.
pub struct SimpleAgentLooper {
    /// The assembled Agent (model + tools + MCP).
    agent: Arc<Agent>,
    /// Maximum model-call iterations before forcing failure.
    max_iterations: usize,

    /// Accumulated message history.
    ///
    /// System prompt is NOT stored here — [`Agent::generate`] injects it on each
    /// call as `instructions`. This vec contains User / Assistant / FunctionCall /
    /// FunctionCallOutput [`InputItem`]s only.
    messages: Vec<Arc<InputItem>>,

    /// Model calls made so far in this run. Checked against `max_iterations`.
    react_loop_iteration: usize,

    /// External cancel signal shared with [`SimpleLooperHandle`].
    cancel_flag: Arc<AtomicBool>,

    /// 可选的自定义 ToolExecutor，覆盖 agent 内置的执行器。
    /// 设置后，工具定义获取和执行均使用此执行器。
    /// 由 [`StructuredOutputExecutor`](crate::executor::StructuredOutputExecutor) 使用。
    tool_executor_override: Option<Arc<dyn ToolExecutor>>,

    /// 工具输出中收集到的产物声明（声明行已从文本剥离）。
    ///
    /// 只有成功出口会把它们交出去；失败出口由 [`spawn_looper`] 统一告警 ——
    /// 收集过程中不能报「丢弃」，此时还不知道最终成功与否。
    artifacts: Vec<ArtifactDecl>,
}

impl SimpleAgentLooper {
    /// Spawn a single-shot agent execution as a background tokio task.
    ///
    /// Returns a [`SimpleLooperHandle`] that can cancel or wait for the result.
    ///
    /// # Arguments
    ///
    /// * `agent` — The assembled Agent instance.
    /// * `prompt` — The task description / user query.
    /// * `max_iterations` — Override for `agent.max_iterations()`. Pass `None` to use
    ///   the agent's configured value.
    pub fn spawn(
        agent: Arc<Agent>,
        prompt: String,
        max_iterations: Option<usize>,
    ) -> SimpleLooperHandle {
        Self::spawn_looper(agent, prompt, max_iterations, None)
    }

    /// 使用自定义 [`ToolExecutor`] 启动（覆盖 agent 内置的）。
    ///
    /// 供 [`StructuredOutputExecutor`](crate::executor::StructuredOutputExecutor)
    /// 注入 `__submit_output__` 工具使用，其他行为与 [`spawn`](SimpleAgentLooper::spawn) 一致。
    pub fn spawn_with_executor(
        agent: Arc<Agent>,
        prompt: String,
        tool_executor: Arc<dyn ToolExecutor>,
        max_iterations: Option<usize>,
    ) -> SimpleLooperHandle {
        Self::spawn_looper(agent, prompt, max_iterations, Some(tool_executor))
    }

    /// 两个公开入口的公共实现。
    fn spawn_looper(
        agent: Arc<Agent>,
        prompt: String,
        max_iterations: Option<usize>,
        tool_executor_override: Option<Arc<dyn ToolExecutor>>,
    ) -> SimpleLooperHandle {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let max_iterations = max_iterations.unwrap_or_else(|| agent.max_iterations());

        let mut looper = SimpleAgentLooper {
            agent,
            max_iterations,
            messages: Vec::new(),
            react_loop_iteration: 0,
            cancel_flag: cancel_flag.clone(),
            tool_executor_override,
            artifacts: Vec::new(),
        };

        let join_handle = tokio::spawn(async move {
            let res = looper.run(prompt).await;
            // `run()` 有若干条不经 collect 的 `?` 错误出口（模型调用失败、
            // 响应未完成、取消、迭代耗尽），声明此时只存在于 looper 内部。
            // 丢弃告警收敛在这里而不是 `execute_tools`：那里还不知道成败，
            // 两边都报会让同一次失败出两条 warn。
            if res.is_err() && !looper.artifacts.is_empty() {
                tracing::warn!(
                    count = looper.artifacts.len(),
                    "Artifact declarations discarded with failed sub-agent run"
                );
            }
            res
        });
        let abort_handle = join_handle.abort_handle();

        SimpleLooperHandle {
            cancel_flag,
            join_handle: Arc::new(tokio::sync::Mutex::new(Some(join_handle))),
            abort_handle,
        }
    }

    // ── Core loop ──────────────────────────────────────────────────────────

    /// Execute the ReAct loop and return the final assistant text plus artifacts.
    async fn run(&mut self, prompt: String) -> Result<SimpleLooperOutput, AgentError> {
        // Ensure deferred MCP connections are established before first tool use.
        self.agent.mcp_manager().ensure_connected().await;

        // Build initial message list: [User(prompt)]
        self.messages.push(Arc::new(InputItem::Message {
            role: Role::User,
            content: prompt.into(),
        }));

        // 本轮请求的输出预算覆盖：截断重试时抬高（粘性到本轮结束）。
        // 与 [`AgentLooper`](super::agent_looper::AgentLooper) 的 `budget_raised`
        // 语义一致 —— 抬过一次后生效预算恒为该值，第二次截断不再重发（逐字节相同）。
        let mut output_budget_override: Option<u32> = None;

        loop {
            // ── Check cancel ──────────────────────────────────────────────
            if self.cancel_flag.load(Ordering::Acquire) {
                return Err(AgentError::AgentProtocol("cancelled".into()));
            }

            // ── Check max_iterations ───────────────────────────────────────────
            if self.react_loop_iteration >= self.max_iterations {
                return Err(AgentError::MaxIterations {
                    max_iterations: self.max_iterations,
                });
            }
            self.react_loop_iteration += 1;

            // ── Model call (batch, non-streaming) ─────────────────────────
            // System prompt is not stored in history — injected as `instructions`
            // on each call (via `Agent::generate` / `generate_with_tools`).
            let instructions = Some(self.agent.system_prompt());

            // 如果有自定义执行器则使用它获取工具定义，否则用 agent 默认的
            let response = if let Some(ref executor) = self.tool_executor_override {
                let tools = executor.definitions();
                self.agent
                    .generate_with_tools(
                        self.messages.clone(),
                        instructions,
                        tools,
                        override_output_budget(output_budget_override),
                    )
                    .await?
            } else {
                self.agent
                    .generate_full(
                        self.messages.clone(),
                        override_output_budget(output_budget_override),
                    )
                    .await?
            };

            // ── 状态收敛：Incomplete / Failed 都是异常终止 ──────────────────
            // SimpleAgentLooper 是 batch-only，必须显式处理非 Completed 响应：
            // 截断（Incomplete + MaxTokens）时可见输出常被 reasoning 挤空，
            // 若直接按「无 tool_calls → 返回 text」处理，子 agent 会**静默返回
            // 空串**（`@memory` `[ORGANIZE]` 的真实故障），既无重试也无归因。
            if response.status != ResponseStatus::Completed {
                // 截断重试：抬输出预算重发一次。两个前提缺一不可——
                // ① 「抬得动」：当前生效预算（一次性覆盖 → agent 配置 → 未设按 0）
                //    已达抬升目标时，重发与上一次逐字节相同，必然同样截断，纯白烧
                //    一次调用。判据与 `AgentLooper::can_retry` 第 4 条一致。
                // ② 还有迭代余量：否则状态机刚回到循环顶就撞 MaxIterations，把
                //    「截断」这个真实原因换成「超出迭代次数」，诊断反而变差。
                let effective_budget = output_budget_override
                    .or(self.agent.model_config().max_tokens)
                    .unwrap_or(0);
                if response.status == ResponseStatus::Incomplete
                    && matches!(response.finish_reason, Some(FinishReason::MaxTokens))
                    && effective_budget < SUB_AGENT_TRUNCATION_RETRY_BUDGET
                    && self.react_loop_iteration < self.max_iterations
                {
                    tracing::warn!(
                        truncated_output_tokens = response.usage.output_tokens,
                        retry_output_budget = SUB_AGENT_TRUNCATION_RETRY_BUDGET,
                        iteration = self.react_loop_iteration,
                        "Sub-agent response truncated (max_tokens); retrying with raised output budget"
                    );
                    output_budget_override = Some(SUB_AGENT_TRUNCATION_RETRY_BUDGET);
                    continue;
                }

                let reason = response
                    .finish_reason
                    .map_or_else(|| "-".to_string(), |r| r.as_str().to_string());
                let detail = response
                    .error
                    .as_ref()
                    .map(|e| e.message.clone())
                    .unwrap_or_else(|| "no error detail".to_string());
                tracing::error!(
                    status = ?response.status,
                    finish_reason = %reason,
                    output_tokens = response.usage.output_tokens,
                    detail = %detail,
                    "Sub-agent model response ended with non-completed status"
                );
                return Err(AgentError::AgentProtocol(format!(
                    "model response not completed: status={:?}, finish_reason={}, detail={}",
                    response.status, reason, detail
                )));
            }

            // Extract text + reasoning + tool calls from ordered output blocks.
            let mut text = String::new();
            let mut reasoning = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            for block in &response.output {
                match block {
                    ContentBlock::Text { text: t } => text.push_str(t),
                    ContentBlock::Reasoning { text: r } => reasoning.push_str(r),
                    ContentBlock::ToolCall {
                        call_id,
                        name,
                        arguments,
                    } => tool_calls.push(ToolCall::new(
                        call_id.clone(),
                        name.clone(),
                        arguments.clone(),
                    )),
                    // 服务端工具由 provider 执行完毕，不并入 text/reasoning/tool_calls
                    ContentBlock::ServerToolUse { .. } | ContentBlock::ServerToolResult { .. } => {}
                    _ => {}
                }
            }

            // Store assistant text + reasoning + function calls in history (as InputItems).
            // Text must precede Reasoning/FunctionCall items so the chat adapter's reverse
            // merge reassembles them into one assistant message.
            if !text.is_empty() {
                self.messages.push(Arc::new(InputItem::Message {
                    role: Role::Assistant,
                    content: text.clone().into(),
                }));
            }
            if !reasoning.is_empty() {
                self.messages
                    .push(Arc::new(InputItem::Reasoning { content: reasoning }));
            }
            for tc in &tool_calls {
                self.messages.push(Arc::new(InputItem::FunctionCall {
                    call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    arguments: tc.function.arguments.clone(),
                }));
            }

            // No tool calls → done, return final text + collected artifacts
            if tool_calls.is_empty() {
                return Ok(SimpleLooperOutput {
                    text,
                    artifacts: std::mem::take(&mut self.artifacts),
                });
            }

            // ── Execute tools ─────────────────────────────────────────────
            let tool_messages = self.execute_tools(&tool_calls).await?;
            self.messages.extend(tool_messages);
        }
    }

    // ── Tool execution ─────────────────────────────────────────────────────

    /// Execute a batch of tool calls concurrently with cancel awareness.
    ///
    /// Spawns one tokio task per tool call. Awaits them in order (preserving
    /// the model's `tool_calls` sequence) while checking cancel between each.
    /// Returns `FunctionCallOutput` [`InputItem`]s to be appended to the history.
    async fn execute_tools(
        &mut self,
        tool_calls: &[ToolCall],
    ) -> Result<Vec<Arc<InputItem>>, AgentError> {
        // 有自定义执行器则用它，否则用 MCP 托管的执行器
        let executor = if let Some(ref ov) = self.tool_executor_override {
            ov.clone()
        } else {
            self.agent.mcp_manager().tools_executor().clone()
        };

        // Spawn all tools concurrently (with index for order preservation)
        let handles: Vec<ToolExecHandle> = tool_calls
            .iter()
            .enumerate()
            .map(|(idx, tc)| {
                let executor = executor.clone();
                let tc = tc.clone();
                tokio::spawn(async move {
                    let result = executor
                        .execute(&tc.function.name, &tc.function.arguments)
                        .await;
                    (idx, tc, result)
                })
            })
            .collect();

        // Await in order, checking cancel between each handle
        let mut results: Vec<(usize, Arc<InputItem>)> = Vec::with_capacity(handles.len());
        for (expected_idx, handle) in handles.into_iter().enumerate() {
            if self.cancel_flag.load(Ordering::Acquire) {
                // Remaining handles will be dropped (tokio tasks continue but
                // their results are discarded)
                return Err(AgentError::AgentProtocol("cancelled".into()));
            }
            match handle.await {
                Ok((idx, tc, output)) => {
                    // 声明剥离必须发生在这里：SimpleAgentLooper 与 AgentLooper 是
                    // 两条并行的「工具输出 → 模型历史」写入路径（子 agent、单轮
                    // 执行、结构化输出都走这条）。漏一次，模型就会看到
                    // `@@PECO_ARTIFACT` 行并照着格式编造不存在的产物。
                    //
                    // **错误路径同样要剥离**：MCP 工具的 error 文本是服务端原文
                    // （`mcp/tool.rs` 的 error_text 即脚本回传内容），同样可能带声明。
                    let content = match output {
                        Ok(r) => r,
                        Err(e) => Content::Text(e),
                    };
                    let (output, decls) = crate::artifacts::strip_content(content);
                    // 收集放在原地（紧跟 await），而不是循环结束后统一取：
                    // 取消/迭代耗尽会中途 return，批尾收集会把已执行工具的声明
                    // 一起丢掉。归属判定交给调用方，这里只负责不吞。
                    self.artifacts.extend(decls);
                    results.push((
                        idx,
                        Arc::new(InputItem::FunctionCallOutput {
                            call_id: tc.id,
                            output,
                        }),
                    ));
                }
                Err(join_err) => {
                    tracing::error!(error = %join_err, "Tool execution task panicked");
                    // Insert an error placeholder with estimated index
                    results.push((
                        expected_idx,
                        Arc::new(InputItem::FunctionCallOutput {
                            call_id: "unknown".into(),
                            output: format!("tool panicked: {join_err}").into(),
                        }),
                    ));
                }
            }
        }

        // Sort by original index to preserve model's tool_calls order
        results.sort_by_key(|(idx, _)| *idx);
        Ok(results.into_iter().map(|(_, msg)| msg).collect())
    }
}

// ============================================================================
// SimpleLooperHandle — public control handle
// ============================================================================

/// Control handle for a running [`SimpleAgentLooper`] background task.
///
/// Created by [`SimpleAgentLooper::spawn`].
///
/// # Lifecycle
///
/// ```text
/// let handle = SimpleAgentLooper::spawn(agent, "do X".into(), None);
///
/// // Option A: wait for result (正文 + 收集到的产物声明)
/// let out = handle.wait().await?;
/// println!("{}", out.text);
///
/// // Option B: cancel early and discard
/// handle.cancel();
/// drop(handle); // cancel flag was already set
///
/// // Option C: just drop — auto-cancels + aborts if last reference
/// drop(handle); // sets cancel flag and aborts the underlying task
/// ```
///
/// # Clone
///
/// `SimpleLooperHandle` is `Clone` — all fields are `Arc`-backed. Multiple
/// holders can share control. Only the last clone being dropped triggers
/// auto-cancel.
pub struct SimpleLooperHandle {
    cancel_flag: Arc<AtomicBool>,
    join_handle: SharedSimpleTask,
    /// 独立于 `join_handle` 的中止句柄（`AbortHandle` 可 clone），
    /// 允许在 `wait()` 已 consume `JoinHandle` 后仍能真正中止底层 task。
    abort_handle: tokio::task::AbortHandle,
}

impl SimpleLooperHandle {
    /// Request cancellation.
    ///
    /// The looper checks this flag at each iteration boundary (before model
    /// calls and between tool executions). In-flight model calls are not
    /// interrupted mid-request.
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Release);
    }

    /// Block until the looper completes, returning the final assistant text and
    /// the artifact declarations collected from tool outputs along the way.
    ///
    /// 消费方按自身能力取用：能落库的读 `out.artifacts`，其余用
    /// [`SimpleLooperOutput::into_text`]（会为被丢弃的声明留下告警）。
    ///
    /// # Errors
    ///
    /// Returns `AgentError::AgentProtocol("task already consumed")` if
    /// `wait()` has already been called on this handle.
    ///
    /// # Panics
    ///
    /// Propagates if the background task panicked.
    pub async fn wait(&self) -> Result<SimpleLooperOutput, AgentError> {
        let handle = self
            .join_handle
            .lock()
            .await
            .take()
            .ok_or_else(|| AgentError::AgentProtocol("task already consumed".into()))?;
        match handle.await {
            Ok(result) => result,
            Err(join_err) if join_err.is_cancelled() => {
                Err(AgentError::AgentProtocol("cancelled".into()))
            }
            Err(join_err) => Err(AgentError::AgentProtocol(format!(
                "looper task panicked: {join_err}"
            ))),
        }
    }

    /// Abort the underlying task immediately.
    ///
    /// Unlike [`cancel`](SimpleLooperHandle::cancel) (which is cooperative —
    /// the looper exits at its next loop boundary), this cancels the in-flight
    /// work (LLM request, tool execution) at the next await point. Safe to call
    /// after the task has already completed.
    pub fn abort(&self) {
        self.cancel_flag.store(true, Ordering::Release);
        self.abort_handle.abort();
    }

    /// Returns `true` if the background task is still executing.
    pub fn is_running(&self) -> bool {
        match self.join_handle.try_lock() {
            Ok(guard) => guard.as_ref().is_some_and(|h| !h.is_finished()),
            Err(_) => false,
        }
    }

    /// Returns `true` if the cancel flag has been set.
    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Acquire)
    }
}

impl Clone for SimpleLooperHandle {
    fn clone(&self) -> Self {
        Self {
            cancel_flag: Arc::clone(&self.cancel_flag),
            join_handle: Arc::clone(&self.join_handle),
            abort_handle: self.abort_handle.clone(),
        }
    }
}

impl Drop for SimpleLooperHandle {
    fn drop(&mut self) {
        // strong_count == 1 → this is the last reference
        if Arc::strong_count(&self.join_handle) == 1 {
            // 设置取消标志作为安全网：若 looper 仍在运行，会在下个循环迭代中正常退出。
            // looper 可能已通过 wait() 正常结束，此时 cancel_flag 无实际作用。
            self.cancel_flag.store(true, Ordering::Release);
            // 同时 abort 底层 task，使在途的 LLM 请求/工具执行被真正取消，
            // 而非在后台继续消耗 token / 产生副作用。AbortHandle 独立于
            // JoinHandle，即使 wait() 已 consume 后者仍能生效。
            self.abort_handle.abort();
            tracing::debug!(
                "SimpleLooperHandle dropped (last reference). \
                 Cancel flag set as safety net for any still-running looper."
            );
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::test_support::*;

    /// Spawn a no-op task purely to obtain an `AbortHandle` for tests that
    /// build a `SimpleLooperHandle` without a real running looper. The task is
    /// aborted immediately so it never executes.
    fn dummy_abort_handle() -> tokio::task::AbortHandle {
        let handle = tokio::spawn(async {});
        let abort = handle.abort_handle();
        handle.abort();
        abort
    }

    #[tokio::test]
    async fn test_handle_clone() {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let join_handle = Arc::new(tokio::sync::Mutex::new(
            None::<tokio::task::JoinHandle<Result<SimpleLooperOutput, AgentError>>>,
        ));
        let h1 = SimpleLooperHandle {
            cancel_flag: cancel_flag.clone(),
            join_handle: join_handle.clone(),
            abort_handle: dummy_abort_handle(),
        };
        let h2 = h1.clone();
        assert!(!h1.is_cancelled());
        assert!(!h2.is_cancelled());
        h2.cancel();
        assert!(h1.is_cancelled());
        assert!(h2.is_cancelled());
    }

    #[tokio::test]
    async fn test_handle_is_running_no_task() {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let join_handle = Arc::new(tokio::sync::Mutex::new(
            None::<tokio::task::JoinHandle<Result<SimpleLooperOutput, AgentError>>>,
        ));
        let h = SimpleLooperHandle {
            cancel_flag,
            join_handle,
            abort_handle: dummy_abort_handle(),
        };
        assert!(!h.is_running());
    }

    #[tokio::test]
    async fn test_handle_wait_already_consumed() {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let join_handle = Arc::new(tokio::sync::Mutex::new(
            None::<tokio::task::JoinHandle<Result<SimpleLooperOutput, AgentError>>>,
        ));
        let h = SimpleLooperHandle {
            cancel_flag,
            join_handle,
            abort_handle: dummy_abort_handle(),
        };
        let result = h.wait().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already consumed"));
    }

    #[tokio::test]
    async fn test_handle_wait_returns_result() {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let join_handle = tokio::spawn(async {
            Ok(SimpleLooperOutput {
                text: "hello".into(),
                artifacts: vec![],
            })
        });
        let abort_handle = join_handle.abort_handle();
        let handle = SimpleLooperHandle {
            cancel_flag,
            join_handle: Arc::new(tokio::sync::Mutex::new(Some(join_handle))),
            abort_handle,
        };
        let out = handle.wait().await.unwrap();
        assert_eq!(out.text, "hello");
    }

    #[tokio::test]
    async fn test_handle_wait_returns_error() {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let join_handle = tokio::spawn(async {
            Err::<SimpleLooperOutput, AgentError>(AgentError::MaxIterations { max_iterations: 1 })
        });
        let abort_handle = join_handle.abort_handle();
        let handle = SimpleLooperHandle {
            cancel_flag,
            join_handle: Arc::new(tokio::sync::Mutex::new(Some(join_handle))),
            abort_handle,
        };
        let result = handle.wait().await;
        assert!(result.is_err());
    }

    // ── 截断重试 / 异常收敛（batch-only run 循环）────────────────────────────

    /// 首次截断、重试成功：返回重试后的文本，且重试请求带抬高后的预算。
    #[tokio::test]
    async fn test_truncation_retries_with_raised_budget() {
        let (agent, provider) = scripted_agent(vec![truncated(), completed("done")]);
        let out = SimpleAgentLooper::spawn(agent, "hi".into(), None)
            .wait()
            .await
            .expect("retry should succeed");
        assert_eq!(out.text, "done", "必须返回重试后的正文，而非被截断的空串");
        assert_eq!(
            provider.budgets(),
            vec![None, Some(SUB_AGENT_TRUNCATION_RETRY_BUDGET)],
            "首次用默认预算，重试必须带上抬高的预算"
        );
    }

    /// 连续两次截断：只重试一次（预算已抬高，再发逐字节相同），最终报错而非返回空串。
    #[tokio::test]
    async fn test_truncation_exhausted_returns_error() {
        let (agent, provider) = scripted_agent(vec![truncated(), truncated()]);
        let err = SimpleAgentLooper::spawn(agent, "hi".into(), None)
            .wait()
            .await
            .expect_err("二次截断必须报错");
        assert!(
            err.to_string().contains("not completed"),
            "错误须指向响应未完成；实际 {err}"
        );
        assert_eq!(
            provider.budgets(),
            vec![None, Some(SUB_AGENT_TRUNCATION_RETRY_BUDGET)],
            "抬过一次后不得再重发"
        );
    }

    /// agent 自身已配置 `max_tokens` ≥ 抬升目标：抬不动 ⇒ 不重发（逐字节相同），
    /// 首次截断即报错，且只发生一次模型调用。镜像 `AgentLooper::can_retry` 第 4 条。
    #[tokio::test]
    async fn test_truncation_no_retry_when_budget_at_ceiling() {
        let (agent, provider) = scripted_agent_with_max_tokens(
            vec![truncated()],
            Some(SUB_AGENT_TRUNCATION_RETRY_BUDGET),
        );
        let err = SimpleAgentLooper::spawn(agent, "hi".into(), None)
            .wait()
            .await
            .expect_err("预算已到抬升目标，抬不动，必须直接报错");
        assert!(
            err.to_string().contains("not completed"),
            "错误须指向响应未完成；实际 {err}"
        );
        assert_eq!(
            provider.budgets(),
            vec![Some(SUB_AGENT_TRUNCATION_RETRY_BUDGET)],
            "预算已达抬升目标，不得重发（否则请求逐字节相同、白烧一次调用）"
        );
    }

    /// `Incomplete` 但 `finish_reason` 缺失：不臆测成截断、不重试，直接报错。
    #[tokio::test]
    async fn test_incomplete_without_finish_reason_returns_error_no_retry() {
        let mut r = truncated();
        r.finish_reason = None;
        let (agent, provider) = scripted_agent(vec![r]);
        let res = SimpleAgentLooper::spawn(agent, "hi".into(), None)
            .wait()
            .await;
        assert!(res.is_err(), "无 finish_reason 不得静默返回");
        assert_eq!(provider.budgets(), vec![None], "不得重试");
    }

    /// `Failed` 状态即便带了 partial 文本，也必须报错 —— 绝不把半截输出当成功。
    #[tokio::test]
    async fn test_failed_status_returns_error_not_partial_text() {
        let mut r = completed("partial");
        r.status = ResponseStatus::Failed;
        r.finish_reason = Some(FinishReason::Error);
        r.error = Some(model_provider::ResponseError {
            code: None,
            message: "upstream boom".into(),
        });
        let (agent, _provider) = scripted_agent(vec![r]);
        let res = SimpleAgentLooper::spawn(agent, "hi".into(), None)
            .wait()
            .await;
        let err = res.expect_err("Failed 必须报错");
        assert!(
            err.to_string().contains("upstream boom"),
            "错误须带上游 detail；实际 {err}"
        );
    }

    // ── 产物声明收集（成功 / 失败两条出口）──────────────────────────────────

    use std::io::Write;
    use tracing_subscriber::fmt::MakeWriter;

    fn decl(path: &str) -> ArtifactDecl {
        ArtifactDecl {
            path: path.to_string(),
            title: None,
            mime: None,
        }
    }

    /// 采集日志的 writer：把字节流追加到共享缓冲，供「丢弃告警恰好一次」的断言。
    ///
    /// 日志断言必须用 `#[tokio::test]` 默认的 `current_thread` 运行时：默认
    /// subscriber 是线程局部的，多线程运行会把后台 task 的日志写到别的线程上，
    /// 断言恒红。
    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for CapturedLogs {
        type Writer = CapturedLogs;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl CapturedLogs {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    fn capture_logs() -> (CapturedLogs, tracing::subscriber::DefaultGuard) {
        let writer = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .without_time()
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        (writer, guard)
    }

    const DROP_WARNING: &str = "Artifact declarations discarded with failed sub-agent run";

    /// 工具吐出的声明必须被收进 `out.artifacts`，且正文里不留 marker。
    ///
    /// 变异点：`execute_tools` 里不收集（或收集后不交给 `SimpleLooperOutput`）
    /// ⇒ `out.artifacts` 为空；剥离失效 ⇒ `out.text` 里出现 marker。
    #[tokio::test]
    async fn test_success_collects_artifact_declarations() {
        let d = decl("/tmp/success.jpg");
        let tool_output = format!(
            "{}生成完成",
            crate::artifacts::render_artifact_lines(std::slice::from_ref(&d))
        );
        let (agent, _provider) = scripted_agent_with_tools(
            vec![
                tool_call_response("call-1", "gen_artifact", serde_json::json!({})),
                completed("done"),
            ],
            vec![Box::new(StaticTool::new(
                "gen_artifact",
                Content::Text(tool_output),
            ))],
        );

        let out = SimpleAgentLooper::spawn(agent, "hi".into(), None)
            .wait()
            .await
            .expect("run should succeed");

        assert_eq!(out.text, "done");
        assert!(
            !out.text.contains(crate::artifacts::ARTIFACT_MARKER),
            "marker must never survive into the final text: {}",
            out.text
        );
        assert_eq!(out.artifacts, vec![d]);
    }

    /// 截断重试不丢已收集的声明：重试前后都是同一批。
    ///
    /// 变异点：把收集放在「最终出口」而非每轮 await 之后，截断重试的
    /// `continue` 会把第一轮的声明冲掉 ⇒ `artifacts` 为空。
    #[tokio::test]
    async fn test_truncation_retry_keeps_collected_artifacts() {
        let d = decl("/tmp/retry.jpg");
        let tool_output = format!(
            "{}生成完成",
            crate::artifacts::render_artifact_lines(std::slice::from_ref(&d))
        );
        let (agent, provider) = scripted_agent_with_tools(
            vec![
                tool_call_response("call-1", "gen_artifact", serde_json::json!({})),
                truncated(),
                completed("done"),
            ],
            vec![Box::new(StaticTool::new(
                "gen_artifact",
                Content::Text(tool_output),
            ))],
        );

        let out = SimpleAgentLooper::spawn(agent, "hi".into(), None)
            .wait()
            .await
            .expect("retry should succeed");

        assert_eq!(out.text, "done");
        assert_eq!(out.artifacts, vec![d]);
        assert_eq!(
            provider.budgets(),
            vec![None, None, Some(SUB_AGENT_TRUNCATION_RETRY_BUDGET)],
            "重试仍须抬高预算"
        );
    }

    /// 迭代耗尽失败：已收集的声明必须告警一次（且只一次）。
    ///
    /// 变异点：告警留在 `execute_tools`（此时尚未知成败）会多报/错报；
    /// 收敛点漏掉会在失败时静默吞掉声明。
    #[tokio::test]
    async fn test_max_iterations_warns_once_about_discarded_artifacts() {
        let d = decl("/tmp/max_iter.jpg");
        let tool_output = format!(
            "{}生成完成",
            crate::artifacts::render_artifact_lines(std::slice::from_ref(&d))
        );
        let (agent, _provider) = scripted_agent_with_tools(
            vec![tool_call_response(
                "call-1",
                "gen_artifact",
                serde_json::json!({}),
            )],
            vec![Box::new(StaticTool::new(
                "gen_artifact",
                Content::Text(tool_output),
            ))],
        );

        let (logs, _guard) = capture_logs();
        let err = SimpleAgentLooper::spawn(agent, "hi".into(), Some(1))
            .wait()
            .await
            .expect_err("max_iterations=1 must fail after the tool round");
        assert!(
            matches!(err, AgentError::MaxIterations { .. }),
            "expected MaxIterations, got {err}"
        );

        let logs = logs.text();
        assert_eq!(
            logs.matches(DROP_WARNING).count(),
            1,
            "discard warning must appear exactly once; logs: {logs}"
        );
        assert!(
            logs.contains("count=1"),
            "warning must carry the artifact count; logs: {logs}"
        );
    }

    /// 取消发生在工具结果尚未收集之前：已执行完的工具声明仍要收集并告警一次。
    ///
    /// 变异点：把收集挪到批尾（循环结束后统一做）⇒ 中途 return 时声明全丢，
    /// 告警不出现 ⇒ 断言红。
    #[tokio::test]
    async fn test_cancel_collects_artifacts_of_finished_tool() {
        let d = decl("/tmp/cancel.jpg");
        let tool = GateTool::new(
            "gate",
            Content::Text(format!(
                "{}生成完成",
                crate::artifacts::render_artifact_lines(std::slice::from_ref(&d))
            )),
        );
        let entered = Arc::clone(&tool.entered);
        let release = Arc::clone(&tool.release);
        let (agent, _provider) = scripted_agent_with_tools(
            vec![tool_call_response("call-1", "gate", serde_json::json!({}))],
            vec![Box::new(tool)],
        );

        let (logs, _guard) = capture_logs();
        let handle = SimpleAgentLooper::spawn(agent, "hi".into(), None);
        // 顺序不可换：先等工具真正开跑，再取消（此时尚未 await 到结果），
        // 最后放行工具 —— 这样声明一定在取消之后才被收集。
        entered.notified().await;
        handle.cancel();
        release.notify_one();

        let err = handle.wait().await.expect_err("cancelled run must fail");
        assert!(matches!(err, AgentError::AgentProtocol(ref m) if m == "cancelled"));
        assert!(
            !err.to_string().contains(crate::artifacts::ARTIFACT_MARKER),
            "error message must not carry artifact declarations: {err}"
        );

        let logs = logs.text();
        assert_eq!(
            logs.matches(DROP_WARNING).count(),
            1,
            "discard warning must appear exactly once; logs: {logs}"
        );
        assert!(
            logs.contains("count=1"),
            "warning must report the single collected declaration; logs: {logs}"
        );
    }

    /// 取消时只收集「已执行完」的工具：后面的工具（B）从未 await，其声明不得计入。
    ///
    /// 变异点：收集改成批尾统一做（或把未执行工具的声明也塞进来）⇒ 计数变 0 或 2，
    /// 告警丢失或翻倍 ⇒ 断言红。
    #[tokio::test]
    async fn test_cancel_collects_only_finished_tools() {
        let d = decl("/tmp/cancel-a.jpg");
        let gate = GateTool::new(
            "gate_a",
            Content::Text(format!(
                "{}生成完成",
                crate::artifacts::render_artifact_lines(std::slice::from_ref(&d))
            )),
        );
        let entered = Arc::clone(&gate.entered);
        let release = Arc::clone(&gate.release);

        let mut response = tool_call_response("call-a", "gate_a", serde_json::json!({}));
        response.output.push(ContentBlock::ToolCall {
            call_id: "call-b".into(),
            name: "late_b".into(),
            arguments: "{}".into(),
        });

        let late_decl = decl("/tmp/cancel-b.jpg");
        let (agent, _provider) = scripted_agent_with_tools(
            vec![response],
            vec![
                Box::new(gate),
                Box::new(StaticTool::new(
                    "late_b",
                    Content::Text(format!(
                        "{}生成完成",
                        crate::artifacts::render_artifact_lines(&[late_decl])
                    )),
                )),
            ],
        );

        let (logs, _guard) = capture_logs();
        let handle = SimpleAgentLooper::spawn(agent, "hi".into(), None);
        entered.notified().await;
        handle.cancel();
        release.notify_one();

        let err = handle.wait().await.expect_err("cancelled run must fail");
        assert!(matches!(err, AgentError::AgentProtocol(ref m) if m == "cancelled"));

        let logs = logs.text();
        assert_eq!(
            logs.matches(DROP_WARNING).count(),
            1,
            "discard warning must appear exactly once; logs: {logs}"
        );
        assert!(
            logs.contains("count=1"),
            "only tool A's declaration may be collected; logs: {logs}"
        );
    }
}
