// ============================================================================
// AgentLooper — 双层状态机驱动的 Agent 执行循环
// ============================================================================
//
// 架构：外层（用户交互）+ 内层（ReAct Loop: 模型推理 → tool 执行 → 循环）
//
//   外层: Idle ──→ ProcessingUserInput ──→ RunningInnerLoop
//                                           │
//   内层: PreparingRequest ──→ [batch] AwaitingModel → ResolvingResponse
//                         ──→ [stream] Streaming
//                         ──→ ExecutingTools ──→ (循环回 PreparingRequest)
//                         ──→ Done / Failed
//
// Session 只存对话历史（User / Assistant / Tool），System prompt 动态注入。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use model_provider::{
    BlockAssembler, Content, ContentBlock, FinishReason, GenerateResult, GenerateStream, InputItem,
    ResponseStatus, Role, StreamChunk, ToolCall, Usage,
};

type ModelTaskHandle = tokio::task::JoinHandle<Result<ModelResponse, AgentError>>;
type SharedModelTask = Arc<tokio::sync::Mutex<Option<ModelTaskHandle>>>;
use serde::{Deserialize, Serialize};

use super::agent::{Agent, MessageFilter, ModelResponse};
use super::compaction::CompactionOutcome;
use super::context::{estimate_item_tokens, estimate_str_tokens};
use super::dynamic_context::DynamicContext;
use super::error::AgentError;
use super::hooks::{HookAction, LooperHook, ToolHookAction};
use crate::persistence::{INFLIGHT_CRASH_REASON, InflightCheckpoint};
use crate::session::{AnnotatedMessage, MessageSource, Session, SessionSnapshot, SessionState};
use crate::utils::intercom::{Listener, Speaker, make_async_intercom_pair};
use tracing::{debug, error, info, warn};

// ============================================================================
// 纯标记状态枚举（不携带数据）
// ============================================================================

/// 外层状态：用户交互层面
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OuterState {
    /// 初始/空闲，等待用户输入
    Idle,
    /// 正在处理用户输入
    ProcessingUserInput,
    /// 内层 ReAct 循环运行中
    RunningInnerLoop,
    /// 已暂停（通过 [`LooperHandle::pause`] 触发），等待 [`LooperHandle::resume`]
    Paused,
}

/// 内层状态：ReAct 推理-执行循环
///
/// batch 和 streaming 分别有独立的状态路径，但共享 ExecutingTools / Done / Failed。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReActState {
    /// 构建 GenerateRequest，决定走 batch 还是 streaming 分支
    PreparingRequest,

    // ── batch 分支 ──
    /// 已发送非流式请求，等待完整 GenerateResult
    AwaitingModel,
    /// 收到完整 GenerateResult，分析结果（有无 tool_calls）
    ResolvingResponse,

    // ── streaming 分支 ──
    /// 消费 GenerateStream，逐 chunk 处理直到流关闭
    Streaming,

    // ── 共享后续状态 ──
    /// 执行收集到的 tool calls
    ExecutingTools,
    /// 本轮完成（无 tool_calls 或模型不再调用 tool）
    Done,
    /// 流程异常终止
    Failed,
}

// ============================================================================
// 运行时数据容器
// ============================================================================

/// 内层循环需要的临时数据
#[derive(Debug, Clone, Default)]
pub(crate) struct ReActContext {
    /// batch 模式的完整响应
    batch_response: Option<GenerateResult>,
    /// 待执行的 tool calls，每个携带自身执行状态
    pending_tool_calls: Vec<PendingToolCall>,
    /// 当前轮 assistant 文本内容
    assistant_text: String,
    /// 当前轮 assistant 推理内容
    assistant_reasoning: String,
}

/// 单个待执行的 tool call 及其执行结果。
///
/// 将 call 和 result 绑定在一起，避免分离的 Vec 顺序依赖；
/// 同时支持断点续执行 — 恢复时只需执行 result 为 None 的项。
///
/// `call` 使用 `Arc<ToolCall>` 共享所有权，避免在 hook 调用、事件发送、
/// task spawn 等多处频繁 clone `ToolCall` 内部的 `String` 字段。
#[derive(Debug, Clone)]
pub(crate) struct PendingToolCall {
    call: Arc<ToolCall>,
    /// None = 尚未执行；Some = 已执行完成（含成功或失败）
    result: Option<ToolCallResult>,
}

/// Tool 执行结果
///
/// `result` 为中立的 `Content`：纯文本工具结果为 `Text`，回图工具（MCP 截图等）
/// 为 `Parts`，写入 session 时零转换；事件与 hook 侧需要文本时取 text 视图。
#[derive(Debug, Clone)]
pub(crate) struct ToolCallResult {
    call: Arc<ToolCall>,
    result: Content,
    is_error: bool,
}

// ============================================================================
// LooperConfig — 配置聚合
// ============================================================================

/// AgentLooper 的配置。
///
/// 聚合所有 looper 级别的可配置参数，包括超时、事件 buffer 和 hook 链。
#[derive(Clone)]
pub struct LooperConfig {
    /// 事件通道 buffer 大小。
    pub event_buffer: usize,
    /// 每轮超时（从 PreparingRequest 到 Done/Failed）。
    pub per_turn_timeout: Option<Duration>,
    /// 总超时（从第一个 Query 到 looper 退出）。
    pub total_timeout: Option<Duration>,
    /// Hook 链（按注册顺序调用）。
    pub hooks: Vec<Arc<dyn LooperHook>>,
    /// 环境上下文：会话级恒定的运行环境描述（用户身份、工作空间路径、日期等）。
    ///
    /// 契约（引擎与宿主层共同维护）：
    /// - 宿主层（peco-server / peco-cli）在构造 looper 时求值一次并传入；
    ///   引擎将其与 system prompt 拼接为稳定前缀，构造时缓存，此后不再读取。
    /// - 内容必须会话级恒定。此字段位于稳定前缀内，若随轮次变化，
    ///   将静默击穿 provider 的前缀缓存。该约束无法由类型系统表达，
    ///   以此文档契约为准。
    /// - 永不持久化：环境块只存在于发往 LLM 的请求中，不写入 Session 历史。
    pub environment: Option<String>,
    /// 动态上下文提供者。
    pub dynamic_context: Option<Arc<dyn DynamicContext>>,
    /// 上下文构建策略。
    pub context_strategy: super::context::ContextStrategy,
    /// 失败时是否把冻结的中断轮落盘。
    ///
    /// **冻结无条件发生（内存），落盘由此开关门控。** 这样 CLI 与 server 的
    /// 事件流行为一致 —— `TurnComplete` 负载不因配置而异。
    /// CLI 走 `NullSessionPersister`，置 `false` 即无副作用；server 置 `true`。
    pub persist_on_failure: bool,
    /// 可选的消息过滤器：在上下文构建完成、system prompt 注入后，
    /// 对最终发送给 LLM 的消息列表进行转换。
    ///
    /// 与 [`ContextFilter`](super::context::ContextFilter) 的区别：
    /// - `ContextFilter` 从 Session 历史中选择*哪些*消息进入上下文
    /// - `MessageFilter` 对已选定的消息列表做*最后一公里*转换
    ///
    /// 默认为 `None`（不过滤）。每个 `AgentLooper` 实例可独立配置。
    pub message_filter: Option<Arc<dyn MessageFilter>>,
    /// 上下文滚动压缩策略（Peco 永续会话等无界历史场景）。
    ///
    /// 在每个 turn 成功提交并持久化后检查：估算上下文超过
    /// [`CompactionPolicy::trigger_tokens`](super::compaction::CompactionPolicy) 时，
    /// 物理驱逐最旧轮次并以结构化摘要钉扎。压缩是非致命的 — 失败仅记录日志。
    /// 默认为 `None`（不压缩）。
    pub compaction: Option<Arc<super::compaction::CompactionPolicy>>,
}

impl Default for LooperConfig {
    fn default() -> Self {
        Self {
            event_buffer: 256,
            per_turn_timeout: Some(Duration::from_secs(180)),
            total_timeout: None,
            hooks: Vec::new(),
            environment: None,
            dynamic_context: None,
            context_strategy: super::context::ContextStrategy::FullHistory,
            persist_on_failure: false,
            message_filter: None,
            compaction: None,
        }
    }
}

impl std::fmt::Debug for LooperConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LooperConfig")
            .field("event_buffer", &self.event_buffer)
            .field("per_turn_timeout", &self.per_turn_timeout)
            .field("total_timeout", &self.total_timeout)
            .field("hooks", &format_args!("{} hooks", self.hooks.len()))
            .finish()
    }
}

// ============================================================================
// Prompt 组装纯函数 — 提取为模块级函数以便在无 Agent 的情况下单测
// ============================================================================

/// 拼接稳定前缀：system prompt + 环境上下文。
///
/// 环境块为空串时视为 `None`——否则会追加尾随 `"\n\n"`，
/// 破坏 `environment: None` 路径与旧行为的字节一致。
fn compose_stable_prefix(system_prompt: &str, environment: Option<&str>) -> String {
    match environment.filter(|e| !e.is_empty()) {
        Some(env) => format!("{system_prompt}\n\n{env}"),
        None => system_prompt.to_string(),
    }
}

/// 拼接最终 instructions：稳定前缀 + 动态上下文。
///
/// 动态上下文拼在稳定前缀之后属于**过渡形态**：
/// 它位于消息序列首条，一旦每轮变化会使其后的全部历史
/// 失去前缀缓存命中。终态方案（动态块前置到本轮 user 消息，
/// 不写入 Session）。
fn compose_effective_prompt(stable_prefix: &str, dynamic_context: Option<&str>) -> String {
    match dynamic_context {
        Some(dyn_ctx) => format!("{stable_prefix}\n\n[Dynamic Context]\n{dyn_ctx}"),
        None => stable_prefix.to_string(),
    }
}

/// 汇总已收敛块的类型（保持首现顺序、去重），供失败日志单行呈现。
///
/// 只打类型不打内容：块的正文可能包含用户数据，日志层不应落原文。
fn block_kinds_of(blocks: &[ContentBlock]) -> Vec<&'static str> {
    let mut kinds = Vec::new();
    for block in blocks {
        let kind = match block {
            ContentBlock::Text { .. } => "text",
            ContentBlock::Reasoning { .. } => "reasoning",
            ContentBlock::ToolCall { .. } => "tool_call",
            _ => "other",
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

// ============================================================================
// LooperEvent — 内部事件
// ============================================================================

// ============================================================================
// TurnFailureReason — 类型安全的 turn 失败原因
// ============================================================================

/// Turn 失败原因。
///
/// 替代原来散落在代码各处的魔法字符串（`"cancelled"`、`"max_turns_exceeded"` 等），
/// 通过 [`TurnComplete`](LooperEvent::TurnComplete) 的 `failure` 字段传递。
///
/// 当 `failure: None` 时表示正常完成（`ReActState::Done`）；
/// `failure: Some(...)` 时表示异常终止（`ReActState::Failed`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum TurnFailureReason {
    /// 外部取消
    Cancelled,
    /// 总运行超时
    TotalTimeout,
    /// 单轮超时
    PerTurnTimeout,
    /// 超出最大轮数
    MaxTurnsExceeded,
    /// Hook 中止（含原因描述）
    HookAbort(String),
    /// 其他未知失败
    Other(String),
}

/// 一轮完成的结果。
///
/// 成功与失败互斥，用 enum 消除 `(text, Option<failure>)` 组合中
/// "同时有 text 又有 failure" 的非法状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TurnOutcome {
    /// 本轮正常完成，携带最终纯文本输出。
    Success {
        /// 本轮最终纯文本输出
        text: String,
    },
    /// 本轮异常终止。
    Failed {
        /// 失败原因
        reason: TurnFailureReason,
        /// 失败前累积的部分文本（可能为空）
        partial_text: String,
    },
}

impl TurnOutcome {
    /// 成功时返回文本，失败时返回 `None`。
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Success { text } => Some(text),
            Self::Failed { .. } => None,
        }
    }

    /// 失败时返回原因，成功时返回 `None`。
    pub fn failure_reason(&self) -> Option<&TurnFailureReason> {
        match self {
            Self::Success { .. } => None,
            Self::Failed { reason, .. } => Some(reason),
        }
    }
}

// ============================================================================
// LooperEvent — 内部事件
// ============================================================================

/// AgentLooper 内部事件（不直接暴露 ModelStreamEvent）。
///
/// 后续可通过 adapter 层转换为 `crate::agent::stream::ModelStreamEvent`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum LooperEvent {
    /// 文本增量
    TextDelta { delta: String },
    /// 推理增量
    ReasoningDelta { delta: String },
    /// 流式 tool call 增量
    ToolCallDelta {
        id: String,
        name: Option<String>,
        arguments: String,
    },
    /// 完整 tool call，准备执行
    ToolCallStart {
        id: String,
        name: String,
        arguments: String,
    },
    /// Tool 执行结果
    ///
    /// `result` 为文本视图；`images` 携带输出中的图片部件 URL（data URI 或
    /// https URL），纯文本工具结果恒为空。
    ToolResult {
        id: String,
        name: String,
        result: String,
        images: Vec<String>,
    },
    /// 模型调用 token 用量
    ModelUsage {
        /// Zero-based index of this call within the run
        call_index: usize,
        usage: Usage,
    },

    // ── 生命周期事件 ──────────────────────────────────────────────────────
    /// ReAct 内层状态转换。
    ///
    /// 在 `react_step()` 状态切换后发出，让外部可追踪 looper 完整生命周期。
    ReactStateChange {
        turn_index: usize,
        from: ReActState,
        to: ReActState,
    },

    /// 外层状态转换。
    OuterStateChange { from: OuterState, to: OuterState },

    /// 新一轮开始。
    TurnStart {
        turn_index: usize,
        /// 本轮用户输入文本
        user_input: String,
    },

    /// 本轮完成（Done 或 Failed 收尾阶段）。
    ///
    /// `outcome` 通过 [`TurnOutcome`] 枚举区分成功/失败，
    /// 成功时携带最终纯文本，失败时携带原因及部分文本。
    /// 外部可直接读取 `outcome.text()` 获取文本，无需拼接
    /// [`TextDelta`](LooperEvent::TextDelta)。
    TurnComplete {
        turn_index: usize,
        /// 本轮结果：成功（含文本）或失败（含原因和部分文本）
        outcome: TurnOutcome,
        /// 本轮 token 用量（该轮模型调用用量）
        usage: Usage,
    },

    /// 上下文滚动压缩完成（历史轮被结构化摘要替换并物理驱逐）。
    ///
    /// 仅当 `LooperConfig::compaction` 已配置且阈值触发时发出。
    /// 前端可据此渲染「更早对话已归档」分隔线。
    ContextCompacted {
        /// 物理驱逐的轮数
        evicted_turns: usize,
        /// 合并后的结构化摘要（已含定界标签）
        summary: String,
        /// 压缩前估算 token
        estimated_tokens_before: usize,
        /// 压缩后估算 token
        estimated_tokens_after: usize,
    },

    /// Looper 即将退出 `run()` 方法。
    Shutdown {
        reason: String,
        total_turns: usize,
        total_usage: Usage,
    },
}

