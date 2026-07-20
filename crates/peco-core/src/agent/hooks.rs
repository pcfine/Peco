// ============================================================================
// LooperHook — AgentLooper 生命周期拦截接口
// ============================================================================
//
// Hook 在 looper 的 async context 中按注册顺序依次调用。
// 第一个返回非 Continue 的 hook 会短路后续 hook 的执行。

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use model_provider::{ChatResponse, Message, ToolCall, Usage};

use super::agent_looper::{OuterState, ReActState, TurnFailureReason};
use crate::session::Session;

// ============================================================================
// Hook 返回值类型
// ============================================================================

/// 通用 hook 返回值：Continue 继续，Abort 中止当前 turn。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookAction {
    /// 正常继续执行。
    Continue,
    /// 中止当前 turn，looper 进入 Failed 状态，
    /// 包含的字符串变为 `TurnFailureReason::HookAbort`。
    Abort(String),
}

/// `on_before_tool` 专用返回值。
///
/// 提供比 `HookAction` 更丰富的控制：可以跳过执行并返回替代结果，
/// 或拒绝执行并将原因反馈给模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolHookAction {
    /// 正常执行 tool。
    Continue,
    /// 跳过 tool 执行，使用包含的值作为 tool 结果。
    Override(String),
    /// 拒绝执行，包含的原因将作为 tool error 结果返回给模型。
    Reject(String),
    /// 中止整个 turn。
    Abort(String),
}

// ============================================================================
// LooperHook trait
// ============================================================================

/// AgentLooper 生命周期 hook。
///
/// 所有方法都有默认空实现，只需覆写关心的节点。
/// 每个 hook 在 looper 的 async context 中调用（可 `.await`）。
///
/// # Hook 执行顺序
///
/// 多个 hook 按 `LooperConfig::hooks` 中的注册顺序依次调用。
/// 第一个返回非 Continue 的 hook 短路后续 hook。
///
/// # Panic 安全
///
/// Hook 实现者应保证不 panic。若 panic 发生，会导致 looper task 终止。
#[async_trait]
pub trait LooperHook: Send + Sync {
    // ── 请求阶段 ──────────────────────────────────────────────────────────

    /// 模型请求发送前调用。
    ///
    /// - `messages`：即将发送给模型的完整消息列表（已包含动态 system prompt）。
    ///   可在此处修改、注入、截断消息。
    /// - 返回 `HookAction::Abort(reason)` 中止本次请求。
    async fn on_before_request(
        &self,
        _turn_index: usize,
        _messages: &mut Vec<Arc<Message>>,
    ) -> HookAction {
        HookAction::Continue
    }

    /// 收到模型完整响应后调用（在写入 session staging 之前）。
    async fn on_after_response(&self, _turn_index: usize, _response: &ChatResponse) -> HookAction {
        HookAction::Continue
    }

    /// 每个 streaming text delta 时调用。
    ///
    /// 返回 `Abort` 可提前终止流式响应。
    async fn on_text_delta(
        &self,
        _turn_index: usize,
        _delta: &str,
        _accumulated: &str,
    ) -> HookAction {
        HookAction::Continue
    }

    // ── Tool 阶段 ─────────────────────────────────────────────────────────

    /// Tool 执行前调用。
    ///
    /// - `Override(result)`：跳过执行，用给定值作为 tool 结果。
    /// - `Reject(reason)`：拒绝执行，将原因作为 tool error 结果返回给模型。
    /// - `Abort(reason)`：中止整个 turn。
    async fn on_before_tool(&self, _turn_index: usize, _tool_call: &ToolCall) -> ToolHookAction {
        ToolHookAction::Continue
    }

    /// Tool 执行完成后、结果写入 session 前调用。
    ///
    /// 纯观察方法，返回值被忽略。
    async fn on_after_tool(
        &self,
        _turn_index: usize,
        _tool_call: &ToolCall,
        _result: &str,
        _is_error: bool,
    ) {
    }

    // ── Turn 阶段 ─────────────────────────────────────────────────────────

    /// Turn 完成时调用（Done 或 Failed 状态转移的收尾阶段）。
    ///
    /// `failure: None` 表示正常完成（`ReActState::Done`）；
    /// `failure: Some(...)` 携带具体的 [`TurnFailureReason`]。
    /// `session` 提供对当前对话消息的只读访问。
    async fn on_turn_complete(
        &self,
        _turn_index: usize,
        _failure: Option<&TurnFailureReason>,
        _usage: &Usage,
        _session: &Session,
    ) {
    }

    // ── 状态机 ────────────────────────────────────────────────────────────

    /// ReAct 内层状态转换时调用。
    async fn on_react_state_change(&self, _turn_index: usize, _from: ReActState, _to: ReActState) {}

    /// 外层状态转换时调用。
    async fn on_outer_state_change(&self, _from: OuterState, _to: OuterState) {}
}

// ============================================================================
// 内置 Hook 实现
// ============================================================================

/// 工具白名单 Hook：只允许执行指定名称的 tool。
///
/// # 示例
///
/// ```ignore
/// use std::collections::HashSet;
/// let allowed: HashSet<String> = ["read_file", "search"].into_iter().map(String::from).collect();
/// let hook = ToolAllowlistHook::new(allowed);
/// ```
#[derive(Debug, Clone)]
pub struct ToolAllowlistHook {
    allowed: HashSet<String>,
}

