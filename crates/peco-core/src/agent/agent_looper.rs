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
    /// 截断重试：单个用户轮内允许的重试次数上限。`0` = 关闭。
    ///
    /// 触发条件是「模型输出顶到 `max_output_tokens` 被截断」
    /// （`FinishReason::MaxTokens` / `ResponseStatus::Incomplete`）——
    /// 推理 token 与可见输出共用这一个预算，预算不足时推理会把可见输出挤没。
    ///
    /// 每次重试都是一次真实的模型调用，**消耗 `react_loop_iteration`
    /// （即 `max_turns` 预算）**，因此上限不需要很大就能收敛。
    /// 默认为 1：截断是确定性的预算问题，抬一次就够；抬了还截断说明
    /// 配置的预算本身不合适，继续重试只是烧钱。
    pub truncation_retry_limit: u32,
    /// 截断重试时的输出预算**下限**（token）。实际取
    /// `max(ModelConfig::max_tokens, 该值)` —— 取 max 而非直接替换，
    /// 免得给已配了大预算的 agent 降配额。
    ///
    /// 默认 32_768，与 provider 侧对思考模式的既有上限常量同量级
    /// （见 `QWEN_THINKING_MAX_TOKENS`）。事故现场的量级是「预算 4096、
    /// 推理 13375 字符、可见输出 491 字符」，32K 给推理留足空间。
    ///
    /// **这是安全网，不是 `max_tokens` 的替代品。** 正经修法是给 `agent.md`
    /// 的 `llm:` 显式写 `max_tokens`；本字段只保证「忘了写」或被服务端默认值
    /// 卡住时，一轮对话不会整轮判死。
    ///
    /// 若模型真实输出上限低于该值，重试请求会被网关拒（400）—— 那时把本值
    /// 调低即可。该失败不劣于现状，见 `begin_truncation_retry` 的残片兜底。
    pub truncation_retry_min_budget: u32,
    /// 瞬时故障重发：单个用户轮内允许的重发次数上限。`0` = 关闭。
    ///
    /// 触发条件是 [`ProviderError::classify`](model_provider::ProviderError)
    /// 判定为瞬时类的失败 — 限流、网络中断、上游 5xx。与截断重试正交：
    /// 截断重发抬预算，瞬时重发原样重发（只退避等待）。
    ///
    /// 每次重发同样消耗 `react_loop_iteration`，两个上限都有界 ⇒ 无死循环。
    /// 默认 2：配合指数退避（500ms → 1s），瞬时抖动两次内基本自愈；
    /// 持续故障更快上抛给用户看到类型化报错。
    pub transient_retry_limit: u32,
    /// 瞬时重发的退避起始延迟（毫秒）。第 n 次重发延迟 =
    /// `base * 2^(n-1)`，截断到 [`Self::transient_retry_max_delay_ms`]。
    /// 默认 500。
    pub transient_retry_base_delay_ms: u64,
    /// 瞬时重发的退避延迟上限（毫秒）。默认 5000。
    pub transient_retry_max_delay_ms: u64,
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
            truncation_retry_limit: 1,
            truncation_retry_min_budget: 32_768,
            transient_retry_limit: 2,
            transient_retry_base_delay_ms: 500,
            transient_retry_max_delay_ms: 5_000,
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
    /// 触发限流，已进行 `attempts` 次尝试（含首次）后放弃。
    /// `message` = provider 原始错误 msg（body 摘要）
    RateLimited {
        /// 实际发起的尝试总数
        attempts: u32,
        /// provider 原始错误 msg
        message: String,
    },
    /// 网络中断 / 上游 5xx，`attempts` 次尝试后放弃
    ModelUnavailable {
        /// 实际发起的尝试总数
        attempts: u32,
        /// provider 原始错误 msg
        message: String,
    },
    /// API Key 无效或无权限
    AuthError {
        /// provider 原始错误 msg
        message: String,
    },
    /// 额度/余额耗尽
    QuotaExhausted {
        /// provider 原始错误 msg
        message: String,
    },
    /// 上下文超出模型窗口 — 需要压缩后重发（当前仅报错）
    ContextOverflow {
        /// provider 原始错误 msg
        message: String,
    },
    /// 输出被内容过滤拦截
    ContentFiltered {
        /// provider 原始错误 msg
        message: String,
    },
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

    /// 本次模型尝试已作废，正在重试。
    ///
    /// **纯通知，不含撤销语义**：接收方不删除任何已下发的增量。该尝试的产出
    /// 已由 [`Self::begin_truncation_retry`] / [`Self::begin_transient_retry`]
    /// 从 Session staging 中回退，因此它只存在于「实时视图」里，不存在于任何
    /// 权威历史中 —— 这正是本事件要解释的事实。前端重载后从快照恢复，
    /// 残句与通知一并消失。
    ///
    /// wire 名保持 `truncation_retry`（历史兼容），`reason` 区分两种重发：
    /// 截断（抬预算）与瞬时故障（退避原样重发）。
    TruncationRetry {
        turn_index: usize,
        /// 第几次重试（1-based）— 各 reason 独立计数
        attempt: u32,
        /// 本轮对应 reason 的重试上限（0 = 关闭，此时不会发出本事件）
        limit: u32,
        /// 截断那次的输出 token 数（瞬时重发传 0）
        output_tokens: u32,
        /// 抬升后的输出预算（瞬时重发传当前生效预算）
        retry_budget: u32,
        /// 该次尝试已下发的正文增量。
        ///
        /// **仅供落库侧对齐**（`chat` 模块的 `messages` 表累加器按后缀剥离），
        /// **不向客户端下发** —— `map_looper_event` 刻意过滤掉它。实时视图
        /// 保留这段残句是本设计的取舍，见 `docs/design/truncation-retry-notice.md`。
        discarded_text: String,
        /// 重发原因：截断（抬预算）还是瞬时故障（退避重发）
        reason: RetryNoticeReason,
    },

    /// Looper 即将退出 `run()` 方法。
    Shutdown {
        reason: String,
        total_turns: usize,
        total_usage: Usage,
    },
}