// ============================================================================
// UserMsg  — 外部通信
// ============================================================================

/// 用户输入消息
#[derive(Debug, Clone)]
pub enum UserMsg {
    /// 用户查询（纯文本或文本 + 图片部件混排）
    Query(Content),
    /// 关闭请求
    Shutdown,
}

// ============================================================================
// LooperHandle — 统一外部控制面
// ============================================================================

/// AgentLooper 的唯一外部操作入口。
///
/// 创建方式：`AgentLooper::spawn(agent, session, config)`。
///
/// # 生命周期
///
/// ```text
/// let h = AgentLooper::spawn(agent, session, config);
///
/// h.send_query("...").await;   // 发送用户输入
/// h.recv_event().await;         // 接收文本/tool/状态事件
/// h.cancel();                   // 请求取消
/// h.pause(); / h.resume();      // 控制流程
/// h.wait().await;               // 等待完成并获取结果
/// ```
///
/// Looper 后台任务 handle。
///
/// 内部使用 `Arc` 共享所有权。最后一个 clone 被 drop 时**只设置 cancel_flag**，
/// 让 looper 在下一个循环迭代里自行收尾退出 —— 不 abort。
/// abort 会跳过失败收尾（冻结 + 落盘），把中断轮丢掉。
/// `try_lock` 保证与 `wait()` 方法无竞态。
struct OwnedTask {
    inner: SharedModelTask,
    /// 取消标志：drop 时先设置此标志通知 looper 退出，再 abort 任务。
    cancel_flag: Arc<AtomicBool>,
}

impl Clone for OwnedTask {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            cancel_flag: Arc::clone(&self.cancel_flag),
        }
    }
}

impl Drop for OwnedTask {
    fn drop(&mut self) {
        // strong_count == 1 表示这是最后一个引用
        if Arc::strong_count(&self.inner) == 1 {
            // 设置取消标志作为安全网：若 looper 仍在运行，会在下个循环迭代中正常退出。
            // looper 可能已通过 shutdown()/wait() 正常结束，此时 cancel_flag 无实际作用。
            self.cancel_flag.store(true, Ordering::Release);
            debug!(
                "LooperHandle dropped (last reference). \
                 Cancel flag set as safety net for any still-running looper."
            );
        }
    }
}

impl OwnedTask {
    fn new(
        handle: tokio::task::JoinHandle<Result<ModelResponse, AgentError>>,
        cancel_flag: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(Some(handle))),
            cancel_flag,
        }
    }

    /// 取出 `JoinHandle` 并等待完成。仅能调用一次。
    async fn take_handle(
        &self,
    ) -> Option<tokio::task::JoinHandle<Result<ModelResponse, AgentError>>> {
        self.inner.lock().await.take()
    }
}

/// `Clone` 可让多个持有者共享控制（内部全部 `Arc`）。
pub struct LooperHandle {
    /// 向 looper 发送用户输入 / 控制命令
    user_speaker: Speaker<UserMsg>,
    /// 接收 looper 事件（Mutex 包裹以支持 Clone）
    event_listener: Arc<tokio::sync::Mutex<Listener<LooperEvent>>>,
    /// 取消标志
    cancel_flag: Arc<AtomicBool>,
    /// 暂停标志
    pause_flag: Arc<AtomicBool>,
    /// looper 后台任务 handle（最后 drop 时自动 abort）
    task_handle: OwnedTask,
}

impl LooperHandle {
    // ── 输入 ──────────────────────────────────────────────────────────────

    /// 向 agent 发送用户查询。
    ///
    /// 若 looper 正在处理上一轮，消息会进入 Session 的 pending 队列，
    /// 当前轮完成后自动处理。若 looper 已结束，返回错误。
    pub async fn send_query(
        &self,
        text: String,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<UserMsg>> {
        self.send_query_content(Content::Text(text)).await
    }

    /// 发送携带图片部件的用户查询（[`Content::Parts`]）。
    ///
    /// 纯文本查询走 [`Self::send_query`] 即可；部件混排时文本与图片部件
    /// 整体进入 Session 的用户输入（rollback 重排队同样保留部件）。
    pub async fn send_query_content(
        &self,
        content: Content,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<UserMsg>> {
        self.user_speaker.send(UserMsg::Query(content)).await
    }

    // ── 控制 ──────────────────────────────────────────────────────────────

    /// 请求取消当前执行。
    ///
    /// - 正在进行的模型调用不会被中断（取决于 provider），但不会再发起新调用
    /// - 正在执行的 tool 会被 abort
    /// - pending 队列中的消息会保留
    /// - looper 状态变为 Failed，failure_reason = TurnFailureReason::Cancelled
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Release);
    }

    /// 请求暂停。looper 在下一轮 `react_step()` 调度前挂起。
    ///
    /// 暂停期间仍可通过 `send_query` 将消息放入 pending 队列。
    /// 调用 `resume()` 恢复执行。
    pub fn pause(&self) {
        self.pause_flag.store(true, Ordering::Release);
    }

    /// 恢复暂停的 looper。
    pub fn resume(&self) {
        self.pause_flag.store(false, Ordering::Release);
    }

    /// 优雅关闭：发送 Shutdown 信号，等待 looper 自然退出。
    pub async fn shutdown(&self) -> Result<ModelResponse, AgentError> {
        let _ = self.user_speaker.send(UserMsg::Shutdown).await;
        self.wait().await
    }

    // ── 事件接收 ──────────────────────────────────────────────────────────

    /// 异步接收下一个 looper 事件。
    ///
    /// 返回 `None` 表示事件通道已关闭（looper 已退出）。
    /// 注意：此方法持有内部锁直到事件到达，期间 `drain_events()` 会返回空。
    pub async fn recv_event(&self) -> Option<LooperEvent> {
        self.event_listener.lock().await.recv().await
    }

    /// 收集所有当前可用的事件（非阻塞 drain）。
    ///
    /// 若 `recv_event()` 正在等待，此方法返回空 vec。
    pub fn drain_events(&self) -> Vec<LooperEvent> {
        let mut events = Vec::new();
        if let Ok(mut listener) = self.event_listener.try_lock() {
            while let Ok(event) = listener.try_recv() {
                events.push(event);
            }
        }
        events
    }

    // ── 状态查询 ──────────────────────────────────────────────────────────

    /// 取消标志是否已触发。
    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Acquire)
    }

    /// 暂停标志是否已触发。
    pub fn is_paused(&self) -> bool {
        self.pause_flag.load(Ordering::Acquire)
    }

    /// looper 后台任务是否仍在运行。
    pub fn is_running(&self) -> bool {
        match self.task_handle.inner.try_lock() {
            Ok(guard) => guard.as_ref().is_some_and(|h| !h.is_finished()),
            Err(_) => false,
        }
    }

    /// 获取取消标志的克隆（用于跨线程共享）。
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel_flag)
    }

    /// 获取暂停标志的克隆（用于跨线程共享）。
    pub fn pause_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.pause_flag)
    }

    // ── 结果等待 ──────────────────────────────────────────────────────────

    /// 等待 looper 完成，返回结果。
    ///
    /// 若 looper 尚未完成，异步等待。若已完成，立即返回。
    /// 只能调用一次（内部 take JoinHandle）。
    pub async fn wait(&self) -> Result<ModelResponse, AgentError> {
        let handle = self.task_handle.take_handle().await;

        match handle {
            Some(h) => match h.await {
                Ok(result) => result,
                Err(join_err) => Err(AgentError::AgentProtocol(format!(
                    "Looper task panicked: {join_err}"
                ))),
            },
            None => Err(AgentError::AgentProtocol("Looper already consumed".into())),
        }
    }
}

impl Clone for LooperHandle {
    fn clone(&self) -> Self {
        Self {
            user_speaker: self.user_speaker.clone(),
            event_listener: Arc::clone(&self.event_listener),
            cancel_flag: Arc::clone(&self.cancel_flag),
            pause_flag: Arc::clone(&self.pause_flag),
            task_handle: self.task_handle.clone(),
        }
    }
}

// ============================================================================
// AgentLooper 主结构
// ============================================================================

/// Agent 执行循环的核心状态机。
///
/// # 使用方式
///
/// **推荐** — 通过 `spawn()` 一键启动并获得 `LooperHandle`：
///
/// ```ignore
/// use peco_core::agent::LooperConfig;
/// let h = AgentLooper::spawn(agent, session, LooperConfig::default());
/// h.send_query("hello".into()).await?;
/// while let Some(event) = h.recv_event().await { ... }
/// let result = h.wait().await?;
/// ```
///
/// **高级** — 手动创建，直接调用 `run()`：
///
/// ```ignore
/// use peco_core::utils::intercom::make_async_intercom_pair;
/// let (looper_side, caller_side) = make_async_intercom_pair::<LooperEvent, UserMsg>(256);
/// let (event_speaker, user_listener) = looper_side.split();
/// let looper = AgentLooper::new(
///     agent, session, event_speaker, cancel_flag, pause_flag, config,
/// );
/// let result = looper.run(user_listener).await?;
/// ```
pub struct AgentLooper {
    // ── 静态配置 ──
    agent: Arc<Agent>,
    max_turns: usize,
    config: LooperConfig,
    /// 稳定前缀：`agent.system_prompt()` + `config.environment`，
    /// 构造时计算一次并缓存，避免每次 turn 重新拼接。
    /// 此后每轮仅在此基础上追加 dynamic context。
    stable_prefix: String,

    /// 当前 turn 缓存的动态上下文字符串。
    /// 在 [`prepare_and_send_request`] 检测到新 query 时更新，
    /// 同一 turn 内多次 ReAct 迭代复用该值。
    dynamic_context: Option<String>,

    // ── 会话 ──
    /// Session 持有全部对话状态（committed + staging + pending + turn_index + usage）。
    session: Box<Session>,

    // ── 状态机 ──
    outer_state: OuterState,
    react_state: ReActState,
    react_ctx: ReActContext,

    // ── 运行时追踪（不可持久化）──
    /// looper run 启动时间（用于 total_timeout）
    run_start_time: Option<Instant>,
    /// 本轮开始时间（用于 per_turn_timeout）
    turn_start: Option<Instant>,
    /// 本轮失败原因；`None` 表示尚未失败 / 正常完成
    failure_reason: Option<TurnFailureReason>,

    // ── 重命名的 turn 概念 ──
    /// ReAct 循环迭代计数：当前对话轮次中已发出的模型调用次数。
    ///
    /// 与 Session 的 `turn_index`（对话轮次）不同，此计数器在每次用户输入
    /// 开始新对话轮次时重置为 0，每次回到 `PreparingRequest` 时递增。
    /// 用于 `max_turns` 限制——限制的是单次对话轮次内得到最终结果
    /// 所需的模型调用轮数，而非对话轮数。
    react_loop_iteration: usize,

    // ── 暂停状态恢复 ──
    /// 进入 `Paused` 状态前的外层状态，用于 resume 时恢复。
    pre_pause_state: Option<OuterState>,

    // ── 事件输出 ──
    event_speaker: Speaker<LooperEvent>,

    // ── 取消控制 ──
    cancel_flag: Arc<AtomicBool>,

    // ── 暂停控制 ──
    pause_flag: Arc<AtomicBool>,

    // ── 持久化 ──
    persister: Arc<dyn crate::persistence::SessionPersister>,

    // ── 纯运行时状态（不可持久化）──
    /// streaming 模式：活跃的 [`GenerateStream`]
    active_stream: Option<GenerateStream>,
    /// streaming 模式：中立块组装器（跨 chunk 持久）
    stream_assembler: BlockAssembler,
    /// streaming 模式：本次请求收到的 [`StreamChunk::Finish`] 原因。
    /// `BlockAssembler` 只保留收敛后的 status，归因（是截断还是上游异常）要靠原始原因，
    /// 故单独留存供失败日志使用；`None` = 流结束但从未收到 `Finish`。
    last_finish_reason: Option<FinishReason>,
    /// 活跃的 tool 执行任务集（增量执行模式：Spawn → Poll → 完成）
    active_tool_tasks: Option<tokio::task::JoinSet<(usize, ToolCallResult)>>,
    /// 最近一次请求的估算上下文 token（响应到达后与实际 usage 对照）。
    /// 估算器单点在 [`super::context`]。
    last_request_estimated_tokens: Option<usize>,
}