impl ToolAllowlistHook {
    /// 创建新的工具白名单 hook。
    pub fn new(allowed: HashSet<String>) -> Self {
        Self { allowed }
    }
}

#[async_trait]
impl LooperHook for ToolAllowlistHook {
    async fn on_before_tool(&self, _turn_index: usize, tool_call: &ToolCall) -> ToolHookAction {
        if self.allowed.contains(&tool_call.function.name) {
            ToolHookAction::Continue
        } else {
            ToolHookAction::Reject(format!(
                "Tool '{}' is not in the allowlist",
                tool_call.function.name
            ))
        }
    }
}

/// Token 预算 Hook：在累计 token 用量超出预算时中止。
///
/// 在每轮完成后累加用量，下一轮请求前检查是否超预算。
///
/// # 示例
///
/// ```ignore
/// let hook = TokenBudgetHook::new(100_000); // 10 万 token 预算
/// ```
pub struct TokenBudgetHook {
    max_total_tokens: u64,
    accumulated: tokio::sync::Mutex<u64>,
}

impl TokenBudgetHook {
    /// 创建新的 token 预算 hook。
    pub fn new(max_total_tokens: u64) -> Self {
        Self {
            max_total_tokens,
            accumulated: tokio::sync::Mutex::new(0),
        }
    }

    /// 返回当前累计用量。
    pub async fn accumulated(&self) -> u64 {
        *self.accumulated.lock().await
    }
}

#[async_trait]
impl LooperHook for TokenBudgetHook {
    async fn on_turn_complete(
        &self,
        _turn_index: usize,
        failure: Option<&TurnFailureReason>,
        usage: &Usage,
        _session: &Session,
    ) {
        if failure.is_none() {
            let mut acc = self.accumulated.lock().await;
            *acc += usage.total_tokens as u64;
        }
    }

    async fn on_before_request(
        &self,
        _turn_index: usize,
        _messages: &mut Vec<Arc<Message>>,
    ) -> HookAction {
        let acc = *self.accumulated.lock().await;
        if acc >= self.max_total_tokens {
            HookAction::Abort(format!(
                "Token budget exceeded: {} >= {}",
                acc, self.max_total_tokens
            ))
        } else {
            HookAction::Continue
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── HookAction tests ──────────────────────────────────────────────────

    #[test]
    fn test_hook_action_continue() {
        assert_eq!(HookAction::Continue, HookAction::Continue);
    }

    #[test]
    fn test_hook_action_abort() {
        let action = HookAction::Abort("test".into());
        assert!(matches!(action, HookAction::Abort(_)));
    }

    // ── ToolHookAction tests ──────────────────────────────────────────────

    #[test]
    fn test_tool_hook_action_variants() {
        assert!(matches!(ToolHookAction::Continue, ToolHookAction::Continue));
        assert!(matches!(
            ToolHookAction::Override("result".into()),
            ToolHookAction::Override(_)
        ));
        assert!(matches!(
            ToolHookAction::Reject("reason".into()),
            ToolHookAction::Reject(_)
        ));
        assert!(matches!(
            ToolHookAction::Abort("reason".into()),
            ToolHookAction::Abort(_)
        ));
    }

    // ── ToolAllowlistHook tests ──────────────────────────────────────────

    #[tokio::test]
    async fn test_allowlist_allows_registered_tool() {
        let allowed: HashSet<String> = ["allowed_tool".into()].into_iter().collect();
        let hook = ToolAllowlistHook::new(allowed);

        let tc = ToolCall::new("id1", "allowed_tool", "{}");
        let result = hook.on_before_tool(0, &tc).await;
        assert!(matches!(result, ToolHookAction::Continue));
    }

    #[tokio::test]
    async fn test_allowlist_blocks_unregistered_tool() {
        let allowed: HashSet<String> = ["allowed_tool".into()].into_iter().collect();
        let hook = ToolAllowlistHook::new(allowed);

        let tc = ToolCall::new("id1", "blocked_tool", "{}");
        let result = hook.on_before_tool(0, &tc).await;
        assert!(matches!(result, ToolHookAction::Reject(_)));
    }

    // ── TokenBudgetHook tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_token_budget_allows_when_under() {
        let hook = TokenBudgetHook::new(1000);
        let result = hook.on_before_request(0, &mut Vec::new()).await;
        assert!(matches!(result, HookAction::Continue));
    }

    #[tokio::test]
    async fn test_token_budget_aborts_when_exceeded() {
        let hook = TokenBudgetHook::new(0); // budget of 0, always exceeded
        let result = hook.on_before_request(0, &mut Vec::new()).await;
        assert!(matches!(result, HookAction::Abort(_)));
    }

    #[tokio::test]
    async fn test_token_budget_accumulates() {
        let hook = TokenBudgetHook::new(1000);
        let usage = Usage {
            total_tokens: 500,
            input_tokens: 200,
            output_tokens: 300,
        };
        hook.on_turn_complete(0, None, &usage, &Session::new("test".into(), "test".into()))
            .await;
        assert_eq!(hook.accumulated().await, 500);
    }
}