/// 重发原因（[`LooperEvent::TruncationRetry`] 的分类）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RetryNoticeReason {
    /// 输出被 max_tokens 截断，抬高预算重发
    Truncated,
    /// 瞬时故障（限流/网络/5xx），退避后原样重发
    Transient,
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

    /// 本用户轮是否已为当前 query 解析过动态上下文。
    ///
    /// "末条是 user" 这个判据不足以说明「该解析」：截断重试会把 staging 回退到
    /// 请求前，末条又变回 user，于是 [`DynamicContext::query`] 对同一条 query 重跑
    /// 一遍完整召回 —— 检索有副作用（命中计数写库、额外一次 embedding），重跑
    /// 不是幂等的。以本标记取代「复用它」的隐式约定。
    ///
    /// 与 `react_loop_iteration` **同生命周期**（单个用户轮），由
    /// [`Self::reset_turn_counters`] 清除。
    dynamic_context_resolved: bool,

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

    // ── 截断重试（纯运行时状态，不落盘）──
    /// 本用户轮内已用掉的截断重试次数。
    ///
    /// 与 `react_loop_iteration` **同生命周期**（单个用户轮），因此凡是要重置
    /// 前者的地方都必须重置后者 —— 见 [`Self::reset_turn_counters`]。
    truncation_retries_used: usize,
    /// 被回退掉的那次截断尝试的 `assistant_text`。
    ///
    /// 兜底用：重试请求**本身**发不出去时（如抬高的预算超过模型真实上限被
    /// 400 拒），[`Self::finalize_failure`] 把它还给 `react_ctx`，让收尾仍能
    /// 带上 `partial_text`，并由 [`plan_failure`] 补进 staging 保住整轮 ——
    /// 「最坏情况不劣于不重试」由此成立。重试的产出落进 staging 后即作废。
    truncation_salvage: Option<String>,

    // ── 瞬时故障重发（纯运行时状态，不落盘）──
    /// 本用户轮内已用掉的瞬时重发次数。与 `truncation_retries_used`
    /// 同生命周期、同重置点（[`Self::reset_turn_counters`]），但**独立计数** —
    /// 两种重试的约束与动作不同（抬预算 vs 退避原样重发），合并计数会让
    /// 一次限流重发挤掉唯一的截断重试额度。
    transient_retries_used: u32,
    /// 下次重发的退避截止时刻。`None` = 无需等待。
    /// 由 [`Self::begin_transient_retry`] 设置，在 [`Self::prepare_and_send_request`]
    /// 入口以可取消的切片 sleep 等待 —— 等待贴在请求发出前，回退与通知立即发生。
    /// 等待**只读不清**（见 [`Self::wait_retry_backoff`]）：`react_step` future
    /// 被 select drop 后重入仍能继续等剩余时间。
    retry_deadline: Option<Instant>,
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
            dynamic_context_resolved: false,
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
            truncation_retries_used: 0,
            truncation_salvage: None,
            transient_retries_used: 0,
            retry_deadline: None,
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
                self.reset_turn_counters();

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
                        self.reset_turn_counters();
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

    // ── 截断重试 ────────────────────────────────────────────────────────

    /// 新一轮对话开始时重置「本用户轮」的运行时状态。
    ///
    /// `react_loop_iteration`、`truncation_retries_used`、`truncation_salvage`
    /// 与 `dynamic_context_resolved` 的生命周期都恰好是一个用户轮 —— 抽成方法
    /// 就是为了让这条不变量无法被单独违反。
    ///
    /// 残片尤其不能跨轮：它只是给本轮收尾兜底的，留着会让下一轮的失败卡片
    /// 显示上一轮的文本。
    fn reset_turn_counters(&mut self) {
        self.react_loop_iteration = 0;
        self.truncation_retries_used = 0;
        self.truncation_salvage = None;
        self.transient_retries_used = 0;
        self.retry_deadline = None;
        self.dynamic_context_resolved = false;
    }

    /// 本用户轮已实际发起的模型调用次数（含刚失败的这次）。
    ///
    /// `react_loop_iteration` 在 [`Self::prepare_and_send_request`] 入口递增，
    /// 失败发生时它已包含当前尝试 —— 正是失败原因里 `attempts` 要报的数。
    fn total_attempts(&self) -> u32 {
        self.react_loop_iteration as u32
    }

    /// 本次截断是否应该重试。纯判定，无副作用。
    fn can_retry_truncation(&self) -> bool {
        // 1. 次数上限（0 = 关闭）
        if self.truncation_retries_used >= self.config.truncation_retry_limit as usize {
            return false;
        }
        // 2. 用户按了停止 —— 不再发起新的模型调用
        if self.is_cancelled() {
            return false;
        }
        // 3. 轮数预算：重试要重新走 `prepare_and_send_request`，会消耗一次
        //    `react_loop_iteration`。没有下一次调用额度时不重试 —— 否则状态机
        //    刚被置回 `PreparingRequest` 就撞上 `MaxTurnsExceeded`，把「截断」
        //    这个真实原因换成「超出轮数」，诊断信息反而变差。
        if self.react_loop_iteration >= self.max_turns {
            return false;
        }
        // 4. 抬不动预算 = 重试请求与上一次逐字节相同，必然同样截断，白烧一次调用。
        //    比的是**当前生效**的预算而非配置值：重试过一次后生效预算已是抬高值，
        //    再比配置值会得出「还抬得动」，把 `truncation_retry_limit >= 2` 的
        //    第二次重试放行成一次注定失败的调用。
        //    未配置 `max_tokens` 时按 0 计：此时走 provider 服务端默认值
        //    （DeepSeek 约 4096），远低于任何合理的重试下限，取 0 不会误判。
        self.effective_output_budget() < self.retry_output_budget()
    }

    /// 本次请求实际会用的输出预算。把「一次性覆盖 → 配置值 → 未设」归一成一个数。
    ///
    /// 未设时按 0 计 —— 仅用于比较（见 [`Self::can_retry_truncation`] 第 4 条），
    /// 不用于发请求：发请求时 `None` 表示交给 provider 服务端默认值。
    fn effective_output_budget(&self) -> u32 {
        self.max_output_tokens_override()
            .or(self.agent.model_config().max_tokens)
            .unwrap_or(0)
    }

    /// 重试请求要用的输出预算：`max(现预算, 配置下限)`。
    ///
    /// 取 `max` 而非直接取配置值：配置是**下限**，已配了更大预算的 agent
    /// 不该因为一次截断重试被降配额。
    fn retry_output_budget(&self) -> u32 {
        self.agent
            .model_config()
            .max_tokens
            .unwrap_or(0)
            .max(self.config.truncation_retry_min_budget)
    }

    /// 本 looper 当前生效的输出预算覆盖值（`None` = 沿用 `ModelConfig::max_tokens`）。
    ///
    /// **粘性**：本轮发生过截断重试后，后续 ReAct 迭代继续用抬高的预算。
    /// 否则 tool 调用之后的下一次迭代可能以同样的方式再截断一次，
    /// 把刚省下的那次调用又浪费掉。计数器随新用户轮次归零，粘性不出轮。
    fn max_output_tokens_override(&self) -> Option<u32> {
        (self.truncation_retries_used > 0).then(|| self.retry_output_budget())
    }

    /// 截断重试：回退 staging 到本次请求前的锚点，清运行态，打回 `PreparingRequest`。
    ///
    /// 返回 `false` 表示不重试 —— 调用方走既有失败路径，零行为回归。
    /// 返回 `true` 时内层状态机已置回 [`ReActState::PreparingRequest`]，
    /// 下一次 `react_step()` 会重新走 [`Self::prepare_and_send_request`]，
    /// 以抬高的输出预算重发同一个请求。
    ///
    /// # 为什么必须回退 staging
    /// 两条非完成路径都是「先 [`Self::stage_output_blocks`] 再判状态」，截断的块
    /// 已经写进 staging。不回退就重发，两次尝试的 assistant 块会叠在一起，直接
    /// 违反 provider 线格式不变量（有 tool 结果待配对期间不得出现 assistant
    /// Message）。回退到请求前的下标，语义上等价于「这次模型调用从未发生」——
    /// 回退后 staging 的结尾与发请求前逐字节相同，而那个状态本身是合法的
    /// （它来自上一次成功步骤）。
    ///
    /// # 为什么不会死循环、为什么不吞掉轮数预算
    /// 重试要重新走 `prepare_and_send_request`，因此**消耗一次
    /// `react_loop_iteration`**：重试是一次真实的模型调用，不占额度会让
    /// `max_turns` 失去意义。次数另受 [`LooperConfig::truncation_retry_limit`]
    /// 约束，两个上限都有界 ⇒ 不可能死循环。
    ///
    /// # 不设 `failure_reason`
    /// 本路径是**恢复**不是失败。故意不碰 `failure_reason`：重试最终跑到
    /// `Done` 时会撞上 `Done` 分支的 `debug_assert!(failure_reason.is_none())`。
    ///
    /// # 锚点由调用方在收敛点就地取
    /// `checkpoint` 是**本次模型请求发出前** staging 的条数，由
    /// [`Self::finish_stream`] / [`Self::resolve_batch_response`] 在
    /// [`Self::stage_output_blocks`] 之前就地读取。它不存成字段：从
    /// `PreparingRequest` 到收敛点之间没有任何 staging 写入（chunk 处理只发事件、
    /// 写 `react_ctx`、记账 usage），因此收敛点的长度必然等于发请求前的长度 ——
    /// 存字段反而多一条「必须在每条路径上写对」的人工不变量。
    ///
    /// # 不刷新在途检查点
    /// 锚点即 `Session::staging_checkpoint()`，与 [`Self::stage_tool_results`]
    /// 落盘的 `InflightCheckpoint` 在同一口径上，且取锚点时常无其他 staging 写入，
    /// 即 `锚点 == 检查点条数`。被丢弃的消息从不属于检查点，落盘的检查点依然
    /// 精确；反而若在此重写，会把已落地的工具结果从检查点抹掉。
    ///
    /// # 为什么发 [`LooperEvent::TruncationRetry`] 而不回退传输层
    /// 已下发的增量留在实时视图里，由一条通知解释它们为什么不属于最终答案。
    /// 用 `emit_event_guaranteed` 而非 `try_send`：丢一条通知 = 画面上多出一段
    /// 无法解释的残句、本特性整个没发生（同 [`LooperEvent::ContextCompacted`]
    /// 的处置）。
    async fn begin_truncation_retry(
        &mut self,
        turn: usize,
        output_tokens: u32,
        checkpoint: usize,
    ) -> bool {
        if !self.can_retry_truncation() {
            return false;
        }

        let (dropped, salvaged_text, had_output) = match self.rollback_attempt(checkpoint) {
            Some(r) => r,
            None => {
                warn!(turn, "Truncation retry skipped: staging rollback failed");
                return false;
            }
        };

        self.truncation_retries_used += 1;
        self.react_state = ReActState::PreparingRequest;

        warn!(
            turn,
            dropped_staging_messages = dropped,
            retry = self.truncation_retries_used,
            limit = self.config.truncation_retry_limit,
            output_tokens_at_truncation = output_tokens,
            configured_budget = self.agent.model_config().max_tokens,
            retry_budget = self.retry_output_budget(),
            "Model output truncated at max_tokens; retrying with a larger output budget"
        );

        // 通知必须在状态置回 `PreparingRequest` **之后**发：同一 Speaker FIFO
        // 保证它早于重试尝试的第一条 TextDelta，前端插的横幅因此落在残句与
        // 新正文之间。`discarded_text` 取自 `rollback_attempt` 的 `take`，
        // 是唯一真相源。
        //
        // 零产出（截断且什么都没吐）不发：没有需要解释的残句，发出去只会让
        // 前端插一条无上下文的横幅。
        if had_output {
            Self::emit_event_guaranteed(
                &self.event_speaker,
                LooperEvent::TruncationRetry {
                    turn_index: turn,
                    attempt: self.truncation_retries_used as u32,
                    limit: self.config.truncation_retry_limit,
                    output_tokens,
                    retry_budget: self.retry_output_budget(),
                    discarded_text: salvaged_text,
                    reason: RetryNoticeReason::Truncated,
                },
            )
            .await;
        }
        true
    }

    /// 回退本次模型尝试的公共骨架：staging 回退到锚点 + 清运行态 + 残片入 salvage。
    ///
    /// 截断重发与瞬时重发共用。返回 `(staging 回退条数, 残片正文, 是否有产出)`；
    /// staging 回退失败（状态非 Active / 锚点越界）返回 `None` —— 此时 staging
    /// 未被改动，调用方走既有失败路径，与无此特性完全一致。
    ///
    /// 残片入 [`Self::truncation_salvage`] 作为失败收尾兜底（字段名沿用，
    /// 语义为「最近一次**非空**的被回退正文」）；正文比推理多留一份，
    /// 它同时是通知载荷（落库侧按后缀剥离用）。
    ///
    /// 空残片不覆写：连续两次回退（如截断重发后、重发尚未收到响应又遇瞬时
    /// 故障）时，第二次零产出若把 `Some("")` 写进去，最终失败 restore 还回
    /// 空串 —— 截断那次的残句凭空消失，反而劣于不重试。新残片非空则照常
    /// 覆写：更新鲜的正文才是用户此刻在画面上看到的内容。
    fn rollback_attempt(&mut self, checkpoint: usize) -> Option<(usize, String, bool)> {
        let dropped = match self.session.truncate_staging(checkpoint) {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, "Attempt rollback failed; not retrying");
                return None;
            }
        };

        let salvaged_text = std::mem::take(&mut self.react_ctx.assistant_text);
        let salvaged_reasoning = std::mem::take(&mut self.react_ctx.assistant_reasoning);
        let had_output = !salvaged_text.is_empty() || !salvaged_reasoning.is_empty();
        if !salvaged_text.is_empty() {
            self.truncation_salvage = Some(salvaged_text.clone());
        }
        self.react_ctx.pending_tool_calls.clear();
        self.react_ctx.batch_response = None;

        Some((dropped, salvaged_text, had_output))
    }

    /// 本次瞬时故障是否应该重发。纯判定，无副作用。
    ///
    /// 与 [`Self::can_retry_truncation`] 的差别：没有「预算抬得动」条件
    /// （瞬时重发不抬预算，原样重发），次数走独立的
    /// [`LooperConfig::transient_retry_limit`] 计数。
    fn can_retry_transient(&self) -> bool {
        // 1. 次数上限（0 = 关闭）
        if self.transient_retries_used >= self.config.transient_retry_limit {
            return false;
        }
        // 2. 用户按了停止 —— 不再发起新的模型调用
        if self.is_cancelled() {
            return false;
        }
        // 3. 轮数预算：与截断重试同理，重发会撞 `MaxTurnsExceeded`
        //    把真实故障原因换成「超出轮数」，诊断信息反而变差。
        if self.react_loop_iteration >= self.max_turns {
            return false;
        }
        true
    }

    /// 瞬时故障重发：回退 staging 到本次请求前的锚点，清运行态，设退避截止时刻，
    /// 打回 `PreparingRequest`。
    ///
    /// 与 [`Self::begin_truncation_retry`] 共用 [`Self::rollback_attempt`] 骨架，
    /// 差别在于：不抬输出预算（原样重发）、独立计数、多一步退避等待 ——
    /// 等待不在此处做，而是记 `retry_deadline`，由
    /// [`Self::prepare_and_send_request`] 在下次请求发出前以可取消的切片
    /// sleep 等待（回退与通知立即发生，取消检查天然衔接入口守卫）。
    ///
    /// `trigger` 是故障类别的日志字段（`rate_limited` / `network` / `server`）。
    /// 返回 `true` 时内层状态机已置回 [`ReActState::PreparingRequest`]。
    async fn begin_transient_retry(
        &mut self,
        turn: usize,
        checkpoint: usize,
        trigger: &'static str,
    ) -> bool {
        if !self.can_retry_transient() {
            return false;
        }

        let (dropped, salvaged_text, had_output) = match self.rollback_attempt(checkpoint) {
            Some(r) => r,
            None => return false,
        };

        self.transient_retries_used += 1;
        self.react_state = ReActState::PreparingRequest;

        // 指数退避：base * 2^(n-1)，截断到上限。
        let delay_ms = self
            .config
            .transient_retry_base_delay_ms
            .saturating_mul(1u64 << (self.transient_retries_used - 1).min(16))
            .min(self.config.transient_retry_max_delay_ms);
        self.retry_deadline = Some(Instant::now() + Duration::from_millis(delay_ms));

        warn!(
            turn,
            trigger,
            dropped_staging_messages = dropped,
            attempt = self.transient_retries_used,
            limit = self.config.transient_retry_limit,
            delay_ms,
            "Transient model failure; scheduling retry"
        );

        // 通知语义与截断重发一致（见 `begin_truncation_retry`）。
        // 瞬时重发不抬预算，`output_tokens` 无意义传 0，`retry_budget`
        // 传当前生效预算供展示。
        if had_output {
            Self::emit_event_guaranteed(
                &self.event_speaker,
                LooperEvent::TruncationRetry {
                    turn_index: turn,
                    attempt: self.transient_retries_used,
                    limit: self.config.transient_retry_limit,
                    output_tokens: 0,
                    retry_budget: self
                        .max_output_tokens_override()
                        .unwrap_or_else(|| self.agent.model_config().max_tokens.unwrap_or(0)),
                    discarded_text: salvaged_text,
                    reason: RetryNoticeReason::Transient,
                },
            )
            .await;
        }
        true
    }

    /// 退避等待：等到 `retry_deadline`（若有）。可取消 — 每 ~200ms 切片
    /// 检查一次取消标志，取消后立即返回，让 [`Self::prepare_and_send_request`]
    /// 入口的取消守卫收尾。无待等待的 deadline 时是 no-op。
    ///
    /// **只读不 `take()`**：`run()` 的 select 在用户输入到达时会 drop 进行中
    /// 的 `react_step` future。取走 deadline 会把它带进 future 栈帧，drop 即
    /// 丢失，重建后零退避立即重发 —— 退避对「等待期间用户发过消息」失效。
    /// 留在 `self` 上则被 drop 后重入本函数继续等**剩余**时间（幂等）。
    /// 过期 deadline 循环条件天然为假；下次 [`Self::begin_transient_retry`]
    /// 覆写、[`Self::reset_turn_counters`] 清零，无需在此清除。
    async fn wait_retry_backoff(&mut self) {
        let Some(deadline) = self.retry_deadline else {
            return;
        };
        while Instant::now() < deadline {
            if self.is_cancelled() {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let slice = remaining.min(Duration::from_millis(200));
            tokio::time::sleep(slice).await;
        }
    }

    /// 把上一次截断留下的残片还给 `react_ctx`。由 [`Self::finalize_failure`]
    /// 在收尾漏斗顶部调用 —— 那里是唯一的归还点。
    ///
    /// 不回还的代价：staging 已被 [`Self::begin_truncation_retry`] 回退掉，
    /// `react_ctx.assistant_text` 又被清空，`TurnOutcome::Failed.partial_text`
    /// 于是变空 —— 最坏情况反而**劣于**不重试。还给 `react_ctx` 之后，
    /// [`plan_failure`] 的 partial_text 与 [`Session::stage_salvage`] 的补条
    /// 都拿得到它，收尾既带文本也仍能冻结整轮。
    ///
    /// 无残片时**不触碰该字段**：非重试路径的行为逐字节不变。
    ///
    /// # 只填空
    /// `react_ctx.assistant_text` 非空说明**重试之后的那次尝试**自己已经吐过
    /// 文本（那是用户此刻在画面上看到的内容），此时残片是陈旧的，覆盖它就
    /// 等于把「这一次」的 partial_text 换成「上一次」的。这条路径真实存在：
    /// 重试尝试成功进入 `ExecutingTools` 后 `finalize_tool_execution` 会清空
    /// `assistant_text`，此后同轮任何一次失败都会走到这里。而
    /// `partial_text` 喂给 [`plan_failure`] 的补条（`Session::stage_salvage`），
    /// 喂错了文本，冻结进历史的整轮内容就是错的。
    ///
    /// 只填空同时给残片补上了缺失的清除点：重试成功的路径不经过
    /// [`Self::finalize_failure`]，残片本会一直留到本轮结束。
    fn restore_truncation_salvage(&mut self) {
        if let Some(salvage) = self.truncation_salvage.take()
            && self.react_ctx.assistant_text.is_empty()
        {
            self.react_ctx.assistant_text = salvage;
        }
    }

    // ── PreparingRequest — 分叉点 ────────────────────────────────────────

    /// 准备请求并分叉到 batch 或 streaming 路径。
    async fn prepare_and_send_request(&mut self, turn: usize) {
        // 0. 瞬时重发的退避等待（可取消；无待等待 deadline 时是 no-op）。
        //    放在最前：等待期间取消 → 下面第 1 步照常收尾为 Cancelled。
        self.wait_retry_backoff().await;

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
        // 若末条消息为 User query 且本用户轮尚未解析过，表示新一轮对话开始，
        // 解析动态上下文；否则（tool 结果返回后的 ReAct 迭代、或截断重试回到
        // 同一锚点）复用已缓存的上下文。
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

        //    `dynamic_context_resolved` 挡住的是重试：回退 staging 后末条又变回
        //    user，同一 turn 的第二次解析对同一条 query 重跑一遍有副作用的召回
        //    （命中计数写库、额外 embedding），值却与上一次完全相同。
        if last_is_user
            && !self.dynamic_context_resolved
            && let Some(dc) = &self.config.dynamic_context
        {
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
            self.dynamic_context_resolved = true;
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
            let budget = self.max_output_tokens_override();
            match self
                .agent
                .generate_stream(session_messages, Some(effective_prompt), budget)
                .await
            {
                Ok(stream) => {
                    self.active_stream = Some(stream);
                    self.stream_assembler = BlockAssembler::new();
                    self.last_finish_reason = None;
                    // 重试请求已成功发出，上一轮残片作废。
                    self.truncation_salvage = None;
                    self.react_state = ReActState::Streaming;
                }
                Err(e) => {
                    error!(error = %e, "Streaming generate request failed");
                    // 瞬时类（限流/网络/5xx）→ 退避后原样重发；
                    // 永久类（Auth/额度/上下文溢出…）→ 类型化失败原因。
                    // 此处 staging 未被本次尝试写入（阶段不变量），锚点即当前长度。
                    if let AgentError::Provider(pe) = &e {
                        let classified = pe.classify();
                        if classified.kind.is_transient()
                            && self
                                .begin_transient_retry(
                                    turn,
                                    self.session.staging_checkpoint(),
                                    classified.kind.as_str(),
                                )
                                .await
                        {
                            return;
                        }
                        self.failure_reason = Some(model_failure_reason(pe, self.total_attempts()));
                    } else {
                        self.failure_reason = Some(TurnFailureReason::Other(format!(
                            "Streaming request failed: {e}"
                        )));
                    }
                    self.react_state = ReActState::Failed;
                }
            }
        } else {
            // ── Batch 路径 ──
            let tools = self.agent.tool_executor().definitions();
            let budget = self.max_output_tokens_override();
            match self
                .agent
                .generate_with_tools(session_messages, Some(effective_prompt), tools, budget)
                .await
            {
                Ok(response) => {
                    self.react_ctx.batch_response = Some(response);
                    // 重试请求已成功发出，上一轮残片作废。
                    self.truncation_salvage = None;
                    self.react_state = ReActState::ResolvingResponse;
                }
                Err(e) => {
                    error!(error = %e, "Batch generate request failed");
                    // 同流式路径：瞬时类退避重发，永久类类型化失败。
                    if let AgentError::Provider(pe) = &e {
                        let classified = pe.classify();
                        if classified.kind.is_transient()
                            && self
                                .begin_transient_retry(
                                    turn,
                                    self.session.staging_checkpoint(),
                                    classified.kind.as_str(),
                                )
                                .await
                        {
                            return;
                        }
                        self.failure_reason = Some(model_failure_reason(pe, self.total_attempts()));
                    } else {
                        self.failure_reason = Some(TurnFailureReason::Other(format!(
                            "Generate request failed: {e}"
                        )));
                    }
                    self.react_state = ReActState::Failed;
                }
            }
        }
    }

    // ── Batch 分支：ResolvingResponse ─────────────────────────────────────

    /// 解析 batch 响应：提取内容、更新 usage、写入 staging、判断下一步。
    async fn resolve_batch_response(&mut self, turn: usize) {
        // ★ 截断重试的回退锚点。必须在 `stage_output_blocks` **之前**取 ——
        //   之后 staging 就带上本次调用的产出了。此刻的值恒等于发请求前的长度：
        //   从 `PreparingRequest` 到这里没有任何 staging 写入。
        let checkpoint = self.session.staging_checkpoint();

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
            // ★ 截断重试，判据与流式路径完全一致：`Incomplete` 且
            //   `FinishReason::MaxTokens`。`GenerateResult::finish_reason` 由适配器
            //   从上游原样映射（chat 的 `length` / responses 的
            //   `incomplete_details.reason`），因此这里能区分「抬预算能救」与
            //   「救了也没用」（`content_filter` 等），不会为后者白烧一次调用。
            //   上游未提供该信息时为 `None` —— 不臆测，不重试。
            if response.status == ResponseStatus::Incomplete
                && matches!(response.finish_reason, Some(FinishReason::MaxTokens))
                && self
                    .begin_truncation_retry(turn, response.usage.output_tokens, checkpoint)
                    .await
            {
                return;
            }

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
                finish_reason = response.finish_reason.map_or("-", |r| r.as_str()),
                message = %msg,
                "Batch model response ended with non-completed status"
            );
            // 内容过滤单独归类 — 与「网络故障」在用户面前必须是两句话，
            // 且它明确不可重试（抬预算/退避都救不了）。
            self.failure_reason = Some(
                if matches!(response.finish_reason, Some(FinishReason::ContentFilter)) {
                    TurnFailureReason::ContentFiltered { message: msg }
                } else {
                    TurnFailureReason::Other(msg)
                },
            );
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
                self.active_stream = None;
                // 流式路径的主分类 choke point：HTTP 非 200（校验阶段 yield Err）、
                // 流内 error payload、established-stream 传输中断都从这里进来。
                // 瞬时类回退 staging 后退避重发（chunk 不碰 staging，锚点即当前长度；
                // react_ctx 里的增量由 rollback_attempt 残片化）；永久类类型化失败。
                let classified = e.classify();
                if classified.kind.is_transient()
                    && self
                        .begin_transient_retry(
                            turn,
                            self.session.staging_checkpoint(),
                            classified.kind.as_str(),
                        )
                        .await
                {
                    return;
                }
                self.failure_reason = Some(model_failure_reason(&e, self.total_attempts()));
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
        // ★ 截断重试的回退锚点。必须在 `stage_output_blocks` **之前**取 ——
        //   之后 staging 就带上本次调用的产出了。流式 chunk 处理只发事件、
        //   写 `react_ctx`、记账 usage，不碰 staging，故此处恒等于发请求前的长度。
        let checkpoint = self.session.staging_checkpoint();

        self.active_stream = None;
        let assembler = std::mem::take(&mut self.stream_assembler);
        let (blocks, usage, status, error) = assembler.finish();
        let finish_reason = self.last_finish_reason.take();

        // 回填 InputItem 到 session staging，并更新 assistant_text / assistant_reasoning
        // （失败分支也先回填，使 Failed 结果携带 partial_text）。
        self.stage_output_blocks(&blocks);

        // 状态收敛：Failed（Aborted / Error / ContentFilter）与 Incomplete
        //（截断 / 流被掐断）都视为异常终止
        if status != ResponseStatus::Completed {
            // ★ 截断重试。只认 `MaxTokens`：截断是「输出预算不够」，抬预算重发一次
            //   比把整轮判死（冻结 staging + turn_index += 1）划算。
            //   `Aborted` / `Error` / `ContentFilter` 抬预算救不回来，
            //   重试只会白烧调用。
            if status == ResponseStatus::Incomplete
                && matches!(finish_reason, Some(FinishReason::MaxTokens))
                && self
                    .begin_truncation_retry(
                        self.session.turn_index(),
                        usage.output_tokens,
                        checkpoint,
                    )
                    .await
            {
                return;
            }

            // ★ 流被掐断（无 Finish 收尾 + 未闭合块 → Incomplete + finish_reason
            //   为 None）：传输层放弃后上抛的「上游中途消失」，是瞬时故障，
            //   回退后退避重发比判死划算。`stage_output_blocks` 已写入 staging，
            //   `checkpoint` 在它之前取好 —— 与截断重试同一锚点语义。
            if status == ResponseStatus::Incomplete
                && finish_reason.is_none()
                && self
                    .begin_transient_retry(self.session.turn_index(), checkpoint, "stream_cut")
                    .await
            {
                return;
            }

            let msg = error.map(|e| e.message).unwrap_or_else(|| match status {
                ResponseStatus::Incomplete => {
                    "model response was truncated (incomplete)".to_string()
                }
                _ => "model response failed".to_string(),
            });
            // 归因字段必须齐全：status 只说「非正常结束」，finish_reason 才区分
            // 截断（max_tokens）、内容过滤与上游异常（aborted/error）；
            // output_tokens 用来判断截断是否真的顶到了输出上限；
            // block_kinds 说明收到的到底是哪些块。
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
            self.failure_reason = Some(
                if matches!(finish_reason, Some(FinishReason::ContentFilter)) {
                    TurnFailureReason::ContentFiltered { message: msg }
                } else if status == ResponseStatus::Incomplete && finish_reason.is_none() {
                    // 瞬时重发已耗尽或不可用 — 上游中途消失的终局归因
                    TurnFailureReason::ModelUnavailable {
                        attempts: self.total_attempts(),
                        message: msg,
                    }
                } else {
                    TurnFailureReason::Other(msg)
                },
            );
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
        // 截断重试的残片到此作废：能走到这里说明**重试之后那次尝试**已经成功
        // 并产出了工具调用，残片要兜底的那个场景已经过去了。不清的话它会一直
        // 留到本轮结束，而本轮任何一次「什么都没吐出来」的失败（请求发不出去）
        // 都会因 `assistant_text` 恰好为空而被它顶上 —— 收尾拿到的
        // `partial_text` 于是是上一次尝试的文本（CLI 会把它再打印一遍）。
        self.truncation_salvage = None;
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
        // ★ 残片的唯一归还点。截断重试把上一次尝试的文本移进了 salvage，此后
        //   任何一条失败路径（请求发不出去、流中途出错、取消、超时、hook 中止）
        //   都要把它还给 `react_ctx`，否则 `plan_failure` 取到的 partial_text 为空。
        //   挂在收尾漏斗上而不是各 Err 分支，是为了让后续新增的失败路径不会漏掉。
        self.restore_truncation_salvage();
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
            frozen_staging_messages = plan.frozen_staging_messages,
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
                    self.reset_turn_counters();
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
    /// 冻结前的部分文本长度
    /// 与冻结前的 `frozen_staging_messages` 并列， 让日志能回答「这轮白跑了多少」
    partial_text_len: usize,
    /// 落盘产物，`persist_on_failure` 门控。
    /// `false` 时冻结照常（内存）但不产出快照（CLI 路径，`NullSessionPersister`）。
    snapshot: Option<SessionSnapshot>,
    /// 冻结前 staging 中的消息数（合成 Output 与中断说明尚未追加）。
    frozen_staging_messages: usize,
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
        TurnFailureReason::RateLimited { attempts, message } => {
            format!(
                "rate limited after {attempts} attempts: {}",
                label_msg(message)
            )
        }
        TurnFailureReason::ModelUnavailable { attempts, message } => {
            format!(
                "model unavailable after {attempts} attempts: {}",
                label_msg(message)
            )
        }
        TurnFailureReason::AuthError { message } => {
            format!("auth error: {}", label_msg(message))
        }
        TurnFailureReason::QuotaExhausted { message } => {
            format!("quota exhausted: {}", label_msg(message))
        }
        TurnFailureReason::ContextOverflow { message } => {
            format!("context window exceeded: {}", label_msg(message))
        }
        TurnFailureReason::ContentFiltered { message } => {
            format!("output blocked by content filter: {}", label_msg(message))
        }
        TurnFailureReason::Other(detail) => format!("failed: {detail}"),
    }
}