impl AgentLooper {
    /// 创建新的 AgentLooper 实例（高级用法）。
    ///
    /// 推荐使用 [`spawn()`](AgentLooper::spawn) 一键创建。
    ///
    /// # 参数
    ///
    /// - `agent` — 已组装的 Agent 实例
    /// - `session` — 对话会话（含历史消息）
    /// - `event_speaker` — 事件广播通道
    /// - `cancel_flag` — 取消标志（外部设置 true 时终止循环）
    /// - `pause_flag` — 暂停标志（外部设置 true 时挂起循环）
    /// - `config` — looper 配置（超时、hook 链等）
    pub fn new(
        agent: Arc<Agent>,
        session: Box<Session>,
        event_speaker: Speaker<LooperEvent>,
        cancel_flag: Arc<AtomicBool>,
        pause_flag: Arc<AtomicBool>,
        config: LooperConfig,
        persister: Arc<dyn crate::persistence::SessionPersister>,
    ) -> Self {
        let max_turns = agent.max_turns();
        // 在 config 被 move 进结构体之前求值稳定前缀
        let stable_prefix =
            compose_stable_prefix(&agent.system_prompt(), config.environment.as_deref());

        AgentLooper {
            agent,
            max_turns,
            config,
            stable_prefix,
            dynamic_context: None,
            session,
            outer_state: OuterState::Idle,
            react_state: ReActState::PreparingRequest,
            react_ctx: ReActContext::default(),
            run_start_time: None,
            turn_start: None,
            failure_reason: None,
            react_loop_iteration: 0,
            pre_pause_state: None,
            event_speaker,
            cancel_flag,
            pause_flag,
            persister,
            active_stream: None,
            stream_assembler: BlockAssembler::new(),
            last_finish_reason: None,
            active_tool_tasks: None,
            last_request_estimated_tokens: None,
        }
    }

    /// 一键创建 AgentLooper 并返回 `LooperHandle`。
    ///
    /// 一次性完成：cancel_flag、pause_flag、intercom 创建拆分、spawn 后台任务。
    /// 外部只需操作返回的 `LooperHandle`。
    pub fn spawn(
        agent: Arc<Agent>,
        session: Box<Session>,
        config: LooperConfig,
        persister: Arc<dyn crate::persistence::SessionPersister>,
    ) -> LooperHandle {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let pause_flag = Arc::new(AtomicBool::new(false));
        let (looper_side, caller_side) =
            make_async_intercom_pair::<LooperEvent, UserMsg>(config.event_buffer);
        let (event_speaker, user_listener) = looper_side.split();
        let (user_speaker, event_listener) = caller_side.split();

        let mut looper = AgentLooper::new(
            agent,
            session,
            event_speaker,
            cancel_flag.clone(),
            pause_flag.clone(),
            config,
            persister,
        );

        let agent_name = looper.agent.config().agent.name.clone();
        let session_id = looper.session.id().to_owned();

        debug!(
            agent = %agent_name,
            session_id = %session_id,
            "AgentLooper spawned"
        );

        let handle = tokio::spawn(async move { looper.run(user_listener).await });

        let task_handle = OwnedTask::new(handle, cancel_flag.clone());

        LooperHandle {
            user_speaker,
            event_listener: Arc::new(tokio::sync::Mutex::new(event_listener)),
            cancel_flag,
            pause_flag,
            task_handle,
        }
    }

    // ── 内部辅助方法 ────────────────────────────────────────────────────────

