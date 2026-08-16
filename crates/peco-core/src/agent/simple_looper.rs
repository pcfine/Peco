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

use model_provider::{Message, ToolCall};

use super::agent::Agent;
use super::error::AgentError;
use crate::tools::ToolExecutor;

/// Concurrent tool execution result: (index, tool_call, output).
type ToolExecResult = (usize, ToolCall, Result<String, String>);
type ToolExecHandle = tokio::task::JoinHandle<ToolExecResult>;
type SimpleTaskHandle = tokio::task::JoinHandle<Result<String, AgentError>>;
type SharedSimpleTask = Arc<tokio::sync::Mutex<Option<SimpleTaskHandle>>>;

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
    max_turns: usize,

    /// Accumulated message history.
    ///
    /// System prompt is NOT stored here — [`Agent::chat`] injects it on each
    /// call. This vec contains User, Assistant, and Tool messages only.
    messages: Vec<Arc<Message>>,

    /// Model calls made so far in this run. Checked against `max_turns`.
    react_loop_iteration: usize,

    /// External cancel signal shared with [`SimpleLooperHandle`].
    cancel_flag: Arc<AtomicBool>,

    /// 可选的自定义 ToolExecutor，覆盖 agent 内置的执行器。
    /// 设置后，工具定义获取和执行均使用此执行器。
    /// 由 [`StructuredOutputExecutor`](crate::executor::StructuredOutputExecutor) 使用。
    tool_executor_override: Option<Arc<dyn ToolExecutor>>,
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
    /// * `max_turns` — Override for `agent.max_turns()`. Pass `None` to use
    ///   the agent's configured value.
    pub fn spawn(
        agent: Arc<Agent>,
        prompt: String,
        max_turns: Option<usize>,
    ) -> SimpleLooperHandle {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let max_turns = max_turns.unwrap_or_else(|| agent.max_turns());

        let mut looper = SimpleAgentLooper {
            agent,
            max_turns,
            messages: Vec::new(),
            react_loop_iteration: 0,
            cancel_flag: cancel_flag.clone(),
            tool_executor_override: None,
        };

        let join_handle = tokio::spawn(async move { looper.run(prompt).await });
        let abort_handle = join_handle.abort_handle();

        SimpleLooperHandle {
            cancel_flag,
            join_handle: Arc::new(tokio::sync::Mutex::new(Some(join_handle))),
            abort_handle,
        }
    }

    /// 使用自定义 [`ToolExecutor`] 启动（覆盖 agent 内置的）。
    ///
    /// 供 [`StructuredOutputExecutor`](crate::executor::StructuredOutputExecutor)
    /// 注入 `__submit_output__` 工具使用，其他行为与 [`spawn`](SimpleAgentLooper::spawn) 一致。
    pub fn spawn_with_executor(
        agent: Arc<Agent>,
        prompt: String,
        tool_executor: Arc<dyn ToolExecutor>,
        max_turns: Option<usize>,
    ) -> SimpleLooperHandle {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let max_turns = max_turns.unwrap_or_else(|| agent.max_turns());

        let mut looper = SimpleAgentLooper {
            agent,
            max_turns,
            messages: Vec::new(),
            react_loop_iteration: 0,
            cancel_flag: cancel_flag.clone(),
            tool_executor_override: Some(tool_executor),
        };

        let join_handle = tokio::spawn(async move { looper.run(prompt).await });
        let abort_handle = join_handle.abort_handle();

        SimpleLooperHandle {
            cancel_flag,
            join_handle: Arc::new(tokio::sync::Mutex::new(Some(join_handle))),
            abort_handle,
        }
    }

    // ── Core loop ──────────────────────────────────────────────────────────

    /// Execute the ReAct loop and return the final assistant text.
    async fn run(&mut self, prompt: String) -> Result<String, AgentError> {
        // Ensure deferred MCP connections are established before first tool use.
        self.agent.mcp_manager().ensure_connected().await;

        // Build initial message list: [User(prompt)]
        self.messages.push(Arc::new(Message::user(&prompt)));

        loop {
            // ── Check cancel ──────────────────────────────────────────────
            if self.cancel_flag.load(Ordering::Acquire) {
                return Err(AgentError::AgentProtocol("cancelled".into()));
            }

            // ── Check max_turns ───────────────────────────────────────────
            if self.react_loop_iteration >= self.max_turns {
                return Err(AgentError::MaxTurns {
                    max_turns: self.max_turns,
                });
            }
            self.react_loop_iteration += 1;

            // ── Model call (batch, non-streaming) ─────────────────────────
            // Build messages with system prompt prepended (system prompt is
            // not stored in history — injected dynamically each call).
            let mut chat_messages = Vec::with_capacity(self.messages.len() + 1);
            chat_messages.push(Arc::new(Message::system(self.agent.system_prompt())));
            chat_messages.extend(self.messages.iter().cloned());

            // 如果有自定义执行器则使用它获取工具定义，否则用 agent 默认的
            let response = if let Some(ref executor) = self.tool_executor_override {
                let tools = executor.definitions();
                self.agent.chat_with_tools(chat_messages, tools).await?
            } else {
                self.agent.chat(chat_messages).await?
            };

            // Extract content from response
            let (text, tool_calls) = match &response.message {
                Message::Assistant {
                    content,
                    tool_calls,
                    ..
                } => (content.clone().unwrap_or_default(), tool_calls.clone()),
                _ => (String::new(), None),
            };

            // Store assistant message in history
            self.messages.push(Arc::new(response.message));

            // No tool calls → done, return final text
            let tool_calls = match tool_calls {
                Some(tcs) if !tcs.is_empty() => tcs,
                _ => return Ok(text),
            };

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
    /// Returns `Vec<Message>` to be appended to the message history.
    async fn execute_tools(
        &self,
        tool_calls: &[ToolCall],
    ) -> Result<Vec<Arc<Message>>, AgentError> {
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
        let mut results: Vec<(usize, Arc<Message>)> = Vec::with_capacity(handles.len());
        for (expected_idx, handle) in handles.into_iter().enumerate() {
            if self.cancel_flag.load(Ordering::Acquire) {
                // Remaining handles will be dropped (tokio tasks continue but
                // their results are discarded)
                return Err(AgentError::AgentProtocol("cancelled".into()));
            }
            match handle.await {
                Ok((idx, tc, output)) => {
                    let content = match output {
                        Ok(r) => r,
                        Err(e) => e,
                    };
                    results.push((idx, Arc::new(Message::tool(&tc.id, &content))));
                }
                Err(join_err) => {
                    tracing::error!(error = %join_err, "Tool execution task panicked");
                    // Insert an error placeholder with estimated index
                    results.push((
                        expected_idx,
                        Arc::new(Message::tool(
                            "unknown",
                            format!("tool panicked: {join_err}"),
                        )),
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
/// // Option A: wait for result
/// let output = handle.wait().await?;
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

    /// Block until the looper completes, returning the final assistant text.
    ///
    /// # Errors
    ///
    /// Returns `AgentError::AgentProtocol("task already consumed")` if
    /// `wait()` has already been called on this handle.
    ///
    /// # Panics
    ///
    /// Propagates if the background task panicked.
    pub async fn wait(&self) -> Result<String, AgentError> {
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
            None::<tokio::task::JoinHandle<Result<String, AgentError>>>,
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
            None::<tokio::task::JoinHandle<Result<String, AgentError>>>,
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
            None::<tokio::task::JoinHandle<Result<String, AgentError>>>,
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
        let join_handle = tokio::spawn(async { Ok("hello".to_string()) });
        let abort_handle = join_handle.abort_handle();
        let handle = SimpleLooperHandle {
            cancel_flag,
            join_handle: Arc::new(tokio::sync::Mutex::new(Some(join_handle))),
            abort_handle,
        };
        let result = handle.wait().await.unwrap();
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn test_handle_wait_returns_error() {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let join_handle = tokio::spawn(async {
            Err::<String, AgentError>(AgentError::MaxTurns { max_turns: 1 })
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
}