/// 失败标签里的原始 msg 摘要上限 — 标签会进模型上下文，body 可能很长。
const FAILURE_LABEL_MSG_MAX: usize = 200;

/// 按字符边界截断失败标签附带的原始 msg（多字节安全）。
fn label_msg(message: &str) -> String {
    if message.chars().count() <= FAILURE_LABEL_MSG_MAX {
        return message.to_string();
    }
    message.chars().take(FAILURE_LABEL_MSG_MAX).collect()
}

/// 把已分类的 [`ProviderError`] 映射为类型化失败原因。
///
/// `attempts` = 实际发起的尝试总数（含触发失败的那次），仅瞬时类变体使用。
/// `ClassifiedError.message` 原样带进变体 — 分类不吞原文，SSE error 文案
/// 与 session 中断标签都靠它保留 provider 诊断信息。
///
/// `AgentError` 的非 Provider 变体（Io/Config/MaxTurns…）不走本函数，
/// 由各自调用点直接构造原因。
fn model_failure_reason(err: &model_provider::ProviderError, attempts: u32) -> TurnFailureReason {
    use model_provider::ApiErrorKind;

    let classified = err.classify();
    let message = classified.message;
    match classified.kind {
        ApiErrorKind::RateLimited => TurnFailureReason::RateLimited { attempts, message },
        ApiErrorKind::Network | ApiErrorKind::Server => {
            TurnFailureReason::ModelUnavailable { attempts, message }
        }
        ApiErrorKind::Auth => TurnFailureReason::AuthError { message },
        ApiErrorKind::QuotaExhausted => TurnFailureReason::QuotaExhausted { message },
        ApiErrorKind::ContextOverflow => TurnFailureReason::ContextOverflow { message },
        ApiErrorKind::ContentFiltered => TurnFailureReason::ContentFiltered { message },
        // NotFound / InvalidRequest / Unknown / 未来新增 kind：保留原始 msg 走 Other
        _ => TurnFailureReason::Other(message),
    }
}