    /// 检查取消标志是否被触发。
    fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Acquire)
    }

    /// 非阻塞发送普通事件；若 channel 满或接收端关闭则静默丢弃。
    /// 适用于高频增量事件（TextDelta、ToolCallDelta 等）。
    fn emit_event(&self, event: LooperEvent) {
        let _ = self.event_speaker.try_send(event);
    }

    /// 贪婪排空 `JoinSet` 中已完成 task 的结果，返回捞回的条数。非阻塞。
    ///
    /// 关联函数（非方法）— 只碰 `JoinSet` 与 `pending_tool_calls`，可完全绕开
    /// looper 单测。`try_join_next` 返回 `None` 表示「当前没有已完成的 task」，
    /// **不等于** JoinSet 已空。
    ///
    /// `Some(Err(_))` 只 warn 不 break：一个 task panic 不应吞掉兄弟 task 的成果。
    ///
    /// 与 `join_next().await` 排空的区别：`abort()` 是**请求**，task 在下一个
    /// await 点才停；若工具内有同步阻塞（`std::process::Command` 而非
    /// `tokio::process`），await 排空会等它跑完 —— 正是用户最不想要的。
    /// `abort_all()` + drop 是正确取舍：长同步工具可能仍跑完，这是接受的行为。
    fn drain_finished_join_results(
        joinset: &mut tokio::task::JoinSet<(usize, ToolCallResult)>,
        pending: &mut [PendingToolCall],
    ) -> usize {
        let mut drained = 0;
        loop {
            match joinset.try_join_next() {
                Some(Ok((idx, tr))) => {
                    pending[idx].result = Some(tr);
                    drained += 1;
                }
                // 已 abort / panic 的 task：warn 后继续。
                // 绝不能 break —— 会漏掉队列里后面那些已经跑完的兄弟 task。
                Some(Err(e)) => {
                    warn!(error = %e, "Tool task ended abnormally during drain");
                }
                None => break,
            }
        }
        drained
    }

    /// 异步发送关键事件，保证送达。
    /// 适用于生命周期终结事件（TurnComplete、Shutdown 等），
    /// 确保消费者不会遗漏。
    ///
    /// 关联函数（非方法），避免借 `&self` 导致 future `!Send`。
    async fn emit_event_guaranteed(speaker: &Speaker<LooperEvent>, event: LooperEvent) {
        let _ = speaker.send(event).await;
    }

    /// 发送 ReactStateChange 事件，并记录调试日志。
    fn emit_react_state_change(&self, from: ReActState, to: ReActState, turn_index: usize) {
        if from != to {
            debug!(
                agent = %self.agent.config().agent.name,
                session_id = %self.session.id(),
                turn = turn_index,
                from = ?from,
                to = ?to,
                "ReAct state changed"
            );
            self.emit_event(LooperEvent::ReactStateChange {
                turn_index,
                from,
                to,
            });
        }
    }

    /// 发送 OuterStateChange 事件，并记录调试日志。
    fn emit_outer_state_change(&self, from: OuterState, to: OuterState) {
        if from != to {
            debug!(
                agent = %self.agent.config().agent.name,
                session_id = %self.session.id(),
                from = ?from,
                to = ?to,
                "Outer state changed"
            );
            self.emit_event(LooperEvent::OuterStateChange { from, to });
        }
    }

    // ── Hook 调用辅助函数 ──────────────────────────────────────────────────
    //
    // NOTE: 这些是关联函数（非方法），直接接收 hooks 切片，避免 `&self` 跨越 await point。
    // 由于 AgentLooper 不是 Sync（含 non-Sync 的 GenerateStream），
    // `&self` 不能在 tokio::spawn 的 future 中跨 await 持有。

    async fn invoke_on_before_request(
        hooks: &[Arc<dyn LooperHook>],
        turn: usize,
        messages: &mut Vec<Arc<InputItem>>,
    ) -> HookAction {
        for hook in hooks {
            match hook.on_before_request(turn, messages).await {
                HookAction::Continue => continue,
                other => return other,
            }
        }
        HookAction::Continue
    }

    async fn invoke_on_after_response(
        hooks: &[Arc<dyn LooperHook>],
        turn: usize,
        response: &GenerateResult,
    ) -> HookAction {
        for hook in hooks {
            match hook.on_after_response(turn, response).await {
                HookAction::Continue => continue,
                other => return other,
            }
        }
        HookAction::Continue
    }

    async fn invoke_on_text_delta(
        hooks: &[Arc<dyn LooperHook>],
        turn: usize,
        delta: &str,
        accumulated: &str,
    ) -> HookAction {
        for hook in hooks {
            match hook.on_text_delta(turn, delta, accumulated).await {
                HookAction::Continue => continue,
                other => return other,
            }
        }
        HookAction::Continue
    }

    async fn invoke_on_before_tool(
        hooks: &[Arc<dyn LooperHook>],
        turn: usize,
        tool_call: &ToolCall,
    ) -> ToolHookAction {
        for hook in hooks {
            match hook.on_before_tool(turn, tool_call).await {
                ToolHookAction::Continue => continue,
                other => return other,
            }
        }
        ToolHookAction::Continue
    }

    async fn invoke_on_after_tool(
        hooks: &[Arc<dyn LooperHook>],
        turn: usize,
        tool_call: &ToolCall,
        result: &str,
        is_error: bool,
    ) {
        for hook in hooks {
            hook.on_after_tool(turn, tool_call, result, is_error).await;
        }
    }

    async fn invoke_on_turn_complete(
        hooks: &[Arc<dyn LooperHook>],
        turn: usize,
        failure: Option<&TurnFailureReason>,
        usage: &Usage,
        session: &Session,
    ) {
        for hook in hooks {
            hook.on_turn_complete(turn, failure, usage, session).await;
        }
    }

    async fn invoke_on_context_compacted(
        hooks: &[Arc<dyn LooperHook>],
        outcome: &CompactionOutcome,
    ) {
        for hook in hooks {
            hook.on_context_compacted(outcome).await;
        }
    }

    /// 对照请求前的估算 token 与 API 返回的实际 input_tokens。
    ///
    /// 只在 debug 级别记录 — 数据用于长期观测估算器偏差，非运行时告警。
    fn log_estimate_calibration(&mut self, turn: usize, actual_input_tokens: u32) {
        if let Some(estimated) = self.last_request_estimated_tokens.take() {
            let ratio = if actual_input_tokens > 0 {
                estimated as f64 / actual_input_tokens as f64
            } else {
                0.0
            };
            debug!(
                turn,
                estimated,
                actual = actual_input_tokens,
                ratio = format!("{ratio:.2}"),
                "Token estimate calibration (estimated / actual)"
            );
        }
    }

    async fn invoke_on_react_state_change(
        hooks: &[Arc<dyn LooperHook>],
        turn: usize,
        from: ReActState,
        to: ReActState,
    ) {
        for hook in hooks {
            hook.on_react_state_change(turn, from, to).await;
        }
    }

    async fn invoke_on_outer_state_change(
        hooks: &[Arc<dyn LooperHook>],
        from: OuterState,
        to: OuterState,
    ) {
        for hook in hooks {
            hook.on_outer_state_change(from, to).await;
        }
    }

    // ── 对外公共 API ───────────────────────────────────────────────────────

    /// 获取此 looper 关联的 Agent 的 Arc 克隆。
    pub fn agent(&self) -> Arc<Agent> {
        Arc::clone(&self.agent)
    }

    /// 获取会话引用。
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// 获取当前外层状态。
    pub fn outer_state(&self) -> OuterState {
        self.outer_state
    }

    /// 获取当前内层状态。
    pub fn react_state(&self) -> ReActState {
        self.react_state
    }

    /// 获取已执行轮数（从 Session 读取）。
    pub fn turn_count(&self) -> usize {
        self.session.turn_index()
    }

    /// 获取聚合 token 用量（从 Session 读取）。
    pub fn total_usage(&self) -> Usage {
        self.session.total_usage()
    }

    /// 返回是否已完成（Idle 且无错误）。
    pub fn is_done(&self) -> bool {
        matches!(self.outer_state, OuterState::Idle) && matches!(self.react_state, ReActState::Done)
    }

    // ── run() 主循环 ──────────────────────────────────────────────────────

    /// 执行 agent run 循环。
    ///
    /// 外层通过 `user_listener` 接收 `UserMsg`；
    /// 内部执行 ReAct 状态机直至 Done / Failed / 超过 max_turns / 取消 / 超时。
    ///
    /// 当 input channel 关闭后（所有 `Speaker` 被 drop），`run()` 不会立即退出，
    /// 而是等待内层 ReAct 循环自然完成后再退出。这确保 `drop(user_speaker)` 后
    /// 仍能正常完成最后一轮对话处理。
    pub async fn run(
        &mut self,
        mut user_listener: Listener<UserMsg>,
    ) -> Result<ModelResponse, AgentError> {
        info!(
            agent = %self.agent.config().agent.name,
            session_id = %self.session.id(),
            max_turns = self.max_turns,
            "AgentLooper::run() started"
        );

        // Ensure deferred MCP connections are established before first tool use.
        self.agent.mcp_manager().ensure_connected().await;

        // 用户输入 channel 是否已关闭（所有 sender 被 drop）。
        // 关闭后不再尝试接收新输入，专注驱动 react_step 直至 Idle。
        let mut input_closed = false;
        // 记录 run 启动时间（若 handle_user_query 未设置则以此为基准）
        let run_start = Instant::now();

        loop {
            // ── 消费 react_step 挂起的失败收尾 ────────────────────────────
            // 必须在取消/超时检查**之前** —— 否则 failure_reason 已被 take，
            // 取消分支会用 Cancelled 覆盖真实原因（如 HookAbort）。
            if matches!(self.react_state, ReActState::Failed) {
                // 兜底：非取消类失败若未设原因则记 Other
                let reason = self
                    .failure_reason
                    .take()
                    .unwrap_or(TurnFailureReason::Other("failed".into()));
                let cancelled = self.is_cancelled();
                // drain_pending 由 !is_cancelled() 推导：用户按了停就不续接排队输入
                if self.finalize_failure(reason, !cancelled).await {
                    // 续接了 pending，保留「失败后自动续接」行为
                    continue;
                }
                if cancelled {
                    // 取消标志常驻，回到循环顶部会立刻再次命中取消检查 → 自旋。
                    // 收尾已完成，此处直接退出。
                    break;
                }
                // 非取消失败：收尾已把 session 置回 `Idle`、内层状态机带回 `Done`，
                // 回到循环顶部停靠等下一句输入 —— 与正常完成后的停靠行为一致。
                continue;
            }

            // ── 检查取消 ──────────────────────────────────────────────────
            if self.is_cancelled() {
                if self
                    .finalize_failure(TurnFailureReason::Cancelled, false)
                    .await
                {
                    continue;
                }
                break;
            }

            // ── 检查总超时 ────────────────────────────────────────────────
            if let Some(total_timeout) = self.config.total_timeout {
                let base = self.run_start_time.unwrap_or(run_start);
                if base.elapsed() > total_timeout {
                    if self
                        .finalize_failure(TurnFailureReason::TotalTimeout, false)
                        .await
                    {
                        continue;
                    }
                    break;
                }
            }

            // ── 暂停时只接收用户输入 ──────────────────────────────────────
            if self.pause_flag.load(Ordering::Acquire) {
                // ★ 进入 Paused 状态（仅首次，避免重复 emit）
                if !matches!(self.outer_state, OuterState::Paused) {
                    let old = self.outer_state;
                    self.pre_pause_state = Some(old);
                    self.outer_state = OuterState::Paused;
                    self.emit_outer_state_change(old, OuterState::Paused);
                }

                if input_closed {
                    break;
                }
                match user_listener.recv().await {
                    Some(UserMsg::Query(content)) => {
                        // 暂停期间收到的输入放入 pending 队列
                        info!("Message queued (looper paused). Will process after resume.");
                        self.session.enqueue_pending(content);
                    }
                    Some(UserMsg::Shutdown) => break,
                    None => {
                        input_closed = true;
                    }
                }
                continue;
            }

            // ★ 从 Paused 恢复（pause_flag 变为 false）
            if matches!(self.outer_state, OuterState::Paused) {
                let prev = self.pre_pause_state.take().unwrap_or(OuterState::Idle);
                self.outer_state = prev;
                self.emit_outer_state_change(OuterState::Paused, prev);
            }

            // ── channel closed ────────────────────────────────────────────
            if input_closed {
                if matches!(self.outer_state, OuterState::RunningInnerLoop) {
                    self.react_step().await;
                } else {
                    // Idle + 无更多输入 → 退出
                    break;
                }
                continue;
            }

            // ── 主 select ─────────────────────────────────────────────────
            tokio::select! {
                biased;

                // 用户输入优先
                maybe_msg = user_listener.recv() => {
                    match maybe_msg {
                        Some(UserMsg::Query(content)) => {
                            self.handle_user_query(content).await?;
                        }
                        Some(UserMsg::Shutdown) => {
                            break;
                        }
                        None => {
                            // Channel closed — 标记并继续，让 react loop 自然完成
                            input_closed = true;
                        }
                    }
                }

                // 内层 ReAct 状态机步进（仅在 RunningInnerLoop 时激活）
                _ = self.react_step(), if matches!(self.outer_state, OuterState::RunningInnerLoop) => {}
            }

            // NOTE: Idle 状态表示等待下一个用户输入，
            // 不应退出循环。退出仅在 input_closed + Idle 或收到 Shutdown 时触发。
        }

        let (usage, turns) = {
            let u = self.session.total_usage();
            let t = self.session.turn_index();
            (u, t)
        };

        // Emit Shutdown 事件
        let shutdown_reason = self
            .failure_reason
            .as_ref()
            .map(|r| format!("{:?}", r))
            .unwrap_or_else(|| "done".to_string());

        info!(
            agent = %self.agent.config().agent.name,
            session_id = %self.session.id(),
            reason = %shutdown_reason,
            total_turns = turns,
            total_tokens = usage.total_tokens,
            "AgentLooper::run() finished"
        );

        Self::emit_event_guaranteed(
            &self.event_speaker,
            LooperEvent::Shutdown {
                reason: shutdown_reason,
                total_turns: turns,
                total_usage: usage.clone(),
            },
        )
        .await;

        Ok(self.build_model_response(usage, turns))
    }

    // ── 用户输入处理 ──────────────────────────────────────────────────────

    /// 处理用户查询：根据 Session 状态决定直接启动 turn 或放入 pending 队列。
    async fn handle_user_query(&mut self, content: Content) -> Result<(), AgentError> {
        // 事件面保持文本视图；部件整体进入 Session（rollback 重排队同样保留）
        let text = content.text_view().into_owned();
        match self.session.state() {
            SessionState::Idle => {
                // 直接启动新 turn
                self.session
                    .start_turn(content)
                    .map_err(|e| AgentError::AgentProtocol(e.to_string()))?;

                // 记录启动时间（首次查询时记录 run_start_time）
                if self.run_start_time.is_none() {
                    self.run_start_time = Some(Instant::now());
                }
                self.turn_start = Some(Instant::now());

                // ★ 新对话轮次：重置 ReAct 循环计数
                self.react_loop_iteration = 0;

                let old_outer = self.outer_state;
                self.outer_state = OuterState::RunningInnerLoop;
                self.react_state = ReActState::PreparingRequest;
                self.failure_reason = None;

                // Emit 状态变更事件
                self.emit_outer_state_change(old_outer, self.outer_state);

                self.emit_event(LooperEvent::TurnStart {
                    turn_index: self.session.turn_index(),
                    user_input: text,
                });
            }
            SessionState::Active | SessionState::Cancelling | SessionState::Interrupted => {
                // InnerLoop 进行中或收尾中 — 放入 pending 队列；
                // 整体入队保留部件，与 turn 启动、rollback 重排队对称
                info!("Message queued. Will process after current turn.");
                self.session.enqueue_pending(content);
            }
        }
        Ok(())
    }

    // ── ReAct 状态机步进 ─────────────────────────────────────────────────

    /// 执行 ReAct 状态机的一步。
    ///
    /// 当内层循环未激活时（outer_state != RunningInnerLoop），此 future 永久 pending，
    /// 让 `select!` 只处理用户输入。
    async fn react_step(&mut self) {
        // Per-turn timeout 检查
        if let (Some(timeout), Some(turn_start)) = (self.config.per_turn_timeout, self.turn_start)
            && turn_start.elapsed() > timeout
        {
            self.failure_reason = Some(TurnFailureReason::PerTurnTimeout);
            self.react_state = ReActState::Failed;
            // 不 return — 让 Failed 分支处理收尾
        }

        let old_react_state = self.react_state;
        // ★ Session 零锁：turn_index() 是字段访问，无需缓存
        let turn = self.session.turn_index();

        match self.react_state {
            ReActState::PreparingRequest => {
                self.prepare_and_send_request(turn).await;
            }

            // ── batch 分支 ──
            ReActState::ResolvingResponse => {
                self.resolve_batch_response(turn).await;
            }

            // ── streaming 分支 ──
            ReActState::Streaming => {
                self.consume_stream_chunk(turn).await;
            }

            // ── 共享后续状态 ──
            ReActState::ExecutingTools => {
                self.execute_tools_step(turn).await;
            }

            ReActState::Done => {
                // Done = 正常完成，failure_reason 必定为 None
                debug_assert!(
                    self.failure_reason.is_none(),
                    "Done state should have no failure reason"
                );
                let _ = self.failure_reason.take();

                // 提交当前 turn
                let _token = match self.session.commit_turn() {
                    Ok(token) => token,
                    Err(e) => {
                        error!(error = %e, "Failed to commit turn");
                        self.react_state = ReActState::Failed;
                        return;
                    }
                };

                // Emit TurnComplete + hook
                let usage = self.session.total_usage();
                let outcome = TurnOutcome::Success {
                    text: std::mem::take(&mut self.react_ctx.assistant_text),
                };
                Self::emit_event_guaranteed(
                    &self.event_speaker,
                    LooperEvent::TurnComplete {
                        turn_index: turn,
                        outcome: outcome.clone(),
                        usage: usage.clone(),
                    },
                )
                .await;
                Self::invoke_on_turn_complete(
                    &self.config.hooks,
                    turn,
                    outcome.failure_reason(),
                    &usage,
                    &self.session,
                )
                .await;

                // ★ 持久化：turn 边界触发（commit 后）
                let snapshot = self.session.snapshot(&_token);
                if let Err(e) = self
                    .persister
                    .save(
                        &snapshot,
                        self.session.id(),
                        self.session.description(),
                        self.session.created_at(),
                    )
                    .await
                {
                    error!(error = %e, "Failed to persist session after turn commit");
                }
                // ★ 本轮已入史，在途检查点作废（早于压缩：压缩只改 committed 内容）。
                self.clear_inflight_checkpoint().await;

                // ★ 上下文滚动压缩：turn 边界（提交并持久化后、pending 续接前）。
                //   非致命：摘要模型失败只记日志，不影响会话继续。
                if let Some(policy) = &self.config.compaction {
                    match policy.maybe_compact(&mut self.session).await {
                        Ok(Some(outcome)) => {
                            info!(
                                evicted_turns = outcome.evicted_turns,
                                tokens_before = outcome.estimated_tokens_before,
                                tokens_after = outcome.estimated_tokens_after,
                                "Context compacted at turn boundary"
                            );
                            Self::emit_event_guaranteed(
                                &self.event_speaker,
                                LooperEvent::ContextCompacted {
                                    evicted_turns: outcome.evicted_turns,
                                    summary: outcome.summary.clone(),
                                    estimated_tokens_before: outcome.estimated_tokens_before,
                                    estimated_tokens_after: outcome.estimated_tokens_after,
                                },
                            )
                            .await;

                            // 重新持久化：快照现在含 pinned 摘要 + 修剪后的历史
                            let snapshot = self.session.snapshot(&_token);
                            if let Err(e) = self
                                .persister
                                .save(
                                    &snapshot,
                                    self.session.id(),
                                    self.session.description(),
                                    self.session.created_at(),
                                )
                                .await
                            {
                                error!(error = %e, "Failed to persist session after compaction");
                            }

                            // 持久化完成后通知 hooks（纯观察，失败不致命）
                            Self::invoke_on_context_compacted(&self.config.hooks, &outcome).await;
                        }
                        Ok(None) => {}
                        Err(e) => {
                            error!(error = %e, "Context compaction failed (non-fatal)");
                        }
                    }
                }

                // 检查是否有 pending 输入自动续接
                match self.session.dequeue_and_start_turn() {
                    Ok(true) => {
                        // ★ 新对话轮次：重置 ReAct 循环计数
                        self.react_loop_iteration = 0;
                        self.react_state = ReActState::PreparingRequest;
                        self.turn_start = Some(Instant::now());
                    }
                    Ok(false) => {
                        // commit_turn 已设置 Idle，无需 set_state
                        let old_outer = self.outer_state;
                        self.outer_state = OuterState::Idle;
                        self.emit_outer_state_change(old_outer, self.outer_state);
                        Self::invoke_on_outer_state_change(
                            &self.config.hooks,
                            old_outer,
                            self.outer_state,
                        )
                        .await;
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to dequeue pending input");
                        self.react_state = ReActState::Failed;
                    }
                }
            }

            ReActState::Failed => {
                // 纯同步 no-op。收尾交给 `run()` 循环顶部 —— 那里不在 `select!` 内，
                // 且 `run()` 的 task 从不被 abort，收尾两段（冻结 + 落盘）都能跑完。
                //
                // 在此处 await 会丢掉异步尾巴：`select!`（`biased`，用户输入优先）
                // 在用户侧任一分支胜出时都会 drop 掉本 future，而本设计里异步尾巴
                // 不只是事件 —— 还有 `snapshot` / `save`，丢了就是「冻结进了
                // committed 却没落盘」。
                return;
            }

            ReActState::AwaitingModel => {
                warn!("Unexpected AwaitingModel state in react_step");
                self.react_state = ReActState::Failed;
            }
        }

        // Emit ReactStateChange event + hook（if state changed）
        if old_react_state != self.react_state {
            self.emit_react_state_change(old_react_state, self.react_state, turn);
            Self::invoke_on_react_state_change(
                &self.config.hooks,
                turn,
                old_react_state,
                self.react_state,
            )
            .await;
        }
    }

    // ── PreparingRequest — 分叉点 ────────────────────────────────────────

    /// 准备请求并分叉到 batch 或 streaming 路径。
    async fn prepare_and_send_request(&mut self, turn: usize) {
        // 1. 检查取消
        if self.is_cancelled() {
            self.failure_reason = Some(TurnFailureReason::Cancelled);
            self.react_state = ReActState::Failed;
            return;
        }

        // 2. 检查 max_turns 限制（限制当前对话轮次内的模型调用次数）
        if self.react_loop_iteration >= self.max_turns {
            self.failure_reason = Some(TurnFailureReason::MaxTurnsExceeded);
            self.react_state = ReActState::Failed;
            return;
        }
        // ★ 递增模型调用计数
        self.react_loop_iteration += 1;

        // 3. 构建消息列表（Agent 层上下文策略，零拷贝 Arc 共享）
        let all_refs: Vec<&AnnotatedMessage> = self.session.all_message_refs().collect();

        // ★ MessageFilter: 在 build_context 之前过滤，system prompt + 动态上下文
        //    由 build_context 单独注入，不受过滤器影响。
        let owned_filtered: Vec<AnnotatedMessage>;
        let refs: Vec<&AnnotatedMessage> = if let Some(filter) = &self.config.message_filter {
            owned_filtered = filter.filter(&all_refs);
            // 图片部件可见性：过滤器输入含图时 warn 一次（含计数）—
            // 脱敏类过滤器不处理图片即原样进入模型上下文，这一事实不能静默。
            let image_count: usize = all_refs.iter().map(|am| am.message.image_count()).sum();
            if image_count > 0 {
                warn!(
                    images = image_count,
                    "MessageFilter input contains image parts; images pass through unless the filter handles them"
                );
            }
            owned_filtered.iter().collect()
        } else {
            all_refs
        };

        // ── 动态上下文解析 ─────────────────────────────────────────────
        // 若末条消息为 User query，表示新一轮对话开始，解析动态上下文；
        // 否则（tool 结果返回后的 ReAct 迭代）复用已缓存的上下文。
        let last_is_user = refs
            .last()
            .map(|am| {
                matches!(
                    am.message.as_ref(),
                    InputItem::Message {
                        role: Role::User,
                        ..
                    }
                )
            })
            .unwrap_or(false);

        if last_is_user && let Some(dc) = &self.config.dynamic_context {
            let query_text = refs
                .last()
                .and_then(|am| match am.message.as_ref() {
                    InputItem::Message {
                        role: Role::User,
                        content,
                    } => Some(content.text_view()),
                    _ => None,
                })
                .unwrap_or_default();
            self.dynamic_context = dc.query(&query_text).await;
        }

        // ── 合并 system prompt → instructions ──────────────────────────
        let effective_prompt =
            compose_effective_prompt(&self.stable_prefix, self.dynamic_context.as_deref());

        let ctx = super::context::build_context(
            &refs,
            Some(effective_prompt.as_str()),
            &self.config.context_strategy,
        );
        let mut session_messages = ctx.messages;

        // ★ Hook: on_before_request — 可修改消息或中止（在 filter 之后，看到最终消息列表）
        if let HookAction::Abort(reason) =
            Self::invoke_on_before_request(&self.config.hooks, turn, &mut session_messages).await
        {
            self.failure_reason = Some(TurnFailureReason::HookAbort(reason));
            self.react_state = ReActState::Failed;
            return;
        }

        // 4. 分支：根据 ModelConfig.stream 决定路径
        let use_stream = self.agent.model_config().stream.unwrap_or(false);

        // 请求前估算 token — 响应到达后经 [`Self::log_estimate_calibration`]
        // 与实际 usage 对照。
        let estimated_request_tokens = estimate_str_tokens(&effective_prompt)
            + session_messages
                .iter()
                .map(|m| estimate_item_tokens(m))
                .sum::<usize>();
        self.last_request_estimated_tokens = Some(estimated_request_tokens);
        debug!(turn, estimated_request_tokens, "Request token estimate");

        if use_stream {
            // ── Streaming 路径 ──
            match self
                .agent
                .generate_stream(session_messages, Some(effective_prompt))
                .await
            {
                Ok(stream) => {
                    self.active_stream = Some(stream);
                    self.stream_assembler = BlockAssembler::new();
                    self.last_finish_reason = None;
                    self.react_state = ReActState::Streaming;
                }
                Err(e) => {
                    error!(error = %e, "Streaming generate request failed");
                    self.failure_reason = Some(TurnFailureReason::Other(format!(
                        "Streaming request failed: {e}"
                    )));
                    self.react_state = ReActState::Failed;
                }
            }
        } else {
            // ── Batch 路径 ──
            let tools = self.agent.tool_executor().definitions();
            match self
                .agent
                .generate_with_tools(session_messages, Some(effective_prompt), tools)
                .await
            {
                Ok(response) => {
                    self.react_ctx.batch_response = Some(response);
                    self.react_state = ReActState::ResolvingResponse;
                }
                Err(e) => {
                    error!(error = %e, "Batch generate request failed");
                    self.failure_reason = Some(TurnFailureReason::Other(format!(
                        "Generate request failed: {e}"
                    )));
                    self.react_state = ReActState::Failed;
                }
            }
        }
    }

    // ── Batch 分支：ResolvingResponse ─────────────────────────────────────

    /// 解析 batch 响应：提取内容、更新 usage、写入 staging、判断下一步。
    async fn resolve_batch_response(&mut self, turn: usize) {
        let response = match self.react_ctx.batch_response.take() {
            Some(r) => r,
            None => {
                error!("No batch_response in ResolvingResponse state");
                self.react_state = ReActState::Failed;
                return;
            }
        };

        // ★ Hook: on_after_response
        if let HookAction::Abort(reason) =
            Self::invoke_on_after_response(&self.config.hooks, turn, &response).await
        {
            self.failure_reason = Some(TurnFailureReason::HookAbort(reason));
            self.react_state = ReActState::Failed;
            return;
        }

        // 聚合 usage
        self.session.add_usage(response.usage.clone());
        self.log_estimate_calibration(turn, response.usage.input_tokens);

        // 广播 usage 事件
        self.emit_event(LooperEvent::ModelUsage {
            call_index: turn,
            usage: response.usage.clone(),
        });

        // 写入 assistant 内容到 session staging（分块回填 InputItem，
        // 同时更新 assistant_text / assistant_reasoning）。
        self.stage_output_blocks(&response.output);

        // 状态收敛：Failed 与 Incomplete 都视为异常终止（partial_text 已回填）
        if response.status != ResponseStatus::Completed {
            let msg = response
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| match response.status {
                    ResponseStatus::Incomplete => {
                        "model response was truncated (incomplete)".to_string()
                    }
                    _ => "model response failed".to_string(),
                });
            error!(
                turn,
                status = ?response.status,
                message = %msg,
                "Batch model response ended with non-completed status"
            );
            self.failure_reason = Some(TurnFailureReason::Other(msg));
            self.react_state = ReActState::Failed;
            return;
        }

        // 提取 tool calls（转 ToolCall）用于决定下一步
        let tool_calls: Vec<ToolCall> = response
            .output
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolCall {
                    call_id,
                    name,
                    arguments,
                } => Some(ToolCall::new(
                    call_id.clone(),
                    name.clone(),
                    arguments.clone(),
                )),
                _ => None,
            })
            .collect();

        // 判断下一步
        if !tool_calls.is_empty() {
            self.react_ctx.pending_tool_calls = tool_calls
                .into_iter()
                .map(|tc| PendingToolCall {
                    call: Arc::new(tc),
                    result: None,
                })
                .collect();
            self.react_state = ReActState::ExecutingTools;
        } else {
            // batch 路径不发送 TextDelta（整段文本不是增量），
            // 统一由 Done 状态中的 TurnComplete 事件提供本轮最终文本。
            self.react_state = ReActState::Done;
        }
    }

    // ── Streaming 分支：Streaming ─────────────────────────────────────────

    /// 消费一个 stream chunk，处理后在同一个状态内循环直到流结束。
    ///
    /// 每收到一个 chunk 就返回（让 select! 有机会处理用户输入 / 取消）。
    async fn consume_stream_chunk(&mut self, turn: usize) {
        let stream = match &mut self.active_stream {
            Some(s) => s,
            None => {
                error!("No active stream in Streaming state");
                self.react_state = ReActState::Failed;
                return;
            }
        };

        match stream.next_chunk().await {
            Some(Ok(chunk)) => {
                // 双轨：delta 原样转发前端，BlockEnd 交由 assembler 折叠。
                self.stream_assembler.push(chunk.clone());

                match chunk {
                    StreamChunk::BlockStart { .. } => {}

                    StreamChunk::TextDelta { delta, .. } => {
                        // ★ Hook: on_text_delta — 可中止流式响应
                        if let HookAction::Abort(reason) = Self::invoke_on_text_delta(
                            &self.config.hooks,
                            turn,
                            &delta,
                            &self.react_ctx.assistant_text,
                        )
                        .await
                        {
                            self.failure_reason = Some(TurnFailureReason::HookAbort(reason));
                            self.react_state = ReActState::Failed;
                            self.active_stream = None;
                            return;
                        }

                        self.emit_event(LooperEvent::TextDelta {
                            delta: delta.clone(),
                        });
                        self.react_ctx.assistant_text.push_str(&delta);
                    }

                    StreamChunk::ReasoningDelta { delta, .. } => {
                        self.emit_event(LooperEvent::ReasoningDelta {
                            delta: delta.clone(),
                        });
                        self.react_ctx.assistant_reasoning.push_str(&delta);
                    }

                    StreamChunk::ToolCallDelta {
                        call_id,
                        name,
                        arguments,
                        ..
                    } => {
                        // Normalize arguments to String at the boundary
                        let args_str = match &arguments {
                            serde_json::Value::String(s) => s.clone(),
                            other => serde_json::to_string(other).unwrap_or_default(),
                        };
                        self.emit_event(LooperEvent::ToolCallDelta {
                            id: call_id,
                            name,
                            arguments: args_str,
                        });
                    }

                    StreamChunk::BlockEnd { .. } => {
                        // 块已由 assembler 折叠；ToolCallStart 统一在 execute_tools_step
                        // 的 spawn 阶段发出，避免流式路径下重复发送 ToolCallStart。
                    }

                    StreamChunk::Usage { usage } => {
                        self.emit_event(LooperEvent::ModelUsage {
                            call_index: turn,
                            usage: usage.clone(),
                        });
                        self.log_estimate_calibration(turn, usage.input_tokens);
                        self.session.add_usage(usage);
                    }

                    StreamChunk::Finish { reason } => {
                        self.last_finish_reason = Some(reason);
                        self.finish_stream().await;
                    }
                }
            }

            Some(Err(e)) => {
                error!(error = %e, "Stream error");
                self.failure_reason = Some(TurnFailureReason::Other(format!("Stream error: {e}")));
                self.active_stream = None;
                self.react_state = ReActState::Failed;
            }

            None => {
                // Stream ended without Finish event — 收敛 assembler，按成功处理。
                // 这里没有 Finish 可依，收敛结果完全取决于块是否闭合，属于异常路径，
                // 必须留下痕迹：否则「流被上游掐断」与「正常结束」在日志里长得一样。
                debug!(
                    turn,
                    "Model stream ended without a Finish chunk; converging on assembler state"
                );
                self.finish_stream().await;
            }
        }
    }

    /// 流结束（收到 `Finish` 或流自然关闭）后的收尾。
    ///
    /// 从 [`BlockAssembler`] 收敛有序块，回填 `InputItem` 到 session staging，
    /// 并决定 `Done` / `ExecutingTools` / `Failed`。
    async fn finish_stream(&mut self) {
        self.active_stream = None;
        let assembler = std::mem::take(&mut self.stream_assembler);
        let (blocks, usage, status, error) = assembler.finish();
        let finish_reason = self.last_finish_reason.take();

        // 回填 InputItem 到 session staging，并更新 assistant_text / assistant_reasoning
        // （失败分支也先回填，使 Failed 结果携带 partial_text）。
        self.stage_output_blocks(&blocks);

        // 状态收敛：Failed（Aborted / Error）与 Incomplete（截断 / 过滤）都视为异常终止
        if status != ResponseStatus::Completed {
            let msg = error.map(|e| e.message).unwrap_or_else(|| match status {
                ResponseStatus::Incomplete => {
                    "model response was truncated (incomplete)".to_string()
                }
                _ => "model response failed".to_string(),
            });
            // 归因字段必须齐全：status 只说「非正常结束」，finish_reason 才区分
            // 截断（max_tokens）与上游异常（aborted/error）；output_tokens 用来判断
            // 截断是否真的顶到了输出上限；block_kinds 说明收到的到底是哪些块。
            error!(
                turn = self.session.turn_index(),
                ?status,
                finish_reason = finish_reason.map_or("-", |r| r.as_str()),
                message = %msg,
                blocks = blocks.len(),
                block_kinds = ?block_kinds_of(&blocks),
                text_len = self.react_ctx.assistant_text.len(),
                reasoning_len = self.react_ctx.assistant_reasoning.len(),
                input_tokens = usage.input_tokens,
                output_tokens = usage.output_tokens,
                "Model stream ended with non-completed status"
            );
            self.failure_reason = Some(TurnFailureReason::Other(msg));
            self.react_state = ReActState::Failed;
            return;
        }

        // 提取 tool calls（转 ToolCall）
        let tool_calls: Vec<ToolCall> = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolCall {
                    call_id,
                    name,
                    arguments,
                } => Some(ToolCall::new(
                    call_id.clone(),
                    name.clone(),
                    arguments.clone(),
                )),
                _ => None,
            })
            .collect();

        if tool_calls.is_empty() {
            self.react_state = ReActState::Done;
        } else {
            self.react_ctx.pending_tool_calls = tool_calls
                .into_iter()
                .map(|tc| PendingToolCall {
                    call: Arc::new(tc),
                    result: None,
                })
                .collect();
            self.react_state = ReActState::ExecutingTools;
        }
    }

    /// 将有序 output 块回填到 session staging：`Text→Message{Assistant}`、
    /// `Reasoning→Reasoning`、`ToolCall→FunctionCall`；并更新本轮
    /// `assistant_text` / `assistant_reasoning`。
    fn stage_output_blocks(&mut self, blocks: &[ContentBlock]) {
        let mut text = String::new();
        let mut reasoning = String::new();
        for block in blocks {
            match block {
                ContentBlock::Text { text: t } => {
                    text.push_str(t);
                    let _ = self.session.stage_item(
                        MessageSource::ModelGeneration,
                        InputItem::Message {
                            role: Role::Assistant,
                            content: t.clone().into(),
                        },
                    );
                }
                ContentBlock::Reasoning { text: t } => {
                    reasoning.push_str(t);
                    let _ = self.session.stage_item(
                        MessageSource::ModelGeneration,
                        InputItem::Reasoning { content: t.clone() },
                    );
                }
                ContentBlock::ToolCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    let _ = self.session.stage_item(
                        MessageSource::ModelGeneration,
                        InputItem::FunctionCall {
                            call_id: call_id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
                    );
                }
                _ => {}
            }
        }
        // 流式路径下 assistant_text / assistant_reasoning 已由 delta 累积；
        // 仅在传入文本非空（或当前为空）时覆盖，避免截断（Incomplete）时把已累积的
        // partial text 抹掉。
        if !text.is_empty() || self.react_ctx.assistant_text.is_empty() {
            self.react_ctx.assistant_text = text;
        }
        if !reasoning.is_empty() || self.react_ctx.assistant_reasoning.is_empty() {
            self.react_ctx.assistant_reasoning = reasoning;
        }
    }

    // ── 共享：ExecutingTools ──────────────────────────────────────────────

    /// Tool 执行增量步进：spawn（首次进入）→ 轮询（后续进入）→ 完成。
    ///
    /// 替代原来阻塞的 `join_all`，利用 `JoinSet` 将执行拆分为多个
    /// `react_step()` 调用。每次调用要么收集一个已完成 tool 的结果，要么在
    /// 短超时（200ms）后返回，让 `run()` 的 `select!` 循环有机会检查取消标志
    /// 和处理用户输入。
    ///
    /// 支持断点续执行：`result: Some(...)` 的已完成项自动跳过。
    async fn execute_tools_step(&mut self, turn: usize) {
        // ── Spawn 阶段：active_tool_tasks 为 None ───────────────────────
        if self.active_tool_tasks.is_none() {
            // 进入 spawn 前检查取消
            if self.is_cancelled() {
                self.failure_reason = Some(TurnFailureReason::Cancelled);
                self.react_state = ReActState::Failed;
                return;
            }

            let executor = self.agent.mcp_manager().tools_executor().clone();

            // 收集所有尚未执行的 tool calls（result == None）
            let pending_indices: Vec<usize> = self
                .react_ctx
                .pending_tool_calls
                .iter()
                .enumerate()
                .filter(|(_, ptc)| ptc.result.is_none())
                .map(|(i, _)| i)
                .collect();

            if pending_indices.is_empty() {
                // 全部已完成（从断点恢复后的情况），直接进入收尾
                self.finalize_tool_execution().await;
                return;
            }

            // ★ Hook: on_before_tool — 可 Override / Reject / Abort
            let mut to_spawn: Vec<usize> = Vec::new();
            for &idx in &pending_indices {
                // Clone call data upfront to avoid borrow conflicts with pending_tool_calls
                let call = self.react_ctx.pending_tool_calls[idx].call.clone();

                // 发送 ToolCallStart 事件
                self.emit_event(LooperEvent::ToolCallStart {
                    id: call.id.clone(),
                    name: call.function.name.clone(),
                    arguments: call.function.arguments.clone(),
                });

                match Self::invoke_on_before_tool(&self.config.hooks, turn, &call).await {
                    ToolHookAction::Continue => {
                        to_spawn.push(idx);
                    }
                    ToolHookAction::Override(result) => {
                        self.react_ctx.pending_tool_calls[idx].result = Some(ToolCallResult {
                            call: call.clone(),
                            result: Content::Text(result.clone()),
                            is_error: false,
                        });
                        self.emit_event(LooperEvent::ToolResult {
                            id: call.id.clone(),
                            name: call.function.name.clone(),
                            result,
                            images: Vec::new(),
                        });
                    }
                    ToolHookAction::Reject(reason) => {
                        self.react_ctx.pending_tool_calls[idx].result = Some(ToolCallResult {
                            call: call.clone(),
                            result: Content::Text(reason.clone()),
                            is_error: true,
                        });
                        self.emit_event(LooperEvent::ToolResult {
                            id: call.id.clone(),
                            name: call.function.name.clone(),
                            result: reason,
                            images: Vec::new(),
                        });
                    }
                    ToolHookAction::Abort(reason) => {
                        self.failure_reason = Some(TurnFailureReason::HookAbort(reason));
                        self.react_state = ReActState::Failed;
                        return;
                    }
                }
            }

            // 如果所有 tool 已被 hook 处理（Override/Reject），直接收尾
            if to_spawn.is_empty() {
                self.finalize_tool_execution().await;
                return;
            }

            // 将需要执行的 tool 作为 tokio task 生成到 JoinSet 中
            let mut joinset = tokio::task::JoinSet::new();
            for &idx in &to_spawn {
                let call = self.react_ctx.pending_tool_calls[idx].call.clone();
                let executor = executor.clone();
                joinset.spawn(async move {
                    let result = match executor
                        .execute(&call.function.name, &call.function.arguments)
                        .await
                    {
                        Ok(r) => ToolCallResult {
                            call: call.clone(),
                            result: r,
                            is_error: false,
                        },
                        // 错误路径保持纯文本
                        Err(e) => ToolCallResult {
                            call: call.clone(),
                            result: Content::Text(e),
                            is_error: true,
                        },
                    };
                    (idx, result)
                });
            }

            self.active_tool_tasks = Some(joinset);
            // 立即返回 — 下一次 react_step 调用将进入 poll 阶段
            return;
        }

        // ── Poll 阶段：active_tool_tasks 为 Some ────────────────────────
        // 每次轮询前检查取消
        if self.is_cancelled() {
            // 先捞回已完成的 task（try_join_next 非阻塞），再 abort 在途的 ——
            // 这是本设计里唯一「连内存都留不住」的窗口。
            if let Some(mut joinset) = self.active_tool_tasks.take() {
                let drained = Self::drain_finished_join_results(
                    &mut joinset,
                    &mut self.react_ctx.pending_tool_calls,
                );
                if drained > 0 {
                    debug!(drained, "Salvaged completed tool results before abort");
                }
                joinset.abort_all();
            }
            self.failure_reason = Some(TurnFailureReason::Cancelled);
            self.react_state = ReActState::Failed;
            return;
        }

        // 以短超时轮询下一个完成的 task
        let poll_result = tokio::time::timeout(
            Duration::from_millis(200),
            self.active_tool_tasks
                .as_mut()
                .expect("active_tool_tasks must be Some in poll phase")
                .join_next(),
        )
        .await;

        match poll_result {
            // 收集到一个完成的 tool 结果
            Ok(Some(Ok((idx, tool_result)))) => {
                // 处理首个结果
                self.react_ctx.pending_tool_calls[idx].result = Some(tool_result.clone());

                self.emit_event(LooperEvent::ToolResult {
                    id: tool_result.call.id.clone(),
                    name: tool_result.call.function.name.clone(),
                    result: tool_result.result.text_view().into_owned(),
                    images: tool_result
                        .result
                        .image_urls()
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                });

                Self::invoke_on_after_tool(
                    &self.config.hooks,
                    turn,
                    &tool_result.call,
                    &tool_result.result.text_view(),
                    tool_result.is_error,
                )
                .await;

                // ★ 贪婪 drain 所有立即可用的已完成 task（非阻塞）
                loop {
                    let next = self
                        .active_tool_tasks
                        .as_mut()
                        .expect("active_tool_tasks must be Some in poll phase")
                        .try_join_next();
                    match next {
                        Some(Ok((idx, tr))) => {
                            self.react_ctx.pending_tool_calls[idx].result = Some(tr.clone());

                            self.emit_event(LooperEvent::ToolResult {
                                id: tr.call.id.clone(),
                                name: tr.call.function.name.clone(),
                                result: tr.result.text_view().into_owned(),
                                images: tr
                                    .result
                                    .image_urls()
                                    .into_iter()
                                    .map(str::to_string)
                                    .collect(),
                            });

                            Self::invoke_on_after_tool(
                                &self.config.hooks,
                                turn,
                                &tr.call,
                                &tr.result.text_view(),
                                tr.is_error,
                            )
                            .await;
                        }
                        Some(Err(join_error)) => {
                            error!(
                                error = %join_error,
                                "Tool execution task panicked; continuing with remaining tools"
                            );
                        }
                        None => break,
                    }
                }

                // 检查是否所有 tool 已全部完成
                let all_done = self
                    .active_tool_tasks
                    .as_ref()
                    .is_none_or(|js| js.is_empty());
                if all_done {
                    self.active_tool_tasks = None;
                    self.finalize_tool_execution().await;
                }
            }

            // Task panic
            Ok(Some(Err(join_error))) => {
                error!(
                    error = %join_error,
                    "Tool execution task panicked; continuing with remaining tools"
                );
            }

            // 超时 — 返回 select! 循环以检查取消和用户输入
            Err(_elapsed) => {}

            // 所有 task 已全部完成
            Ok(None) => {
                self.active_tool_tasks = None;
                self.finalize_tool_execution().await;
            }
        }
    }

    /// 把 `pending_tool_calls` 中 `result: Some` 的项写入 staging。**只写不清理**。
    ///
    /// 写入与清理彻底分开：`finalize_tool_execution` 写完还要清 looper 状态，
    /// 而失败收尾（[`plan_failure`]，走同一个自由函数 `stage_tool_results`）
    /// 一个字段都不该清 —— 它还要从 `assistant_text` 取 partial_text。
    /// 用一个 `clear: bool` 表达不了这组差异，硬塞会变成三个 bool，且调用点写错
    /// 就会静默清空状态。
    fn stage_completed_tool_results(&mut self) {
        stage_tool_results(&mut self.session, &self.react_ctx.pending_tool_calls);
    }

    /// 落一次在途轮检查点（staging 全量）。
    ///
    /// 取 `&mut self` 而非 `&self`：两个方法都要跨越 `await`，而共享借用
    /// 跨越 await 要求 `AgentLooper: Sync` —— 它持有非 Sync 的流。
    async fn persist_inflight_checkpoint(&mut self) {
        let checkpoint = InflightCheckpoint {
            session_id: self.session.id().to_string(),
            turn_index: self.session.turn_index(),
            reason: INFLIGHT_CRASH_REASON.to_string(),
            staged: self.session.staging_all(),
        };
        if let Err(e) = self.persister.save_inflight(&checkpoint).await {
            warn!(error = %e, "Failed to persist inflight turn checkpoint");
        }
    }

    /// 清除在途轮检查点（turn 已收尾，检查点不再有意义）。
    ///
    /// 幂等且不致命：残留行会被水化侧的轮次编号守卫判为陈旧后丢弃。
    async fn clear_inflight_checkpoint(&mut self) {
        if let Err(e) = self.persister.delete_inflight(self.session.id()).await {
            warn!(error = %e, "Failed to clear inflight turn checkpoint");
        }
    }

    /// 所有 tool 执行完毕后的收尾：批量写入 staging，切换状态。
    async fn finalize_tool_execution(&mut self) {
        self.stage_completed_tool_results();

        // ★ 在途轮检查点：一批工具刚落地的时刻 —— 副作用已经发生，
        //   此刻进程若在后续模型调用中崩溃，这批结果靠它进历史。
        //   写入失败只 warn：检查点是尽力而为，不该影响正常路径。
        self.persist_inflight_checkpoint().await;

        // 清理本轮 tool 数据，进入下一轮
        self.react_ctx.pending_tool_calls.clear();
        self.react_ctx.assistant_text.clear();
        self.react_ctx.assistant_reasoning.clear();
        self.react_ctx.batch_response = None;
        self.react_state = ReActState::PreparingRequest;
    }

    // ── 构建最终响应 ──────────────────────────────────────────────────────

    /// 从当前状态构建 `ModelResponse`（sync，参数由调用方预先收集）。
    ///
    /// 注意：`output` 和 `messages` 不再包含在 ModelResponse 中。
    /// 最终纯文本通过 [`LooperEvent::TurnComplete`] 的 `text` 字段获取，
    /// 消息历史通过 `Session` 获取。
    fn build_model_response(&self, usage: Usage, turns: usize) -> ModelResponse {
        ModelResponse { usage, turns }
    }

    // ── 失败收尾 ──────────────────────────────────────────────────────────

    /// 统一的 turn 失败收尾：冻结在途轮 → 落盘 → 发事件 → 续接 pending。
    ///
    /// 返回 `true` 表示续接了排队输入（调用方应 `continue`）。
    async fn finalize_failure(&mut self, reason: TurnFailureReason, drain_pending: bool) -> bool {
        match plan_failure(
            &mut self.session,
            &mut self.react_ctx,
            self.config.persist_on_failure,
            reason,
            drain_pending,
        ) {
            Some(plan) => self.finalize_failure_async(plan).await,
            None => false,
        }
    }

    /// 消费 [`FinalizePlan`] 的异步段：事件 → hook → 落盘 → 回写原因 → 续接。
    ///
    /// 变更（冻结 + 快照）已在 [`plan_failure`] 内同步完成，此处只做 I/O 与编排。
    async fn finalize_failure_async(&mut self, plan: FinalizePlan) -> bool {
        warn!(
            turn = plan.turn,
            reason = ?plan.reason,
            discarded_staging_messages = plan.discarded_staging_messages,
            partial_text_len = plan.partial_text_len,
            "Turn interrupted; staging frozen into committed history"
        );

        // 事件照发 —— 冻结失败只跳过落盘，不跳过 TurnComplete。
        // 「这轮失败了」这个消息不能让用户看不到。
        let usage = self.session.total_usage();
        Self::emit_event_guaranteed(
            &self.event_speaker,
            LooperEvent::TurnComplete {
                turn_index: plan.turn,
                outcome: plan.outcome.clone(),
                usage: usage.clone(),
            },
        )
        .await;
        Self::invoke_on_turn_complete(
            &self.config.hooks,
            plan.turn,
            plan.outcome.failure_reason(),
            &usage,
            &self.session,
        )
        .await;

        // 落盘门控：冻结无条件发生（内存），持久化由 persist_on_failure 决定
        if let Some(snapshot) = plan.snapshot
            && let Err(e) = self
                .persister
                .save(
                    &snapshot,
                    self.session.id(),
                    self.session.description(),
                    self.session.created_at(),
                )
                .await
        {
            error!(error = %e, "Failed to persist session after turn failure");
        }
        // ★ 冻结已落进 committed，在途检查点作废（与落盘门控无关：
        //   门控关掉的是快照，检查点留着会在下次冷启动被判为陈旧）。
        self.clear_inflight_checkpoint().await;

        // ★ 回写失败原因：`take()` 之后 run() 末尾的 shutdown_reason 会退化成
        //   "done"，Shutdown 事件丢失失败原因。这次回写是刻意的，不是冗余赋值。
        self.failure_reason = Some(plan.reason.clone());

        if plan.drain_pending {
            match self.session.dequeue_and_start_turn() {
                Ok(true) => {
                    // ★ 新对话轮次：重置 ReAct 循环计数
                    self.react_loop_iteration = 0;
                    self.react_state = ReActState::PreparingRequest;
                    self.turn_start = Some(Instant::now());
                    // ★ 上一轮的失败原因不得跨轮存活：新一轮若不经过
                    //   `handle_user_query`（那条路径会清），会在正常跑到 `Done` 时
                    //   撞上 `debug_assert!(failure_reason.is_none())`。
                    self.failure_reason = None;
                    return true;
                }
                Ok(false) => {}
                Err(e) => {
                    error!(error = %e, "Failed to dequeue pending input");
                }
            }
        }

        // interrupt_turn / rollback_turn 已把 session 置回 Idle。
        // 内层也回到 `Done`（无在途工作）—— 留着 `Failed` 会让循环顶部反复命中。
        self.react_state = ReActState::Done;
        let old_outer = self.outer_state;
        self.outer_state = OuterState::Idle;
        self.emit_outer_state_change(old_outer, self.outer_state);
        Self::invoke_on_outer_state_change(&self.config.hooks, old_outer, self.outer_state).await;
        false
    }
}