/// 无 await，全部 session 变更在此完成。
///
/// **不变量：本函数全程禁止 `.await`。** 正因为是纯同步，「冻结」对取消免疫 ——
/// 若 `run()` 被 abort，最多丢事件与落盘，绝不会出现「落了盘但 committed 里
/// 没有这一轮」。严防后人往这段塞 `.await`。
///
/// 步骤顺序：① 守卫 → ② 取标量 → ③ 补回被重试回退的部分文本 → ④ 冲刷已完成
/// 的工具结果 → ⑤ 冻结 → ⑥ 快照 → ⑦ 清状态。③ 在 ⑤ 之前是硬要求：
/// 它决定 `interrupt_turn` 是冻结还是退化成 `rollback_turn`。④ 在 ⑤ 之前
/// 同样是硬要求：捞回来的工具结果必须和这一轮一起进 committed，且 `stage_item`
/// 只在 `Active` 下可用（⑤ 之后就 `Idle` 了）。
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

    // ③ 补回被截断重试回退掉的部分文本。回退后 staging 只剩 `user_input`，
    //    `interrupt_turn` 会因此退化成 `rollback_turn`，这一轮（含用户提问）
    //    整轮丢失 —— 比不重试还差。补一条 assistant 消息让冻结路径照常成立；
    //    没有残片（非重试路径）时是 no-op，行为逐字节不变。
    if session.staging_messages().is_empty() && !partial_text.is_empty() {
        session.stage_salvage(partial_text.clone());
    }

    let frozen_staging_messages = session.staging_messages().len(); // ④ 冲刷后会变

    // ④ 冲刷 poll 阶段已捞回的工具结果（只写不清理；见 `stage_tool_results`）。
    //    必须在 ⑤ 之前 —— 这些结果是「这一轮已完成的成果」，要和本轮一起冻结。
    stage_tool_results(session, &ctx.pending_tool_calls);

    // ⑤ 冻结在途轮（内部补齐悬空 tool_call 并追加中断说明）
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

    // ⑥ token 之后才 snapshot —— 顺序不可颠倒，否则快照里没有这一轮
    let snapshot = match (&token, persist) {
        (Some(t), true) => Some(session.snapshot(t)),
        _ => None,
    };

    // ⑦ 清 ctx —— pending_tool_calls 必须清，否则 drain_pending 时下一轮的
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
        frozen_staging_messages,
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
        assert_eq!(config.truncation_retry_limit, 1);
        assert_eq!(config.truncation_retry_min_budget, 32_768);
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
        assert_eq!(plan.frozen_staging_messages, 1);

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

    // ── 截断重试 ────────────────────────────────────────────────────────

    /// 一次模型调用的脚本：吐一串 chunk、直接给一个非流式响应，或者直接失败。
    enum Script {
        Chunks(Vec<StreamChunk>),
        /// 流中途 yield `Err` —— 模拟 established-stream 中断 / 流内错误 payload。
        ChunksWithErr(Vec<Result<StreamChunk, model_provider::ProviderError>>),
        Batch(Box<model_provider::GenerateResult>),
        Fail(model_provider::ProviderError),
    }

    /// 按调用顺序弹出脚本的 provider，并记录每次请求的输出预算。
    ///
    /// 脚本与调用方式必须配对：流式路径只能拿到 `Chunks`、批量路径只能拿到
    /// `Batch`，配错即测试自身写错，故宁可直接 panic 也不静默降级。
    struct ScriptedStreamProvider {
        scripts: std::sync::Mutex<std::collections::VecDeque<Script>>,
        budgets: std::sync::Mutex<Vec<Option<u32>>>,
    }

    impl ScriptedStreamProvider {
        fn new(scripts: Vec<Script>) -> Self {
            Self {
                scripts: std::sync::Mutex::new(scripts.into()),
                budgets: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// 每次请求实际收到的输出预算（`None` = 未指定，走服务端默认）。
        fn budgets(&self) -> Vec<Option<u32>> {
            self.budgets.lock().unwrap().clone()
        }

        fn next_script(&self) -> Script {
            self.scripts
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted provider ran out of scripts")
        }
    }

    #[async_trait::async_trait]
    impl model_provider::ModelProvider for ScriptedStreamProvider {
        fn name(&self) -> &str {
            "scripted-stream"
        }

        async fn generate_full(
            &self,
            request: &model_provider::GenerateRequest,
        ) -> Result<model_provider::GenerateResult, model_provider::ProviderError> {
            self.budgets.lock().unwrap().push(request.max_output_tokens);
            match self.next_script() {
                Script::Batch(r) => Ok(*r),
                Script::Fail(e) => Err(e),
                Script::Chunks(_) | Script::ChunksWithErr(_) => {
                    panic!("batch path received a streaming script; test harness misconfigured")
                }
            }
        }

        async fn generate_stream(
            &self,
            request: &model_provider::GenerateRequest,
        ) -> Result<GenerateStream, model_provider::ProviderError> {
            self.budgets.lock().unwrap().push(request.max_output_tokens);
            match self.next_script() {
                Script::Fail(e) => Err(e),
                Script::Chunks(chunks) => Ok(GenerateStream::new(Box::pin(futures::stream::iter(
                    chunks.into_iter().map(Ok),
                )))),
                Script::ChunksWithErr(chunks) => {
                    Ok(GenerateStream::new(Box::pin(futures::stream::iter(chunks))))
                }
                Script::Batch(_) => {
                    panic!("streaming path received a batch script; test harness misconfigured")
                }
            }
        }
    }

    /// 非流式响应：`status` / `finish_reason` 由调用方指定，用来钉住批量路径的判据。
    fn batch_response(
        text: &str,
        output_tokens: u32,
        status: model_provider::ResponseStatus,
        finish_reason: Option<FinishReason>,
    ) -> Script {
        Script::Batch(Box::new(model_provider::GenerateResult {
            id: "r".to_string(),
            output: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            usage: Usage {
                input_tokens: 10,
                output_tokens,
                total_tokens: 10 + output_tokens,
            },
            status,
            finish_reason,
            error: None,
        }))
    }

    /// 一段可见文本 + 收敛原因。`output_tokens` 用来模拟顶到预算。
    fn text_chunks(text: &str, output_tokens: u32, reason: FinishReason) -> Script {
        Script::Chunks(vec![
            StreamChunk::BlockStart {
                index: 0,
                block_type: model_provider::BlockType::Text,
            },
            StreamChunk::TextDelta {
                index: 0,
                delta: text.to_string(),
            },
            StreamChunk::BlockEnd {
                index: 0,
                block: ContentBlock::Text {
                    text: text.to_string(),
                },
            },
            StreamChunk::Usage {
                usage: Usage {
                    input_tokens: 10,
                    output_tokens,
                    total_tokens: 10 + output_tokens,
                },
            },
            StreamChunk::Finish { reason },
        ])
    }

    /// 被截断的那次：文本落了块，但没有 `Stop`。
    fn truncated(text: &str) -> Script {
        text_chunks(text, 4096, FinishReason::MaxTokens)
    }

    /// 正常收尾的那次。
    fn completed(text: &str) -> Script {
        text_chunks(text, 20, FinishReason::Stop)
    }

    /// 截断重试测试的 looper 及其通道端点（保活语义同 [`FailureHarness`]）。
    struct RetryHarness {
        looper: AgentLooper,
        _user_speaker: Speaker<UserMsg>,
        user_listener: Option<Listener<UserMsg>>,
        provider: Arc<ScriptedStreamProvider>,
        events: Listener<LooperEvent>,
        /// 与 looper 共享的取消标志 — 测试用来在退避等待期间触发取消。
        cancel_flag: Arc<AtomicBool>,
    }

    impl RetryHarness {
        fn committed_turns(&self) -> usize {
            self.looper.session().committed_turns().len()
        }

        /// 收齐本轮事件里的 `TurnComplete` 结果。
        fn turn_outcomes(&mut self) -> Vec<TurnOutcome> {
            let mut out = Vec::new();
            while let Ok(ev) = self.events.try_recv() {
                if let LooperEvent::TurnComplete { outcome, .. } = ev {
                    out.push(outcome);
                }
            }
            out
        }

        /// 原样排空事件通道 —— 需要断言事件**顺序**（如通知早于重试的增量）时用。
        ///
        /// 与 [`Self::turn_outcomes`] 互斥：两者都在消费同一个 listener，
        /// 先 drain 的拿走全部。
        fn drain_events(&mut self) -> Vec<LooperEvent> {
            let mut out = Vec::new();
            while let Ok(ev) = self.events.try_recv() {
                out.push(ev);
            }
            out
        }
    }

    /// 取出 `events` 里全部 `TruncationRetry` 通知，连同它们在序列中的下标。
    fn truncation_notices(events: &[LooperEvent]) -> Vec<(usize, u32, u32, u32, u32, String)> {
        events
            .iter()
            .enumerate()
            .filter_map(|(i, ev)| match ev {
                LooperEvent::TruncationRetry {
                    attempt,
                    limit,
                    output_tokens,
                    retry_budget,
                    discarded_text,
                    ..
                } => Some((
                    i,
                    *attempt,
                    *limit,
                    *output_tokens,
                    *retry_budget,
                    discarded_text.clone(),
                )),
                _ => None,
            })
            .collect()
    }

    /// `events` 里全部 `TextDelta` 原文首尾相接 —— 即客户端此刻累积到的内容。
    fn streamed_text(events: &[LooperEvent]) -> String {
        events
            .iter()
            .filter_map(|ev| match ev {
                LooperEvent::TextDelta { delta } => Some(delta.as_str()),
                _ => None,
            })
            .collect()
    }

    /// 构造截断重试测试用的 looper：流式 provider + 指定的重试配置。
    fn retry_harness(scripts: Vec<Script>, config: LooperConfig) -> RetryHarness {
        retry_harness_with_budget(scripts, config, None)
    }

    /// 同上，但可指定 `ModelConfig::max_tokens`（用于「预算已高于下限」的守卫用例）。
    fn retry_harness_with_budget(
        scripts: Vec<Script>,
        config: LooperConfig,
        max_tokens: Option<u32>,
    ) -> RetryHarness {
        retry_harness_inner(scripts, config, max_tokens, true)
    }

    /// 走**批量**路径的 harness（`ModelConfig::stream = false`）。
    ///
    /// 批量路径的重试判据与流式不同（流式有 `FinishReason`，批量要靠
    /// `GenerateResult::finish_reason`），需要各自的用例钉住。
    fn batch_retry_harness(scripts: Vec<Script>, config: LooperConfig) -> RetryHarness {
        retry_harness_inner(scripts, config, None, false)
    }

    fn retry_harness_inner(
        scripts: Vec<Script>,
        config: LooperConfig,
        max_tokens: Option<u32>,
        stream: bool,
    ) -> RetryHarness {
        let profile: crate::agent::AgentProfile = serde_yaml::from_str(
            "agent:\n  name: t\n  description: d\nllm:\n  provider: p\n  model: m\n",
        )
        .unwrap();
        let executor: Arc<dyn crate::tools::ToolExecutor> =
            Arc::new(crate::tools::DefaultToolsExecutor::new(Vec::new()));
        let provider = Arc::new(ScriptedStreamProvider::new(scripts));
        let mut builder = crate::agent::ModelConfigBuilder::new().stream(stream);
        if let Some(m) = max_tokens {
            builder = builder.max_tokens(m);
        }
        let agent = Arc::new(Agent::from_parts(
            std::path::PathBuf::from("/tmp/agent.md"),
            profile,
            "sys".to_string(),
            Arc::clone(&provider) as Arc<dyn model_provider::ModelProvider>,
            builder.build(),
            Arc::clone(&executor),
            Arc::new(crate::mcp::McpManager::empty(executor)),
            None,
        ));

        let session = Session::new("s1".to_string(), "d".to_string());

        let (looper_side, caller_side) = make_async_intercom_pair::<LooperEvent, UserMsg>(64);
        let (event_speaker, user_listener) = looper_side.split();
        let (user_speaker, event_listener) = caller_side.split();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let looper = AgentLooper::new(
            agent,
            Box::new(session),
            event_speaker,
            Arc::clone(&cancel_flag),
            Arc::new(AtomicBool::new(false)),
            config,
            Arc::new(crate::persistence::NullSessionPersister),
        );

        RetryHarness {
            looper,
            _user_speaker: user_speaker,
            user_listener: Some(user_listener),
            provider,
            events: event_listener,
            cancel_flag,
        }
    }

    /// 送一句 query 并驱动到本轮结束，然后停靠回 Idle。
    ///
    /// `run()` 正常完成会停靠等输入而不退出，故以超时结束 —— 与
    /// [`test_non_cancel_failure_docks_at_idle`] 同法，超时即「已收敛」。
    async fn drive_query(h: &mut RetryHarness) {
        let listener = h.user_listener.take().expect("harness 持有 listener");
        h._user_speaker
            .send(UserMsg::Query("hi".into()))
            .await
            .expect("looper 侧仍在监听");
        let _ = tokio::time::timeout(Duration::from_secs(5), h.looper.run(listener)).await;
    }

    #[tokio::test]
    async fn test_truncation_retry_resends_with_raised_budget() {
        let mut h = retry_harness(
            vec![truncated("part"), completed("done")],
            LooperConfig {
                truncation_retry_limit: 1,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
        );

        drive_query(&mut h).await;

        assert_eq!(
            h.provider.budgets(),
            vec![None, Some(32_768)],
            "首次用配置预算；重试必须带上抬高的预算"
        );
        let outcomes = h.turn_outcomes();
        assert_eq!(outcomes.len(), 1, "必须恰好收尾一次");
        match &outcomes[0] {
            TurnOutcome::Success { text, .. } => {
                assert_eq!(text, "done", "截断那次的文本不得残留");
            }
            other => panic!("重试成功应收敛为 Success，实际 {other:?}"),
        }
        assert_eq!(h.committed_turns(), 1, "重试在同一轮内完成，只提交一轮");
        assert_eq!(h.looper.session().turn_index(), 1);
        assert!(
            h.looper.session().staging_messages().is_empty(),
            "轮次已提交，staging 必须已清空"
        );
    }

    #[tokio::test]
    async fn test_truncation_retry_exhausted_falls_back_to_failed() {
        let mut h = retry_harness(
            vec![truncated("first"), truncated("second")],
            LooperConfig {
                truncation_retry_limit: 1,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
        );

        drive_query(&mut h).await;

        assert_eq!(
            h.provider.budgets().len(),
            2,
            "limit=1 恰好重试一次，不得继续重试"
        );
        let outcomes = h.turn_outcomes();
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            TurnOutcome::Failed { reason, .. } => {
                let msg = format!("{reason:?}");
                assert!(msg.contains("truncated"), "失败原因应指向截断；实际 {msg}");
            }
            other => panic!("重试耗尽应回落为 Failed，实际 {other:?}"),
        }
        assert_eq!(h.committed_turns(), 1, "重试耗尽后仍冻结入史，与改动前一致");
    }

    #[tokio::test]
    async fn test_truncation_retry_disabled_by_zero_limit() {
        let mut h = retry_harness(
            vec![truncated("part")],
            LooperConfig {
                truncation_retry_limit: 0,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
        );

        drive_query(&mut h).await;

        assert_eq!(h.provider.budgets().len(), 1, "limit=0 必须完全关闭重试");
        assert!(matches!(
            h.turn_outcomes().as_slice(),
            [TurnOutcome::Failed { .. }]
        ));
    }

    #[tokio::test]
    async fn test_truncation_retry_skipped_when_budget_already_above_floor() {
        // 抬不动预算 = 重试请求逐字节不变、必然同样截断，白烧一次调用。
        let mut h = retry_harness_with_budget(
            vec![truncated("part")],
            LooperConfig {
                truncation_retry_limit: 1,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
            Some(40_000),
        );

        drive_query(&mut h).await;

        assert_eq!(
            h.provider.budgets(),
            vec![Some(40_000)],
            "预算已高于下限则不重试，且首次请求就带上配置值"
        );
        assert!(matches!(
            h.turn_outcomes().as_slice(),
            [TurnOutcome::Failed { .. }]
        ));
    }

    #[tokio::test]
    async fn test_truncation_retry_ignores_non_max_tokens_status() {
        // Aborted → status Failed（不是 Incomplete），抬预算救不回来。
        let mut h = retry_harness(
            vec![text_chunks("cut", 100, FinishReason::Aborted)],
            LooperConfig {
                truncation_retry_limit: 1,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
        );

        drive_query(&mut h).await;

        assert_eq!(h.provider.budgets().len(), 1, "Aborted 不得触发重试");
        assert!(matches!(
            h.turn_outcomes().as_slice(),
            [TurnOutcome::Failed { .. }]
        ));
    }

    #[tokio::test]
    async fn test_truncation_retry_salvages_partial_text_on_request_error() {
        // 抬高的预算若超过模型真实上限，重试请求本身会失败（400）。
        // 此时 staging 已被回退为空，若不归还残片，partial_text 会丢光；
        // 不补回 staging，interrupt_turn 还会退化成 rollback，整轮进不了历史。
        // 本用例钉住「不劣于现状」的完整含义。
        let mut h = retry_harness(
            vec![
                truncated("salvaged"),
                Script::Fail(model_provider::ProviderError::Api {
                    status: 400,
                    body: "max_output_tokens too large".to_string(),
                }),
            ],
            LooperConfig {
                truncation_retry_limit: 1,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
        );

        drive_query(&mut h).await;

        assert_eq!(h.provider.budgets(), vec![None, Some(32_768)]);
        let outcomes = h.turn_outcomes();
        match &outcomes[0] {
            TurnOutcome::Failed { partial_text, .. } => {
                assert_eq!(partial_text, "salvaged", "重试请求失败时必须归还截断残片");
            }
            other => panic!("应收敛为 Failed，实际 {other:?}"),
        }
        assert!(h.looper.session().is_idle(), "收尾后必须回到 Idle");
        // 回退掉的唯一产物必须补回 staging，否则 interrupt_turn 退化成 rollback，
        // 这一轮连用户提问一起从历史里消失。
        assert_eq!(
            h.committed_turns(),
            1,
            "重试失败仍须冻结整轮，不得退化成 rollback"
        );
        assert!(
            h.looper
                .session()
                .committed_turns()
                .iter()
                .flatten()
                .any(|am| matches!(
                    am.message.as_ref(),
                    InputItem::Message { content, .. } if content.text_view() == "salvaged"
                )),
            "冻结进历史的轮次里必须留下截断那次的文本"
        );
    }

    #[tokio::test]
    async fn test_can_retry_truncation_no_headroom_is_false() {
        // 没有下一次模型调用额度时不重试：否则状态机刚被置回 PreparingRequest
        // 就撞上 MaxTurnsExceeded，把「截断」这个真实原因换掉。
        let mut h = retry_harness(
            vec![],
            LooperConfig {
                truncation_retry_limit: 1,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
        );
        h.looper.react_loop_iteration = h.looper.max_turns;
        assert!(!h.looper.can_retry_truncation(), "无轮数余量必须不重试");
    }

    #[test]
    fn test_max_output_tokens_override_is_sticky_within_turn() {
        // 粘性：本轮发生过重试后，后续 ReAct 迭代继续用抬高的预算，
        // 否则 tool 调用后的下一次迭代可能同样截断。
        let mut h = retry_harness(
            vec![],
            LooperConfig {
                truncation_retry_limit: 1,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
        );
        assert_eq!(h.looper.max_output_tokens_override(), None);

        h.looper.truncation_retries_used = 1;
        assert_eq!(h.looper.max_output_tokens_override(), Some(32_768));

        // 计数器随新用户轮归零，粘性不出轮
        h.looper.reset_turn_counters();
        assert_eq!(h.looper.max_output_tokens_override(), None);
    }

    #[test]
    fn test_can_retry_truncation_false_once_budget_already_raised() {
        // limit >= 2 时，第二次重试的请求与第一次逐字节相同（预算已是抬高值），
        // 守卫必须挡住 —— 否则白烧一次调用和一次 react_loop_iteration。
        // 守卫因此比的是当前生效预算，不是配置值。
        let mut h = retry_harness_with_budget(
            vec![],
            LooperConfig {
                truncation_retry_limit: 3,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
            Some(4096),
        );

        assert!(
            h.looper.can_retry_truncation(),
            "首次：4096 < 32768，抬得动"
        );

        h.looper.truncation_retries_used = 1;
        assert!(
            !h.looper.can_retry_truncation(),
            "生效预算已是 32768，再重试只会发出同样的请求"
        );
    }

    #[tokio::test]
    async fn test_batch_truncation_retry_resends_with_raised_budget() {
        let retry_cfg = LooperConfig {
            truncation_retry_limit: 1,
            truncation_retry_min_budget: 32_768,
            ..Default::default()
        };
        let mut h = batch_retry_harness(
            vec![
                batch_response(
                    "part",
                    4096,
                    model_provider::ResponseStatus::Incomplete,
                    Some(FinishReason::MaxTokens),
                ),
                batch_response(
                    "done",
                    20,
                    model_provider::ResponseStatus::Completed,
                    Some(FinishReason::Stop),
                ),
            ],
            retry_cfg,
        );

        drive_query(&mut h).await;

        assert_eq!(h.provider.budgets(), vec![None, Some(32_768)]);
        match &h.turn_outcomes()[0] {
            TurnOutcome::Success { text, .. } => assert_eq!(text, "done"),
            other => panic!("重试成功应收敛为 Success，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_batch_content_filter_does_not_retry() {
        // 收窄前的判据是裸 `Incomplete`，content_filter 也会被当成截断重发一次
        // —— 抬预算救不回来，纯白烧一次调用。`GenerateResult::finish_reason`
        // 让批量路径能做出与流式路径同样的区分。
        let mut h = batch_retry_harness(
            vec![batch_response(
                "filtered",
                12,
                model_provider::ResponseStatus::Incomplete,
                Some(FinishReason::Error),
            )],
            LooperConfig {
                truncation_retry_limit: 1,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
        );

        drive_query(&mut h).await;

        assert_eq!(h.provider.budgets().len(), 1, "content_filter 不得触发重试");
        assert!(matches!(
            h.turn_outcomes().as_slice(),
            [TurnOutcome::Failed { .. }]
        ));
    }

    #[tokio::test]
    async fn test_batch_incomplete_without_finish_reason_does_not_retry() {
        // 上游没给原因时不臆测成截断 —— 抬预算未必救得了，重试代价却是确定的。
        let mut h = batch_retry_harness(
            vec![batch_response(
                "unknown",
                12,
                model_provider::ResponseStatus::Incomplete,
                None,
            )],
            LooperConfig {
                truncation_retry_limit: 1,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
        );

        drive_query(&mut h).await;

        assert_eq!(h.provider.budgets().len(), 1, "原因未知时不得重试");
    }

    #[tokio::test]
    async fn test_truncation_retry_emits_notice_before_retried_text() {
        let mut h = retry_harness(
            vec![truncated("part"), completed("done")],
            LooperConfig {
                truncation_retry_limit: 1,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
        );

        drive_query(&mut h).await;
        let events = h.drain_events();

        // ── 通知本身：恰好一条，四个数字取自实际请求 ──
        let notices = truncation_notices(&events);
        assert_eq!(notices.len(), 1, "limit=1 恰好一条通知");
        let (notice_at, attempt, limit, output_tokens, retry_budget, discarded) = &notices[0];
        assert_eq!(*attempt, 1);
        assert_eq!(*limit, 1);
        assert_eq!(*output_tokens, 4096, "截断那次的输出 token 数");
        assert_eq!(*retry_budget, 32_768, "抬升后的预算");
        assert_eq!(discarded, "part", "载荷是被丢弃那次尝试的正文");

        // ── 顺序：通知必须早于重试尝试的第一条增量 ──
        // 前端据此把横幅插在残句与新正文之间；晚于增量就会插到答案下面。
        let second_delta_at = events
            .iter()
            .enumerate()
            .filter(|(_, ev)| matches!(ev, LooperEvent::TextDelta { .. }))
            .nth(1)
            .map(|(i, _)| i)
            .expect("重试尝试必须发出第二条 TextDelta");
        assert!(
            *notice_at < second_delta_at,
            "通知（下标 {notice_at}）必须早于重试的增量（下标 {second_delta_at}）"
        );

        // ── 落库侧的剥离不变量 ──
        // 通知到来这一刻，累加器必然**以载荷结尾** —— 该次尝试的增量是最后
        // 追加进累加器的内容，之后到通知之间不可能再有别的 delta（之间只隔
        // 一个 Finish / Usage 块）。落库侧按后缀剥离依赖的正是这条。
        //
        // 注意残句在**最终**画面里是前缀（后面还跟着重试的正文），所以剥离
        // 只能发生在通知这一刻，不能等到轮次结束再对着最终文本做。
        let up_to_notice = streamed_text(&events[..*notice_at]);
        assert_eq!(up_to_notice, "part");
        assert!(
            up_to_notice.ends_with(discarded.as_str()),
            "剥离不变量：通知到达时累加器以载荷结尾"
        );

        // ── 取舍的显式记录：客户端拿到的增量串是「残句 + 新正文」──
        // 本设计**不**在传输层删除已下发的增量，靠通知解释这段残句。
        // 断言最终串，避免将来有人「顺手」在 core 侧把残句吞掉而测试仍全绿。
        assert_eq!(streamed_text(&events), "partdone");
    }

    #[tokio::test]
    async fn test_truncation_retry_notice_carries_reasoning_only_attempt() {
        // 正文为空、只有推理被吐出来的那次尝试同样要通知：画面上确实多了一段
        // 属于已作废尝试的推理内容。载荷正文为空，落库侧剥离是空操作。
        let mut h = retry_harness(
            vec![
                Script::Chunks(vec![
                    StreamChunk::BlockStart {
                        index: 0,
                        block_type: model_provider::BlockType::Reasoning,
                    },
                    StreamChunk::ReasoningDelta {
                        index: 0,
                        delta: "想想".to_string(),
                    },
                    StreamChunk::BlockEnd {
                        index: 0,
                        block: ContentBlock::Reasoning {
                            text: "想想".to_string(),
                        },
                    },
                    StreamChunk::Finish {
                        reason: FinishReason::MaxTokens,
                    },
                ]),
                completed("done"),
            ],
            LooperConfig {
                truncation_retry_limit: 1,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
        );

        drive_query(&mut h).await;
        let events = h.drain_events();

        let notices = truncation_notices(&events);
        assert_eq!(notices.len(), 1, "只有推理产出也要通知");
        assert_eq!(notices[0].5, "", "载荷正文为空");
    }

    #[tokio::test]
    async fn test_truncation_retry_notice_absent_on_zero_output() {
        // 截断但什么都没吐：没有需要解释的残句，发出去只会让前端插一条
        // 无上下文的横幅。重试本身照常进行。
        let mut h = retry_harness(
            vec![truncated(""), completed("done")],
            LooperConfig {
                truncation_retry_limit: 1,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
        );

        drive_query(&mut h).await;
        let events = h.drain_events();

        assert!(truncation_notices(&events).is_empty(), "零产出不得发通知");
        assert_eq!(
            h.provider.budgets().len(),
            2,
            "零产出仍要重试 —— 不发通知只是不发通知"
        );
    }

    #[tokio::test]
    async fn test_batch_truncation_retry_emits_notice() {
        // 批量路径不发 delta，客户端没有残句可解释；通知照发（事件陈述的是
        // 「本次尝试作废」这个事实），落库侧的载荷是该次尝试的整段文本。
        let mut h = batch_retry_harness(
            vec![
                batch_response(
                    "part",
                    4096,
                    model_provider::ResponseStatus::Incomplete,
                    Some(FinishReason::MaxTokens),
                ),
                batch_response(
                    "done",
                    20,
                    model_provider::ResponseStatus::Completed,
                    Some(FinishReason::Stop),
                ),
            ],
            LooperConfig {
                truncation_retry_limit: 1,
                truncation_retry_min_budget: 32_768,
                ..Default::default()
            },
        );

        drive_query(&mut h).await;
        let events = h.drain_events();

        let notices = truncation_notices(&events);
        assert_eq!(notices.len(), 1, "批量路径同样发通知");
        assert_eq!(notices[0].5, "part", "载荷是整段文本，供落库侧剥离");
        assert!(
            streamed_text(&events).is_empty(),
            "批量路径本就不发 delta，客户端无残句"
        );
    }

    #[test]
    fn test_restore_truncation_salvage_only_fills_empty() {
        // 当前尝试自己吐过文本时残片是陈旧的，覆盖它会把 partial_text
        // 换成上一次尝试的内容 —— 而 partial_text 会经 plan_failure 的
        // 补条冻结进历史。
        let mut h = retry_harness(vec![], LooperConfig::default());
        h.looper.truncation_salvage = Some("stale".to_string());
        h.looper.react_ctx.assistant_text = "current".to_string();

        h.looper.restore_truncation_salvage();

        assert_eq!(h.looper.react_ctx.assistant_text, "current");
        assert!(
            h.looper.truncation_salvage.is_none(),
            "残片即便不被采用也必须被消费掉，否则会留到下一次收尾"
        );

        // 当前尝试没吐过文本（重试请求发不出去那条路径）→ 照常归还
        h.looper.react_ctx.assistant_text.clear();
        h.looper.truncation_salvage = Some("stale".to_string());
        h.looper.restore_truncation_salvage();
        assert_eq!(h.looper.react_ctx.assistant_text, "stale");
    }

    #[tokio::test]
    async fn test_finalize_tool_execution_drops_stale_salvage() {
        // 重试之后那次尝试成功产出工具调用 → 残片要兜底的场景已过去。
        // 不清的话本轮后续任何一次「什么都没吐出来」的失败都会把它顶上。
        let mut h = retry_harness(vec![], LooperConfig::default());
        h.looper.truncation_salvage = Some("stale".to_string());

        h.looper.finalize_tool_execution().await;

        assert!(
            h.looper.truncation_salvage.is_none(),
            "进入下一轮 ReAct 迭代时残片必须作废"
        );
    }

    #[test]
    fn test_reset_turn_counters_clears_salvage() {
        // 残片只在给本轮收尾兜底时有意义。跨轮存活会让下一轮的失败卡片
        // 显示上一轮的文本 —— 归还点（finalize_failure）之外的出口（本轮
        // 未走收尾漏斗就结束）也必须在换轮时被清干净。
        let mut h = retry_harness(vec![], LooperConfig::default());
        h.looper.truncation_salvage = Some("stale".to_string());

        h.looper.reset_turn_counters();

        assert!(
            h.looper.truncation_salvage.is_none(),
            "残片不得跨用户轮存活"
        );
    }

    #[tokio::test]
    async fn test_empty_rollback_preserves_earlier_salvage() {
        // 截断重发存下残片后，重发在收到响应前又遇瞬时故障（本次零产出）：
        // 第二次回退不得把残片覆写成空串，最终失败时 partial_text 必须还回
        // 截断那次的内容。
        let mut h = retry_harness(
            vec![
                truncated("part"),
                Script::Fail(api_err(429, r#"{"error":{"message":"rate limited"}}"#)),
                Script::Fail(api_err(429, r#"{"error":{"message":"rate limited"}}"#)),
            ],
            LooperConfig {
                truncation_retry_limit: 1,
                truncation_retry_min_budget: 32_768,
                transient_retry_limit: 1,
                transient_retry_base_delay_ms: 1,
                transient_retry_max_delay_ms: 2,
                ..Default::default()
            },
        );

        drive_query(&mut h).await;

        let outcomes = h.turn_outcomes();
        assert_eq!(outcomes.len(), 1, "必须恰好收尾一次");
        match &outcomes[0] {
            TurnOutcome::Failed {
                reason,
                partial_text,
            } => {
                assert!(
                    matches!(reason, TurnFailureReason::RateLimited { .. }),
                    "429 耗尽应收敛为 RateLimited，实际 {reason:?}"
                );
                assert_eq!(partial_text, "part", "零产出的瞬时回退不得摧毁截断残片");
            }
            other => panic!("期望 Failed，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_backoff_deadline_survives_future_drop() {
        // run() 的 select 在用户输入到达时 drop 进行中的 react_step future。
        // deadline 必须留在 self 上（只读等待），重入后继续等剩余时间 ——
        // take() 会把它带进 future 栈帧，drop 即丢失 → 零退避立即重发。
        let mut h = retry_harness(vec![], LooperConfig::default());
        h.looper.retry_deadline = Some(Instant::now() + Duration::from_millis(150));

        let started = Instant::now();
        tokio::select! {
            _ = h.looper.wait_retry_backoff() => panic!("退避不应在抢占窗口内完成"),
            _ = tokio::time::sleep(Duration::from_millis(30)) => {}
        }
        assert!(
            h.looper.retry_deadline.is_some(),
            "future 被 drop 不得丢失 deadline"
        );

        h.looper.wait_retry_backoff().await;
        assert!(
            started.elapsed() >= Duration::from_millis(130),
            "重入必须等满剩余退避（幂等），实际 {:?}",
            started.elapsed()
        );
    }

    // ── 瞬时故障重发与类型化失败原因 ──────────────────────────────────────

    /// 瞬时重发测试用的快退避配置（避免测试真的等 500ms+）。
    fn fast_transient_config(limit: u32) -> LooperConfig {
        LooperConfig {
            transient_retry_limit: limit,
            transient_retry_base_delay_ms: 1,
            transient_retry_max_delay_ms: 2,
            ..Default::default()
        }
    }

    fn api_err(status: u16, body: &str) -> model_provider::ProviderError {
        model_provider::ProviderError::Api {
            status,
            body: body.to_string(),
        }
    }

    #[tokio::test]
    async fn test_batch_rate_limited_retries_then_fails_with_typed_reason() {
        // 429 是瞬时类：重发 transient_retry_limit 次后放弃，
        // 失败原因是类型化的 RateLimited，且保留 provider 原始 body。
        let body = r#"{"error":{"message":"Rate limit reached for deepseek-v4","code":"rate_limit_exceeded"}}"#;
        let mut h = batch_retry_harness(
            vec![
                Script::Fail(api_err(429, body)),
                Script::Fail(api_err(429, body)),
                Script::Fail(api_err(429, body)),
            ],
            fast_transient_config(2),
        );

        drive_query(&mut h).await;

        assert_eq!(
            h.provider.budgets().len(),
            3,
            "首次 + 2 次瞬时重发 = 3 次请求"
        );
        let outcomes = h.turn_outcomes();
        assert_eq!(outcomes.len(), 1, "必须恰好收尾一次");
        match &outcomes[0] {
            TurnOutcome::Failed { reason, .. } => match reason {
                TurnFailureReason::RateLimited { attempts, message } => {
                    assert_eq!(*attempts, 3, "attempts = 实际发起的总请求数");
                    assert!(
                        message.contains("Rate limit reached"),
                        "原始 msg 必须保留，实际 {message}"
                    );
                }
                other => panic!("429 耗尽应收敛为 RateLimited，实际 {other:?}"),
            },
            other => panic!("期望 Failed，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_batch_auth_error_fails_without_retry() {
        // 401 是永久类：零重试，立即 AuthError。
        let mut h = batch_retry_harness(
            vec![Script::Fail(api_err(
                401,
                r#"{"error":{"message":"Invalid API key"}}"#,
            ))],
            fast_transient_config(2),
        );

        drive_query(&mut h).await;

        assert_eq!(h.provider.budgets().len(), 1, "永久类不得重发");
        match &h.turn_outcomes()[..] {
            [TurnOutcome::Failed { reason, .. }] => match reason {
                TurnFailureReason::AuthError { message } => {
                    assert!(message.contains("Invalid API key"), "实际 {message}");
                }
                other => panic!("401 应为 AuthError，实际 {other:?}"),
            },
            other => panic!("期望单次 Failed，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_batch_quota_exhausted_fails_without_retry() {
        // 额度耗尽是永久类（重发也不会有额度）：零重试，类型化报错。
        let mut h = batch_retry_harness(
            vec![Script::Fail(api_err(
                429,
                r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota"}}"#,
            ))],
            fast_transient_config(2),
        );

        drive_query(&mut h).await;

        assert_eq!(h.provider.budgets().len(), 1, "额度耗尽不得重发");
        match &h.turn_outcomes()[..] {
            [TurnOutcome::Failed { reason, .. }] => {
                assert!(
                    matches!(reason, TurnFailureReason::QuotaExhausted { .. }),
                    "应为 QuotaExhausted，实际 {reason:?}"
                );
            }
            other => panic!("期望单次 Failed，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_stream_transient_error_rolls_back_and_retries() {
        // 流中途断开（established-stream transport error）：
        // 残句回退 + 通知（reason=Transient）+ 退避重发 → 第二次成功。
        let mut h = retry_harness(
            vec![
                Script::ChunksWithErr(vec![
                    Ok(StreamChunk::BlockStart {
                        index: 0,
                        block_type: model_provider::BlockType::Text,
                    }),
                    Ok(StreamChunk::TextDelta {
                        index: 0,
                        delta: "半个答案".to_string(),
                    }),
                    Err(model_provider::ProviderError::Stream(
                        "connection reset by peer".into(),
                    )),
                ]),
                completed("完整答案"),
            ],
            fast_transient_config(2),
        );

        drive_query(&mut h).await;

        assert_eq!(
            h.provider.budgets().len(),
            2,
            "断流后必须原样重发一次（不抬预算）"
        );
        assert_eq!(
            h.provider.budgets()[1],
            h.provider.budgets()[0],
            "瞬时重发不改输出预算"
        );

        let events = h.drain_events();
        // 通知：恰好一条，reason=Transient，载荷是被回退的残句
        let notices: Vec<_> = events
            .iter()
            .filter_map(|ev| match ev {
                LooperEvent::TruncationRetry {
                    attempt,
                    discarded_text,
                    reason,
                    ..
                } => Some((*attempt, discarded_text.clone(), *reason)),
                _ => None,
            })
            .collect();
        assert_eq!(notices.len(), 1, "断流重发必须恰好一条通知");
        assert_eq!(notices[0].0, 1);
        assert_eq!(notices[0].1, "半个答案", "残句必须作为载荷供落库剥离");
        assert!(
            notices[0].2 == RetryNoticeReason::Transient,
            "reason 应为 Transient"
        );

        // 通知早于重试尝试的第一条 delta —— 横幅才能插在残句与新正文之间。
        // 注意：被回退那次的 delta 也在事件流里（在通知之前），所以只看
        // 通知之后是否还有 delta，以及通知之后的第一段正文是否为重试内容。
        let notice_at = events
            .iter()
            .position(|ev| matches!(ev, LooperEvent::TruncationRetry { .. }))
            .unwrap();
        let post_notice_text: String = events
            .iter()
            .skip(notice_at + 1)
            .filter_map(|ev| match ev {
                LooperEvent::TextDelta { delta } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            post_notice_text, "完整答案",
            "通知之后必须只包含重试尝试的正文"
        );

        // turn_outcomes 与 drain_events 互斥（同一 listener），从已 drain 的
        // 事件里直接取 TurnComplete。
        let outcomes: Vec<TurnOutcome> = events
            .iter()
            .filter_map(|ev| match ev {
                LooperEvent::TurnComplete { outcome, .. } => Some(outcome.clone()),
                _ => None,
            })
            .collect();
        match &outcomes[..] {
            [TurnOutcome::Success { text }] => {
                assert_eq!(text, "完整答案", "残句不得进入最终结果");
            }
            other => panic!("重试成功应收敛为 Success，实际 {other:?}"),
        }
        assert_eq!(h.committed_turns(), 1, "重试在同一轮内完成");
    }

    #[tokio::test]
    async fn test_content_filter_fails_with_typed_reason_no_retry() {
        // FinishReason::ContentFilter → ContentFiltered，双路径都不重试。
        let mut h = batch_retry_harness(
            vec![batch_response(
                "filtered",
                12,
                model_provider::ResponseStatus::Failed,
                Some(FinishReason::ContentFilter),
            )],
            fast_transient_config(2),
        );

        drive_query(&mut h).await;

        assert_eq!(h.provider.budgets().len(), 1, "内容过滤不得重试");
        match &h.turn_outcomes()[..] {
            [TurnOutcome::Failed { reason, .. }] => {
                assert!(
                    matches!(reason, TurnFailureReason::ContentFiltered { .. }),
                    "应为 ContentFiltered，实际 {reason:?}"
                );
            }
            other => panic!("期望单次 Failed，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_stream_content_filter_fails_with_typed_reason_no_retry() {
        let mut h = retry_harness(
            vec![text_chunks("被过滤", 12, FinishReason::ContentFilter)],
            fast_transient_config(2),
        );

        drive_query(&mut h).await;

        assert_eq!(h.provider.budgets().len(), 1, "内容过滤不得重试");
        match &h.turn_outcomes()[..] {
            [TurnOutcome::Failed { reason, .. }] => {
                assert!(
                    matches!(reason, TurnFailureReason::ContentFiltered { .. }),
                    "应为 ContentFiltered，实际 {reason:?}"
                );
            }
            other => panic!("期望单次 Failed，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_stream_cut_without_finish_retries_transiently() {
        // 流自然关闭但从未收到 Finish、块也未闭合（上游中途消失）：
        // Incomplete + finish_reason=None → 瞬时重发。
        let mut h = retry_harness(
            vec![
                Script::Chunks(vec![
                    StreamChunk::BlockStart {
                        index: 0,
                        block_type: model_provider::BlockType::Text,
                    },
                    StreamChunk::TextDelta {
                        index: 0,
                        delta: "断在半".to_string(),
                    },
                    // 无 BlockEnd、无 Finish — 模拟上游掐断
                ]),
                completed("重试成功"),
            ],
            fast_transient_config(2),
        );

        drive_query(&mut h).await;

        assert_eq!(h.provider.budgets().len(), 2, "掐断应触发一次瞬时重发");
        match &h.turn_outcomes()[..] {
            [TurnOutcome::Success { text }] => assert_eq!(text, "重试成功"),
            other => panic!("期望重试后 Success，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_cancel_during_backoff_stops_before_resend() {
        // 退避等待期间用户取消 → 立即收尾，不发第二次请求。
        let mut h = batch_retry_harness(
            vec![
                Script::Fail(api_err(503, "service unavailable")),
                Script::Fail(api_err(503, "service unavailable")),
            ],
            LooperConfig {
                transient_retry_limit: 2,
                // 大退避：给取消留出窗口
                transient_retry_base_delay_ms: 60_000,
                transient_retry_max_delay_ms: 60_000,
                ..Default::default()
            },
        );

        let listener = h.user_listener.take().expect("harness 持有 listener");
        h._user_speaker
            .send(UserMsg::Query("hi".into()))
            .await
            .expect("looper 侧仍在监听");

        // 退避 60s，取消在 300ms 到达 —— run() 必须在取消后立刻收敛，
        // 而不是等满 60s 再发第二次请求。
        let flag = Arc::clone(&h.cancel_flag);
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            flag.store(true, Ordering::Release);
        });

        let started = Instant::now();
        let _ = tokio::time::timeout(Duration::from_secs(5), h.looper.run(listener)).await;
        let elapsed = started.elapsed();
        canceller.await.ok();

        assert!(
            elapsed < Duration::from_secs(5),
            "取消必须打断退避等待，实际 {elapsed:?}"
        );
        assert_eq!(h.provider.budgets().len(), 1, "取消后不得发出第二次请求");
        match &h.turn_outcomes()[..] {
            [TurnOutcome::Failed { reason, .. }] => {
                assert!(
                    matches!(reason, TurnFailureReason::Cancelled),
                    "应为 Cancelled，实际 {reason:?}"
                );
            }
            other => panic!("期望 Cancelled，实际 {other:?}"),
        }
    }

    #[test]
    fn test_failure_label_includes_typed_reasons() {
        // 中断标签进模型上下文 — 必须是英文短句且带原始 msg 摘要。
        let rate = TurnFailureReason::RateLimited {
            attempts: 3,
            message: "Rate limit reached".into(),
        };
        let label = failure_label(&rate);
        assert!(label.starts_with("rate limited after 3 attempts"));
        assert!(label.contains("Rate limit reached"));

        let auth = TurnFailureReason::AuthError {
            message: "Invalid API key".into(),
        };
        assert_eq!(failure_label(&auth), "auth error: Invalid API key");

        // 长 msg 截断 — 标签不能把整个错误体塞进上下文
        let long = TurnFailureReason::ContextOverflow {
            message: "x".repeat(1000),
        };
        assert!(
            failure_label(&long).chars().count() < 300,
            "标签必须截断长 msg"
        );
    }

    #[test]
    fn test_model_failure_reason_classification() {
        use model_provider::ApiErrorKind;

        // (错误, 期望匹配谓词) — 谓词用 fn 指针避免闭包类型推断噪音
        type Case = (
            model_provider::ProviderError,
            fn(&TurnFailureReason) -> bool,
        );

        fn is_rate(r: &TurnFailureReason) -> bool {
            matches!(r, TurnFailureReason::RateLimited { .. })
        }
        fn is_unavailable(r: &TurnFailureReason) -> bool {
            matches!(r, TurnFailureReason::ModelUnavailable { .. })
        }
        fn is_auth(r: &TurnFailureReason) -> bool {
            matches!(r, TurnFailureReason::AuthError { .. })
        }
        fn is_quota(r: &TurnFailureReason) -> bool {
            matches!(r, TurnFailureReason::QuotaExhausted { .. })
        }
        fn is_overflow(r: &TurnFailureReason) -> bool {
            matches!(r, TurnFailureReason::ContextOverflow { .. })
        }

        let cases: Vec<Case> = vec![
            (api_err(429, "rate limit"), is_rate),
            (
                model_provider::ProviderError::Stream("reset".into()),
                is_unavailable,
            ),
            (api_err(500, "oops"), is_unavailable),
            (api_err(401, "bad key"), is_auth),
            (api_err(400, "insufficient quota"), is_quota),
            (api_err(400, "context_length_exceeded"), is_overflow),
        ];
        for (err, pred) in cases {
            let reason = model_failure_reason(&err, 1);
            assert!(
                pred(&reason),
                "{:?} 映射错误 → {reason:?}",
                err.classify().kind
            );
        }

        // is_transient 与映射一致：只有瞬时类进重发
        assert!(ApiErrorKind::RateLimited.is_transient());
        assert!(!ApiErrorKind::QuotaExhausted.is_transient());
    }
}