// ============================================================================
// 失败收尾（自由函数 — 可用真实 Session 直接单测，无需 Agent / provider）
// ============================================================================

/// 失败收尾的同步产物。
#[derive(Debug)]
struct FinalizePlan {
    /// 冻结前的 turn 编号。必须在 `interrupt_turn` **之前**取 ——
    /// 之后取则 `TurnComplete` 报的编号比 committed 轮数大 1，前端与历史对不上。
    turn: usize,
    outcome: TurnOutcome,
    /// 供 `Shutdown` 回写。
    reason: TurnFailureReason,
    /// 冻结前的部分文本长度。**必须在这里存一份** —— `partial_text` 已经
    /// 移进 `outcome`，而 `TurnOutcome::text()` 对 `Failed` 恒返回 `None`，
    /// 从 outcome 反查长度会恒得 0。与冻结前的 `discarded_staging_messages` 并列，
    /// 让日志能回答「这轮白跑了多少」。
    partial_text_len: usize,
    /// 落盘产物，`persist_on_failure` 门控。
    /// `false` 时冻结照常（内存）但不产出快照（CLI 路径，`NullSessionPersister`）。
    snapshot: Option<SessionSnapshot>,
    /// 冻结前 staging 中的消息数（合成 Output 与中断说明尚未追加）。
    discarded_staging_messages: usize,
    /// 是否续接排队输入。由 `!is_cancelled()` 单点推导 —— 用户按了停就不自动跑下一条。
    drain_pending: bool,
}

/// 把 `pending_tool_calls` 中 `result: Some` 的项写入 session 的 staging。只写不清理。
fn stage_tool_results(session: &mut Session, pending: &[PendingToolCall]) {
    for ptc in pending {
        if let Some(ref result) = ptc.result {
            let _ = session.stage_item(
                MessageSource::ToolExecution {
                    tool_name: ptc.call.function.name.clone(),
                },
                InputItem::FunctionCallOutput {
                    call_id: ptc.call.id.clone(),
                    output: result.result.clone(),
                },
            );
        }
    }
}

/// 失败原因 → 写进 committed 历史的人类可读文案。
///
/// **不可泄漏 `Debug` 格式**：产物会进入模型上下文，`HookAbort("x")` 原样进去
/// 就是 `HookAbort("x")` 而非 `x`。
fn failure_label(reason: &TurnFailureReason) -> String {
    match reason {
        TurnFailureReason::Cancelled => "cancelled by user".to_string(),
        TurnFailureReason::TotalTimeout => "total run timeout".to_string(),
        TurnFailureReason::PerTurnTimeout => "per-turn timeout".to_string(),
        TurnFailureReason::MaxTurnsExceeded => "max turns exceeded".to_string(),
        TurnFailureReason::HookAbort(detail) => format!("aborted by hook: {detail}"),
        TurnFailureReason::Other(detail) => format!("failed: {detail}"),
    }
}

/// 无 await，全部 session 变更在此完成。
///
/// **不变量：本函数全程禁止 `.await`。** 正因为是纯同步，「冻结」对取消免疫 ——
/// 若 `run()` 被 abort，最多丢事件与落盘，绝不会出现「落了盘但 committed 里
/// 没有这一轮」。严防后人往这段塞 `.await`。
///
/// 步骤顺序：① 守卫 → ② 取标量 → ③ 冲刷已完成的工具结果 → ④ 冻结
/// → ⑤ 快照 → ⑥ 清状态。③ 在 ④ 之前是硬要求：捞回来的工具结果必须和
/// 这一轮一起进 committed，且 `stage_item` 只在 `Active` 下可用（④ 之后就 `Idle` 了）。
///
/// 返回 `None` 表示无在途轮可收尾 —— 挡的是「取消发生在 `Idle`」（根本没有在途轮，
/// 例如 looper 正空转等输入时用户点了停止）时发出语义为空的 `TurnComplete`。
fn plan_failure(
    session: &mut Session,
    ctx: &mut ReActContext,
    persist: bool,
    reason: TurnFailureReason,
    drain_pending: bool,
) -> Option<FinalizePlan> {
    // ① 无在途轮守卫
    if session.state() == SessionState::Idle && session.staging_messages().is_empty() {
        return None;
    }

    // ② 取标量 —— 必须在所有变更之前
    let turn = session.turn_index(); // interrupt_turn 后会 +1
    let partial_text = std::mem::take(&mut ctx.assistant_text);
    let partial_text_len = partial_text.len();
    let discarded_staging_messages = session.staging_messages().len(); // ③ 冲刷后会变

    // ③ 冲刷 poll 阶段已捞回的工具结果（只写不清理；见 `stage_tool_results`）。
    //    必须在 ④ 之前 —— 这些结果是「这一轮已完成的成果」，要和本轮一起冻结。
    stage_tool_results(session, &ctx.pending_tool_calls);

    // ④ 冻结在途轮（内部补齐悬空 tool_call 并追加中断说明）
    let label = failure_label(&reason);
    let token = match session.interrupt_turn(&label) {
        Ok(token) => Some(token),
        Err(e) => {
            error!(error = %e, "Failed to interrupt turn; falling back to rollback");
            // rollback 无 state 守卫，必回 Idle
            if let Err(e) = session.rollback_turn(false) {
                error!(error = %e, "Failed to rollback turn");
            }
            None
        }
    };

    // ⑤ token 之后才 snapshot —— 顺序不可颠倒，否则快照里没有这一轮
    let snapshot = match (&token, persist) {
        (Some(t), true) => Some(session.snapshot(t)),
        _ => None,
    };

    // ⑥ 清 ctx —— pending_tool_calls 必须清，否则 drain_pending 时下一轮的
    //    execute_tools_step 会把上一轮取消掉的 tool 重跑一遍（to_spawn 只 filter
    //    result.is_none()），正是本设计明确禁止的重放副作用。
    ctx.pending_tool_calls.clear();
    ctx.assistant_text.clear();
    ctx.assistant_reasoning.clear();
    ctx.batch_response = None;

    Some(FinalizePlan {
        turn,
        outcome: TurnOutcome::Failed {
            reason: reason.clone(),
            partial_text,
        },
        reason,
        partial_text_len,
        snapshot,
        discarded_staging_messages,
        drain_pending,
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    // ── LooperConfig tests ────────────────────────────────────────────

    #[test]
    fn test_looper_config_default() {
        let config = LooperConfig::default();
        assert_eq!(config.event_buffer, 256);
        assert_eq!(config.per_turn_timeout, Some(Duration::from_secs(180)));
        assert!(config.total_timeout.is_none());
        assert!(config.hooks.is_empty());
        assert!(config.environment.is_none());
    }

    // ── prompt 组装纯函数 tests ────────────────────────────────────────

    #[test]
    fn test_compose_stable_prefix_none() {
        assert_eq!(compose_stable_prefix("SYS", None), "SYS");
    }

    #[test]
    fn test_compose_stable_prefix_some() {
        assert_eq!(
            compose_stable_prefix("SYS", Some("<environment>x</environment>")),
            "SYS\n\n<environment>x</environment>"
        );
    }

    #[test]
    fn test_compose_stable_prefix_empty_env() {
        // 空串必须等同 None——否则追加尾随 "\n\n" 破坏字节一致
        assert_eq!(compose_stable_prefix("SYS", Some("")), "SYS");
    }

    #[test]
    fn test_compose_effective_prompt_none() {
        assert_eq!(compose_effective_prompt("PREFIX", None), "PREFIX");
    }

    #[test]
    fn test_compose_effective_prompt_some() {
        assert_eq!(
            compose_effective_prompt("PREFIX", Some("ctx")),
            "PREFIX\n\n[Dynamic Context]\nctx"
        );
    }

    #[test]
    fn test_compose_roundtrip_compat() {
        // 与旧行为字节等价的回归锚：无 environment、无 dynamic context 时
        // effective prompt == agent.system_prompt()
        let stable = compose_stable_prefix("SYS", None);
        assert_eq!(compose_effective_prompt(&stable, None), "SYS");
    }

    // ── ReActContext tests ─────────────────────────────────────────────

    #[test]
    fn test_react_context_default() {
        let ctx = ReActContext::default();
        assert!(ctx.batch_response.is_none());
        assert!(ctx.pending_tool_calls.is_empty());
        assert!(ctx.assistant_text.is_empty());
        assert!(ctx.assistant_reasoning.is_empty());
    }

    // ── PendingToolCall tests ──────────────────────────────────────────

    #[test]
    fn test_pending_tool_call_new() {
        let tc = Arc::new(ToolCall::new("id1", "test_tool", "{}"));
        let ptc = PendingToolCall {
            call: Arc::clone(&tc),
            result: None,
        };
        assert_eq!(ptc.call.id, "id1");
        assert!(ptc.result.is_none());
    }

    #[test]
    fn test_pending_tool_call_with_result() {
        let tc = Arc::new(ToolCall::new("id1", "test_tool", "{}"));
        let result = ToolCallResult {
            call: Arc::clone(&tc),
            result: Content::Text("output".to_string()),
            is_error: false,
        };
        let ptc = PendingToolCall {
            call: tc,
            result: Some(result),
        };
        assert!(ptc.result.is_some());
        assert!(!ptc.result.unwrap().is_error);
    }

    // ── drain_finished_join_results tests ──────────────────────────────

    /// 构造 `pending` 槽位（`call` 与 `result` 均占位，索引与 spawn 时的 idx 对齐）。
    fn pending_slots(n: usize) -> Vec<PendingToolCall> {
        (0..n)
            .map(|i| PendingToolCall {
                call: Arc::new(ToolCall::new(format!("c{i}"), "t", "{}")),
                result: None,
            })
            .collect()
    }

    fn tool_result(idx: usize) -> (usize, ToolCallResult) {
        let call = Arc::new(ToolCall::new(format!("c{idx}"), "t", "{}"));
        (
            idx,
            ToolCallResult {
                call,
                result: Content::Text(format!("r{idx}")),
                is_error: false,
            },
        )
    }

    #[tokio::test]
    async fn test_drain_collects_all_finished_tasks() {
        let mut joinset: tokio::task::JoinSet<(usize, ToolCallResult)> =
            tokio::task::JoinSet::new();
        joinset.spawn(async { tool_result(0) });
        joinset.spawn(async { tool_result(1) });
        // 永不完成 —— 必须不被伪造结果
        joinset.spawn(async {
            std::future::pending::<()>().await;
            tool_result(2)
        });

        tokio::time::sleep(Duration::from_millis(30)).await;

        let mut pending = pending_slots(3);
        let n = AgentLooper::drain_finished_join_results(&mut joinset, &mut pending);

        assert_eq!(n, 2);
        assert!(pending[0].result.is_some());
        assert!(pending[1].result.is_some());
        assert!(pending[2].result.is_none(), "在途 task 不得被伪造结果");
    }

    #[tokio::test]
    async fn test_drain_survives_panicking_sibling() {
        // 回归锚：`while let Some(Ok(..))` 写法会让 panic 的 task 提前终止排空，
        // 排在它后面的正常 task 成果被吞掉。
        let mut joinset: tokio::task::JoinSet<(usize, ToolCallResult)> =
            tokio::task::JoinSet::new();
        joinset.spawn(async { panic!("boom") });
        joinset.spawn(async { tool_result(1) });

        tokio::time::sleep(Duration::from_millis(30)).await;

        let mut pending = pending_slots(2);
        let n = AgentLooper::drain_finished_join_results(&mut joinset, &mut pending);

        assert_eq!(n, 1, "panic 的兄弟 task 不得吞掉正常 task 的成果");
        assert!(pending[0].result.is_none(), "panic 的 task 不应有结果");
        assert!(pending[1].result.is_some(), "正常 task 的成果必须被捞回");
    }

    // ── plan_failure tests（零脚手架：真实 Session，无 Agent / provider）──

    fn plan_session() -> Session {
        Session::new("plan-test".to_string(), "d".to_string())
    }

    fn plan_ctx_with_text(text: &str) -> ReActContext {
        ReActContext {
            assistant_text: text.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_plan_failure_freezes_dangling_tool_call() {
        let mut session = plan_session();
        session.start_turn("do it".into()).unwrap();
        session
            .stage_item(
                MessageSource::ModelGeneration,
                InputItem::FunctionCall {
                    call_id: "c1".to_string(),
                    name: "t1".to_string(),
                    arguments: "{}".to_string(),
                },
            )
            .unwrap();

        let mut ctx = plan_ctx_with_text("partial");
        let turn_before = session.turn_index();

        let plan = plan_failure(
            &mut session,
            &mut ctx,
            true,
            TurnFailureReason::Cancelled,
            false,
        )
        .expect("in-flight turn must produce a plan");

        // turn 必须等于冻结**前**的 index（interrupt_turn 后会 +1）
        assert_eq!(plan.turn, turn_before);
        assert_eq!(session.turn_index(), turn_before + 1);
        assert_eq!(session.committed_turns().len(), 1);
        assert!(plan.snapshot.is_some(), "persist=true 必须产出快照");
        assert_eq!(plan.partial_text_len, "partial".len());
        // 冻结的轮里必须有合成的 Output，否则历史对 provider 非法
        let turn = &session.committed_turns()[0];
        assert!(turn.iter().any(|am| matches!(
            am.message.as_ref(),
            InputItem::FunctionCallOutput { call_id, .. } if call_id == "c1"
        )));
        // partial_text 被取走，looper 状态被清
        assert!(ctx.assistant_text.is_empty());
        assert!(ctx.pending_tool_calls.is_empty());
    }

    /// C2 的 drain 捞回的结果必须由 `plan_failure` 写进 staging，随本轮一起冻结 ——
    /// 这是「取消时已完成的工具成果不丢」的最后一环。
    #[test]
    fn test_plan_failure_stages_salvaged_tool_results() {
        let mut session = plan_session();
        session.start_turn("do it".into()).unwrap();
        session
            .stage_item(
                MessageSource::ModelGeneration,
                InputItem::FunctionCall {
                    call_id: "c1".to_string(),
                    name: "t1".to_string(),
                    arguments: "{}".to_string(),
                },
            )
            .unwrap();

        let call = Arc::new(ToolCall::new("c1", "t1", "{}"));
        let mut ctx = ReActContext {
            pending_tool_calls: vec![PendingToolCall {
                call: Arc::clone(&call),
                result: Some(ToolCallResult {
                    call,
                    result: Content::Text("salvaged".to_string()),
                    is_error: false,
                }),
            }],
            ..Default::default()
        };

        let plan = plan_failure(
            &mut session,
            &mut ctx,
            true,
            TurnFailureReason::Cancelled,
            false,
        )
        .expect("有在途轮");

        // 计数取的是冲刷**前**的 staging 规模（悬空的 FunctionCall 计 1 条）
        assert_eq!(plan.discarded_staging_messages, 1);

        let turn = &session.committed_turns()[0];
        assert!(
            turn.iter().any(|am| matches!(
                am.message.as_ref(),
                InputItem::FunctionCallOutput { call_id, output }
                    if call_id == "c1" && matches!(output, Content::Text(t) if t == "salvaged")
            )),
            "捞回的工具结果必须随本轮冻进 committed"
        );
        assert!(ctx.pending_tool_calls.is_empty(), "ctx 必须清空，防重放");
    }

    #[test]
    fn test_plan_failure_only_user_input_degenerates() {
        let mut session = plan_session();
        session.start_turn("hi".into()).unwrap();
        let mut ctx = ReActContext::default();

        let plan = plan_failure(
            &mut session,
            &mut ctx,
            true,
            TurnFailureReason::Cancelled,
            false,
        )
        .expect("Active with only user_input still has a turn to close");

        assert_eq!(plan.turn, 0);
        assert!(session.committed_turns().is_empty(), "退化路径=rollback");
        assert_eq!(session.turn_index(), 0, "退化路径不自增");
        assert_eq!(session.state(), SessionState::Idle);
    }

    #[test]
    fn test_plan_failure_returns_none_on_idle() {
        // 无在途轮 → 不发语义为空的 TurnComplete
        let mut session = plan_session();
        let mut ctx = ReActContext::default();

        assert!(
            plan_failure(
                &mut session,
                &mut ctx,
                true,
                TurnFailureReason::Cancelled,
                false,
            )
            .is_none()
        );
    }

    #[test]
    fn test_plan_failure_persist_false_skips_snapshot_only() {
        let mut session = plan_session();
        session.start_turn("q".into()).unwrap();
        session
            .stage_item(
                MessageSource::ModelGeneration,
                InputItem::FunctionCall {
                    call_id: "c1".to_string(),
                    name: "t1".to_string(),
                    arguments: "{}".to_string(),
                },
            )
            .unwrap();
        let mut ctx = ReActContext::default();

        let plan = plan_failure(
            &mut session,
            &mut ctx,
            false,
            TurnFailureReason::TotalTimeout,
            false,
        )
        .unwrap();

        // CLI 路径：冻结照常（内存），只是不落盘 —— committed 有这一轮、快照为空
        assert!(plan.snapshot.is_none());
        assert_eq!(session.committed_turns().len(), 1);
    }

    #[test]
    fn test_failure_label_covers_all_variants_without_debug_leak() {
        let variants = [
            TurnFailureReason::Cancelled,
            TurnFailureReason::TotalTimeout,
            TurnFailureReason::PerTurnTimeout,
            TurnFailureReason::MaxTurnsExceeded,
            TurnFailureReason::HookAbort("hook says no".to_string()),
            TurnFailureReason::Other("boom".to_string()),
        ];
        for reason in &variants {
            let label = failure_label(reason);
            assert!(!label.is_empty(), "{reason:?} produced an empty label");
            // Debug 泄漏会把 `HookAbort("x")` 原样送进模型上下文
            assert!(
                !label.contains("Some("),
                "{reason:?} leaked Debug formatting into {label:?}"
            );
        }
    }

    // ── looper 级收尾测试 ──────────────────────────────────────────────

    /// 计数 persister —— `NullSessionPersister` 是 no-op，看不出有没有被调用。
    #[derive(Default)]
    struct CountingPersister {
        saves: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl crate::persistence::SessionPersister for CountingPersister {
        async fn save(
            &self,
            _snapshot: &SessionSnapshot,
            _session_id: &str,
            _description: &str,
            _created_at: u64,
        ) -> Result<crate::persistence::PersistResult, crate::persistence::PersistError> {
            self.saves.fetch_add(1, Ordering::SeqCst);
            Ok(crate::persistence::PersistResult {
                bytes_written: 0,
                path: std::path::PathBuf::new(),
            })
        }
        async fn load(
            &self,
            _session_id: &str,
        ) -> Result<Option<(SessionSnapshot, crate::SessionMeta)>, crate::persistence::PersistError>
        {
            Ok(None)
        }
        async fn delete(&self, _session_id: &str) -> Result<(), crate::persistence::PersistError> {
            Ok(())
        }
        async fn list(&self) -> Result<Vec<crate::SessionMeta>, crate::persistence::PersistError> {
            Ok(Vec::new())
        }
    }

    /// 失败路径永不触达模型 —— 所有方法 `unimplemented!()`。
    struct PanicProvider;

    #[async_trait::async_trait]
    impl model_provider::ModelProvider for PanicProvider {
        fn name(&self) -> &str {
            "panic-provider"
        }
        async fn generate_full(
            &self,
            _request: &model_provider::GenerateRequest,
        ) -> Result<model_provider::GenerateResult, model_provider::ProviderError> {
            unimplemented!("failure path must not reach the model")
        }
        async fn generate_stream(
            &self,
            _request: &model_provider::GenerateRequest,
        ) -> Result<model_provider::GenerateStream, model_provider::ProviderError> {
            unimplemented!("failure path must not reach the model")
        }
    }

    /// 失败收尾测试的 looper 及其通道端点。
    ///
    /// 端点都是字段而非返回值的一部分，因为**必须保活**：
    /// `user_speaker` 一 drop，`run()` 就认为输入 channel 已关闭而提前退出；
    /// `events` 一 drop，`emit_event_guaranteed` 送不出去。
    struct FailureHarness {
        looper: AgentLooper,
        /// 只作为「输入 channel 未关闭」的保活句柄存在，本身从不被读。
        _user_speaker: Speaker<UserMsg>,
        /// `run()` 取所有权，故用 `Option` 让调用点能把它移出去。
        user_listener: Option<Listener<UserMsg>>,
        events: Listener<LooperEvent>,
        persister: Arc<CountingPersister>,
    }

    /// 构造失败收尾测试用的 looper：`PanicProvider` + 计数 persister
    /// （失败路径永不触达模型），会话预置一个悬空 `FunctionCall`，
    /// 使收尾走冻结路径而非退化 rollback。
    fn failure_looper_harness(
        cancel: bool,
        persist_on_failure: bool,
        pending_input: bool,
    ) -> FailureHarness {
        let profile: crate::agent::AgentProfile = serde_yaml::from_str(
            "agent:\n  name: t\n  description: d\nllm:\n  provider: p\n  model: m\n",
        )
        .unwrap();
        let executor: Arc<dyn crate::tools::ToolExecutor> =
            Arc::new(crate::tools::DefaultToolsExecutor::new(Vec::new()));
        let agent = Arc::new(Agent::from_parts(
            std::path::PathBuf::from("/tmp/agent.md"),
            profile,
            "sys".to_string(),
            Arc::new(PanicProvider),
            crate::agent::ModelConfigBuilder::new().build(),
            Arc::clone(&executor),
            Arc::new(crate::mcp::McpManager::empty(executor)),
            None,
        ));

        let mut session = Session::new("s1".to_string(), "d".to_string());
        session.start_turn("q".into()).unwrap();
        session
            .stage_item(
                MessageSource::ModelGeneration,
                InputItem::FunctionCall {
                    call_id: "c1".to_string(),
                    name: "t1".to_string(),
                    arguments: "{}".to_string(),
                },
            )
            .unwrap();
        if pending_input {
            session.enqueue_pending("queued".into());
        }

        let (looper_side, caller_side) = make_async_intercom_pair::<LooperEvent, UserMsg>(64);
        let (event_speaker, user_listener) = looper_side.split();
        let (user_speaker, event_listener) = caller_side.split();

        let persister = Arc::new(CountingPersister::default());
        let looper = AgentLooper::new(
            agent,
            Box::new(session),
            event_speaker,
            Arc::new(AtomicBool::new(cancel)),
            Arc::new(AtomicBool::new(false)),
            LooperConfig {
                persist_on_failure,
                ..Default::default()
            },
            Arc::clone(&persister) as Arc<dyn crate::persistence::SessionPersister>,
        );

        FailureHarness {
            looper,
            _user_speaker: user_speaker,
            user_listener: Some(user_listener),
            events: event_listener,
            persister,
        }
    }

    impl FailureHarness {
        /// 数一数收到过几次 `TurnComplete`。
        fn turn_completes(&mut self) -> usize {
            let mut n = 0;
            while let Ok(ev) = self.events.try_recv() {
                if matches!(ev, LooperEvent::TurnComplete { .. }) {
                    n += 1;
                }
            }
            n
        }
    }

    /// 驱动 run() 直到取消收尾完成，返回 (TurnComplete 次数, committed 轮数, 落盘次数)。
    async fn drive_cancelled_looper(persist_on_failure: bool) -> (usize, usize, usize) {
        let mut h = failure_looper_harness(true, persist_on_failure, false);
        let listener = h.user_listener.take().expect("harness 持有 listener");
        let _ = h.looper.run(listener).await;

        (
            h.turn_completes(),
            h.looper.session().committed_turns().len(),
            h.persister.saves.load(Ordering::SeqCst),
        )
    }

    /// 失败后自动续接排队输入时，上一轮的失败原因不得跨轮存活 ——
    /// 续接的那一轮若正常跑到 `Done`，会撞上 `debug_assert!(failure_reason.is_none())`。
    #[tokio::test]
    async fn test_auto_continue_clears_failure_reason() {
        let mut h = failure_looper_harness(false, true, true);

        let continued = h
            .looper
            .finalize_failure(TurnFailureReason::HookAbort("hooked".into()), true)
            .await;
        assert!(continued, "队列里有输入时必须续接");
        assert!(
            h.looper.failure_reason.is_none(),
            "续接后必须清空 failure_reason；现状={:?}",
            h.looper.failure_reason
        );

        // 续接的那一轮正常跑完 —— Done 分支的 debug_assert 不得触发
        h.looper.react_state = ReActState::Done;
        h.looper.react_step().await;
        assert_eq!(h.looper.session().committed_turns().len(), 2);
    }

    /// 非取消类失败收尾后应停靠回 `Idle` 等下一句输入，而不是终止整个 run()。
    #[tokio::test]
    async fn test_non_cancel_failure_docks_at_idle() {
        let mut h = failure_looper_harness(false, true, false);
        // 模拟 react_step 已挂起的失败（如 hook abort / max turns）
        h.looper.react_state = ReActState::Failed;
        h.looper.failure_reason = Some(TurnFailureReason::HookAbort("boom".into()));

        let listener = h.user_listener.take().expect("harness 持有 listener");
        let res = tokio::time::timeout(Duration::from_millis(300), h.looper.run(listener)).await;
        assert!(
            res.is_err(),
            "失败收尾后应停靠等输入，而不是 break 退出 run()"
        );

        assert_eq!(
            h.looper.session().committed_turns().len(),
            1,
            "非取消失败同样要冻结在途轮"
        );
        assert!(matches!(h.looper.outer_state(), OuterState::Idle));
        assert_eq!(h.turn_completes(), 1, "冻结必须恰好发一次 TurnComplete");
    }

    #[tokio::test]
    async fn test_cancel_freezes_turn_and_emits_turn_complete_once() {
        let (turn_completes, committed, _) = drive_cancelled_looper(true).await;
        assert_eq!(turn_completes, 1, "取消收尾必须恰好发一次 TurnComplete");
        assert_eq!(committed, 1, "中断轮必须冻结进 committed");
    }

    #[tokio::test]
    async fn test_persist_on_failure_gates_save() {
        let (_, committed_on, saves_on) = drive_cancelled_looper(true).await;
        assert_eq!(committed_on, 1);
        assert_eq!(saves_on, 1, "persist_on_failure=true 必须落盘一次");

        let (_, committed_off, saves_off) = drive_cancelled_looper(false).await;
        assert_eq!(committed_off, 1, "冻结无条件发生（内存）");
        assert_eq!(saves_off, 0, "persist_on_failure=false 不得落盘");
    }

    // ── State enum tests ───────────────────────────────────────────────

    #[test]
    fn test_outer_state_serde() {
        let state = OuterState::Idle;
        let json = serde_json::to_string(&state).unwrap();
        let deserialized: OuterState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, OuterState::Idle);
    }

    #[test]
    fn test_react_state_serde() {
        let state = ReActState::PreparingRequest;
        let json = serde_json::to_string(&state).unwrap();
        let deserialized: ReActState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, ReActState::PreparingRequest);
    }
}
