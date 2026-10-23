// ============================================================================
// AgentLooper — 单一状态机驱动的 Agent 执行循环
// ============================================================================
//
// 架构：唯一权威枚举 `LooperState` 平铺全部位置（停靠 / 运行 / 暂停）：
//
//   Idle ──③起轮──→ PreparingRequest ──→ [batch]  ResolvingResponse
//                                       └→ [stream] Streaming ─收尾→ ResolvingResponse
//   ResolvingResponse ──→ ExecutingTools ──→ 回 PreparingRequest
//                     └→ Done ──→ Idle     └→ Failed ──②→ Idle
//
//   暂停只在两个边界落地（挂起意图经 `pause_pending` 传递）：
//   PreparingRequest 入口 → PausedBeforeRequest（Resume → PreparingRequest）
//   ExecutingTools spawn 前 → PausedDuringTools（Resume → ExecutingTools）
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
use tokio::sync::mpsc::error::TrySendError;

use super::agent::{Agent, MessageFilter, ModelResponse, override_output_budget};
use super::compaction::{BoundaryReduction, CompactionOutcome};
use super::context::{estimate_item_tokens, estimate_str_tokens};
use super::dynamic_context::DynamicContext;
use super::error::AgentError;
use super::hooks::{HookAction, LooperHook, ToolHookAction};
use crate::persistence::{INFLIGHT_CRASH_REASON, InflightCheckpoint, PersistError, PersistResult};
use crate::session::{AnnotatedMessage, MessageSource, Session, SessionSnapshot, SessionState};
use crate::utils::intercom::{Listener, Speaker, make_async_intercom_pair};
use tracing::{debug, error, info, warn};

/// 步进边界上界 —— looper 里唯一的「一步最多等多久」，三处共用：流式
/// `consume_stream_chunk` 等 chunk、工具 `execute_tools_step` poll 等任务、
/// `wait_retry_backoff` 的退避切片。
///
/// `run()` 的 ① 只在每步返回后才消费控制消息，故它同时是**取消的最坏生效延迟**。
///
/// **batch 路径（`stream: false`）没有步进边界**：`generate_full().await` 整段生成
/// 期间不可重入，该轮取消延迟 = 整段生成时间。
const STEP_POLL: Duration = Duration::from_millis(200);

/// 轮末收尾报告的合成超时。
///
/// 这段期间 `run()` 不在循环顶，消息通道不被排空 —— 因此它同时是**取消的最坏
/// 生效延迟**：排在通道里的 `Cancel` 要等它返回才看得见。取 30s：Flash 档模型
/// 生成千字符以内文本的正常耗时远低于此，超时即视为上游异常、直接回退。
const EPILOGUE_TIMEOUT: Duration = Duration::from_secs(30);

// ============================================================================
// 状态枚举（不携带数据）
// ============================================================================

/// AgentLooper 的唯一状态 —— 停靠 / 运行 / 暂停平铺在一张表上。
///
/// 字面平铺、无嵌套载荷：恢复位置由两个 `Paused*` 变体自身编码
/// （`PausedDuringTools → ExecutingTools`、`PausedBeforeRequest → PreparingRequest`），
/// `Resume` 因此是一条单次迁移，无过渡态。
///
/// 取消请求**不进枚举**：`OwnedTask::drop` 的安全网在**外部线程**上直接置闩
/// （`cancel_request`），摸不到 `&mut self` 的本枚举；取消的生效点是 `run()`
/// 收尾分支读闩后走 [`AgentLooper::finalize_cancel`]，与状态迁移各管一段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LooperState {
    // ── 停靠 ──
    /// 无在途轮。可启动 pending 轮。
    Idle,

    // ── 运行位置 ──
    /// 构建 GenerateRequest，决定走 batch 还是 streaming 分支。
    PreparingRequest,
    /// 收到完整响应（batch 或 stream），统一解析并决定下一步。
    ResolvingResponse,
    /// 消费 GenerateStream，逐 chunk 处理；结束后迁移到
    /// [`Self::ResolvingResponse`]。
    Streaming,
    /// 执行收集到的 tool calls（spawn + poll 两阶段）。
    ExecutingTools,
    /// 本轮完成（无 tool_calls），待 commit。
    Done,
    /// 流程异常终止，待 `run()` ② 收尾。
    Failed,

    // ── 暂停（恢复位置由变体自身编码）──
    /// 模型输出已完成、本批工具尚未 spawn 时落地的暂停。
    /// Resume → ExecutingTools（从 spawn 阶段继续，无在途任务）。
    PausedDuringTools,
    /// 请求尚未发出时落地的暂停（首开发前 / 瞬时重发前 / 工具批完成后下一次请求前）。
    /// Resume → PreparingRequest。
    PausedBeforeRequest,
}

impl LooperState {
    /// 暂停中。`pause_mirror` 写入、server `set_paused` 共用。
    pub fn is_paused(self) -> bool {
        matches!(self, Self::PausedDuringTools | Self::PausedBeforeRequest)
    }

    /// 有在途推进（`run()` ④ 放行、server「还在跑」判断共用）。
    pub fn is_active(self) -> bool {
        !matches!(
            self,
            Self::Idle | Self::PausedDuringTools | Self::PausedBeforeRequest
        )
    }
}

// ============================================================================
// 运行时数据容器
// ============================================================================

/// 一次模型调用的收敛结果，batch 与 stream 共用。
#[derive(Debug, Clone)]
struct ResolvedResponse {
    result: GenerateResult,
    origin: ResponseOrigin,
}

/// 响应来自 batch 还是 stream。
///
/// 两处行为据此区分：usage 记账时机、流被掐断时的瞬时重试。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseOrigin {
    Batch,
    Stream,
}

/// 内层循环需要的临时数据
#[derive(Debug, Clone, Default)]
pub(crate) struct ReActContext {
    /// 模型调用的收敛结果，待 [`LooperState::ResolvingResponse`] 解析
    resolved: Option<ResolvedResponse>,
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
    /// 工具输出中声明的产物（`@@PECO_ARTIFACT`）。
    ///
    /// 声明行已从 `result` 剥离，因此不会进入模型上下文；归属判定在宿主层
    /// （那里才知道 conversation_id）。
    artifacts: Vec<crate::artifacts::ArtifactDecl>,
}

impl ToolCallResult {
    /// 构造工具结果，并在**唯一入口**剥离产物声明。
    ///
    /// 剥离放在构造函数而不是各调用点：调用点有四处，散着写迟早漏一处，
    /// 而漏一次就会把 `@@PECO_ARTIFACT` 行送进模型上下文（模型会照着
    /// 输出格式编造不存在的产物）。
    fn new(call: Arc<ToolCall>, result: Content, is_error: bool) -> Self {
        let (result, artifacts) = crate::artifacts::strip_content(result);
        Self {
            call,
            result,
            is_error,
            artifacts,
        }
    }
}

// ============================================================================
// LooperConfig — 配置聚合
// ============================================================================

/// AgentLooper 的配置。
///
/// 聚合所有 looper 级别的可配置参数，包括事件 buffer 和 hook 链。
#[derive(Clone)]
pub struct LooperConfig {
    /// 事件通道 buffer 大小。
    pub event_buffer: usize,
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
    /// 统一重发上限：单个用户轮内所有原因合计的重发次数。`0` = 关闭。
    /// 触发原因 `RetryCause`：截断（抬输出预算重发）与瞬时故障
    /// （限流/网络/5xx/流被掐断，退避后原样重发），共用单计数。
    ///
    /// 每次重发消耗一次 `react_loop_iteration`（即 `max_iterations` 预算），
    /// 与本上限双重有界 ⇒ 无死循环。默认 3。
    /// env `PECO_RETRY_LIMIT` 可覆盖（[`Self::from_env`]）。
    pub retry_limit: u32,
    /// 截断重发的输出预算抬升目标，同时是抬升上限（headroom 判据与粘性
    /// 覆盖共用这一个参数）。判据：当前生效预算 < 本值 ⇒ 抬到本值；
    /// 抬过一次后粘性覆盖到轮末（`budget_raised`），生效预算恒 ≥ 本值，
    /// 第二次截断重试被拦死。
    ///
    /// 默认 32_768。**这是安全网，不是 `max_tokens` 的替代品**：正经修法
    /// 是给 `agent.md` 的 `llm:` 显式写 `max_tokens`。模型真实输出上限低于
    /// 本值时，抬升后的重试请求会被网关 400 拒 —— 调低本值，或
    /// `PECO_RETRY_LIMIT=0` 关闭重试。env `PECO_RETRY_OUTPUT_BUDGET` 可覆盖。
    pub retry_output_budget: u32,
    /// 重发的退避起始延迟（毫秒）。第 n 次重发延迟 =
    /// `base * 2^(n-1)`，截断到 [`Self::retry_max_delay_ms`]。
    /// 所有原因（含截断）统一走退避。默认 500。
    /// env `PECO_RETRY_BASE_DELAY_MS` 可覆盖。
    pub retry_base_delay_ms: u64,
    /// 重发的退避延迟上限（毫秒）。默认 5000。
    /// env `PECO_RETRY_MAX_DELAY_MS` 可覆盖。
    pub retry_max_delay_ms: u64,
    /// 撞上 `max_iterations` 上限时的收尾报告合成器。
    ///
    /// `Some` 时，冻结前额外调一次**不带工具**的元任务模型（见
    /// [`super::compaction::TurnSummarizer::summarize_inflight`]），产出「完成了
    /// 什么 / 还剩什么 / 怎么续接」作为本轮最后一条 assistant 消息；`None` 时
    /// 保持既有的固定中断说明。合成失败/超时**非致命**，回退到固定说明 —— 收尾
    /// 只决定「多不多一条 assistant 消息」，绝不改变 `TurnOutcome` / 落盘门控。
    ///
    /// 只对 `MaxIterationsExceeded` 生效（`Cancelled` / 瞬时类失败不合成）。
    /// 默认为 `None`（关闭）。
    pub epilogue: Option<Arc<dyn super::compaction::TurnSummarizer>>,
}

impl Default for LooperConfig {
    fn default() -> Self {
        Self {
            event_buffer: 256,
            hooks: Vec::new(),
            environment: None,
            dynamic_context: None,
            context_strategy: super::context::ContextStrategy::FullHistory,
            persist_on_failure: false,
            message_filter: None,
            compaction: None,
            retry_limit: 3,
            retry_output_budget: 32_768,
            retry_base_delay_ms: 500,
            retry_max_delay_ms: 5_000,
            epilogue: None,
        }
    }
}

impl LooperConfig {
    /// 从环境变量读取重试配置，其余字段取 [`Self::default`]。
    ///
    /// 四个 env（与字段一一对应，旧 `PECO_TRUNCATION_RETRY_*` /
    /// `PECO_TRANSIENT_RETRY_*` 名称已废弃、不兼容读取）：
    ///
    /// | 变量 | 默认 |
    /// |------|------|
    /// | `PECO_RETRY_LIMIT` | 3 |
    /// | `PECO_RETRY_OUTPUT_BUDGET` | 32768 |
    /// | `PECO_RETRY_BASE_DELAY_MS` | 500 |
    /// | `PECO_RETRY_MAX_DELAY_MS` | 5000 |
    ///
    /// Peco / chat / CLI 三个构造点统一经本方法读取。
    pub fn from_env() -> Self {
        let default = Self::default();
        Self {
            retry_limit: env_parse("PECO_RETRY_LIMIT", default.retry_limit),
            retry_output_budget: env_parse("PECO_RETRY_OUTPUT_BUDGET", default.retry_output_budget),
            retry_base_delay_ms: env_parse("PECO_RETRY_BASE_DELAY_MS", default.retry_base_delay_ms),
            retry_max_delay_ms: env_parse("PECO_RETRY_MAX_DELAY_MS", default.retry_max_delay_ms),
            ..default
        }
    }
}

/// 读一个数值环境变量：缺失取默认值，写错告警后取默认值。
///
/// 解析失败不静默吞 —— 值写错却「看起来生效了」是最难查的配置问题。
fn env_parse<T: std::str::FromStr>(name: &str, default: T) -> T {
    match std::env::var(name) {
        Ok(raw) => match raw.trim().parse() {
            Ok(value) => value,
            Err(_) => {
                warn!(
                    variable = name,
                    value = %raw,
                    "Invalid numeric env var; using default"
                );
                default
            }
        },
        Err(_) => default,
    }
}

impl std::fmt::Debug for LooperConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LooperConfig")
            .field("event_buffer", &self.event_buffer)
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
            ContentBlock::ServerToolUse { .. } => "server_tool_use",
            ContentBlock::ServerToolResult { .. } => "server_tool_result",
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
/// 经 [`TurnComplete`](LooperEvent::TurnComplete) 的 `failure` 字段传递：
/// `None` 表示正常完成（`LooperState::Done`），`Some(...)` 表示异常终止
/// （`LooperState::Failed`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum TurnFailureReason {
    /// 外部取消
    Cancelled,
    /// 超出最大迭代次数
    MaxIterationsExceeded,
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
        /// 工具声明的产物（已从 `result` 剥离）。
        artifacts: Vec<crate::artifacts::ArtifactDecl>,
    },
    /// 服务端工具调用（模型发起，provider 侧执行，不在客户端执行）
    ServerToolUse {
        id: String,
        name: String,
        input: String,
    },
    /// 服务端工具结果
    ServerToolResult {
        id: String,
        name: String,
        content: String,
    },
    /// 模型调用 token 用量
    ModelUsage {
        /// Zero-based index of this call within the run
        call_index: usize,
        usage: Usage,
    },

    // ── 生命周期事件 ──────────────────────────────────────────────────────
    /// 状态迁移 —— 唯一权威 [`LooperState`] 的每次真变化都发一条
    /// （含运行位置之间的迁移，如 `Streaming → ExecutingTools`）。
    ///
    /// `set` 幂等（`from == to`）时不发。
    StateChange { from: LooperState, to: LooperState },

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
    /// 已由 `AgentLooper::begin_retry` 从 Session staging 中回退，因此它只存在于
    /// 「实时视图」里，不存在于任何权威历史中 —— 这正是本事件要解释的事实。
    /// 前端重载后从快照恢复，残句与通知一并消失。
    ///
    /// wire 名保持 `truncation_retry`（历史兼容），`reason` 区分两种重发：
    /// 截断（抬预算）与瞬时故障（退避原样重发）。
    TruncationRetry {
        turn_index: usize,
        /// 第几次重试（1-based）— 全部原因共用单计数
        attempt: u32,
        /// 本轮的重试上限（0 = 关闭，此时不会发出本事件）
        limit: u32,
        /// 截断那次的输出 token 数（瞬时重发传 0）
        output_tokens: u32,
        /// 抬升后的输出预算（瞬时重发传当前生效预算）
        retry_budget: u32,
        /// 该次尝试已下发的正文增量。
        ///
        /// **仅供落库侧对齐**（`chat` 模块的 `messages` 表累加器按后缀剥离），
        /// **不向客户端下发** —— `map_looper_event` 刻意过滤掉它。
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

/// 重发原因 —— 决定是否抬预算、通知 reason 与日志字段。
///
/// 两种原因共享同一骨架（守卫 → 回退 → 计数 → 退避 → 通知，见
/// [`AgentLooper::begin_retry`]），差异只是入参级别的数据：
/// 截断抬输出预算且需要 headroom 判据，瞬时原样重发。
#[derive(Debug, Clone, Copy)]
enum RetryCause {
    /// 输出顶到 `max_tokens` 被截断（`Incomplete + FinishReason::MaxTokens`）。
    Truncated {
        /// 触发截断那次响应的输出 token 数（进通知载荷）
        output_tokens: u32,
    },
    /// 瞬时故障：限流 / 网络 / 上游 5xx / 流被掐断。
    Transient {
        /// 故障类别的日志字段（`rate_limited` / `network` / `server` / `stream_cut`）
        trigger: &'static str,
    },
}

impl RetryCause {
    /// 通知载荷里的输出 token 数：截断取实际值，瞬时无意义传 0。
    fn retry_notice_output_tokens(&self) -> u32 {
        match self {
            Self::Truncated { output_tokens } => *output_tokens,
            Self::Transient { .. } => 0,
        }
    }

    /// 通知载荷里的重发原因（wire 枚举 [`RetryNoticeReason`] 不变）。
    fn retry_notice_reason(&self) -> RetryNoticeReason {
        match self {
            Self::Truncated { .. } => RetryNoticeReason::Truncated,
            Self::Transient { .. } => RetryNoticeReason::Transient,
        }
    }
}

// ============================================================================
// UserMsg  — 外部通信
// ============================================================================

/// 用户输入消息
///
/// 暂停/恢复/取消与查询同走 `user_listener` 一条通道：FIFO 全序保证
/// 「先发的 query、后到的 pause」交错语义确定，且消息本身即可唤醒
/// 阻塞在 `recv()` 上的 looper（flag 做不到）。
///
/// 消息在**步进边界**消费：`react_step()` 在途时不抢占、不丢弃 future ——
/// `run()` 循环顶先非阻塞排空通道，再推进一步（工具=≤200ms 轮询、
/// batch=整段生成）。流式在途时 `Pause` 不落在此边界，见 [`UserMsg::Pause`]。
#[derive(Debug, Clone)]
pub enum UserMsg {
    /// 用户查询（纯文本或文本 + 图片部件混排）
    Query(Content),
    /// 请求暂停：置挂起意图（`pause_pending`），在两个边界落地 ——
    /// 「请求发出前」（[`LooperState::PausedBeforeRequest`]）或
    /// 「工具执行前」（[`LooperState::PausedDuringTools`]）。
    ///
    /// 运行位（PreparingRequest / ResolvingResponse / Streaming / ExecutingTools）
    /// 收到时只记意图；空闲 / 已完成 / 已失败 / 已暂停收到时无操作
    /// （空闲不响应挂起；已暂停幂等）。
    Pause,
    /// 请求恢复：从暂停态单次迁移到变体编码的恢复位置 ——
    /// `PausedDuringTools → ExecutingTools`、`PausedBeforeRequest → PreparingRequest`。
    /// 非暂停态则撤回尚未落地的暂停意图（清 `pause_pending`）—— 含 `Done` /
    /// `Idle` 上被轮次完成吸收、尚未被下一轮起点复位的意图。
    Resume,
    /// 请求取消：中止当前 ReAct 轮，looper 继续运行（收尾见
    /// [`AgentLooper::finalize_cancel`]）
    Cancel,
    /// 关闭请求（在途轮直接丢弃，不记账）
    Shutdown,
}

/// 一条用户消息对「继续读取同批剩余消息」的裁决。
///
/// `Cancel` 与 `Shutdown` 都要丢弃同批后续消息（典型是「按停后马上发的新 query」），
/// 但两者对循环的诉求不同 —— 单个 `bool` 表达不了「停不停读」与「退不退出」两件事，
/// 故本枚举只管前者，后者由 [`AgentLooper::shutdown_requested`] 承担。
#[derive(Debug, Clone, Copy)]
enum ReadDirective {
    /// 继续读同批剩余消息。
    Continue,
    /// 本条消息改变了后续消息的解释前提 —— 同批剩余一律丢弃。
    StopReading,
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
/// h.cancel().await;             // 请求取消（走 user channel）
/// h.pause().await; / h.resume().await;  // 暂停 / 继续（走 user channel）
/// h.wait().await;               // 等待完成并获取结果
/// ```
///
/// Looper 后台任务 handle。
///
/// 内部使用 `Arc` 共享所有权。最后一个 clone 被 drop 时**只设置 cancel_request**，
/// 让 looper 在下一个循环迭代里自行收尾退出 —— 不 abort。
/// abort 会跳过失败收尾（冻结 + 落盘），把中断轮丢掉。
/// `try_lock` 保证与 `wait()` 方法无竞态。
struct OwnedTask {
    inner: SharedModelTask,
    /// 取消请求闩：drop 时先设置此闩通知 looper 退出，再 abort 任务。
    cancel_request: Arc<AtomicBool>,
}

impl Clone for OwnedTask {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            cancel_request: Arc::clone(&self.cancel_request),
        }
    }
}

impl Drop for OwnedTask {
    fn drop(&mut self) {
        // strong_count == 1 表示这是最后一个引用
        if Arc::strong_count(&self.inner) == 1 {
            // 设置取消请求闩作为安全网：若 looper 仍在运行，会在下个循环迭代中正常退出。
            // looper 可能已通过 shutdown()/wait() 正常结束，此时 cancel_request 无实际作用。
            self.cancel_request.store(true, Ordering::Release);
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
        cancel_request: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(Some(handle))),
            cancel_request,
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
    /// 取消请求闩
    cancel_request: Arc<AtomicBool>,
    /// 暂停状态镜像（looper 进出 Paused 时写入，仅供 `is_paused()` 查询；
    /// 暂停控制本身走 [`UserMsg::Pause`] / [`UserMsg::Resume`]）
    pause_mirror: Arc<AtomicBool>,
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

    /// 请求取消：向 looper 发送 [`UserMsg::Cancel`]。
    ///
    /// - 消息本身唤醒停靠或暂停中阻塞在 `recv()` 上的 looper
    ///   （裸 flag 置位叫不醒 —— 与 pause/resume 同走 user channel）
    /// - 收尾统一走循环顶取消分支：在途轮冻结进历史（`TurnComplete` +
    ///   `Failed{Cancelled}`，落盘由 `persist_on_failure` 门控），pending
    ///   队列保留但不续接
    /// - 消息在步进边界消费，**不丢弃**在途 `react_step` future —— 当前
    ///   步进完成后才收尾（流式=单 chunk、工具=≤200ms 轮询、batch=整段
    ///   生成）；Done 分支的 commit→save 窗口因此完整跑完，不会丢轮
    /// - 返回 `Err` 表示 looper 已结束（channel 关闭）
    ///
    /// 与 [`Self::shutdown`] 不重复：cancel 记账（半成品冻结进历史），
    /// shutdown 即弃（staging 直接丢弃）。
    pub async fn cancel(&self) -> Result<(), tokio::sync::mpsc::error::SendError<UserMsg>> {
        self.user_speaker.send(UserMsg::Cancel).await
    }

    /// 请求暂停：向 looper 发送 [`UserMsg::Pause`]。
    ///
    /// 运行位只记挂起意图、不立即挂起，在边界检查点落地（请求发出前 /
    /// 工具执行前，见 [`Self::react_step`]）：模型调用在途时等到本次调用收尾，
    /// 工具执行中等到本批跑完（与 `send_query` 同通道 FIFO，交错语义确定）。
    /// 暂停期间 `send_query`
    /// 的消息进入 pending 队列，恢复时若 looper 停靠在 Idle 会立即续接。
    /// 调用 [`Self::resume`] 恢复。
    /// 返回 `Err` 表示 looper 已结束（channel 关闭）。
    pub async fn pause(&self) -> Result<(), tokio::sync::mpsc::error::SendError<UserMsg>> {
        self.user_speaker.send(UserMsg::Pause).await
    }

    /// 请求恢复：向 looper 发送 [`UserMsg::Resume`]，从暂停态单次迁移到变体
    /// 编码的恢复位置（见 [`UserMsg::Resume`]）。非暂停态下若有尚未落地的暂停
    /// 意图则将其撤回。消息本身唤醒阻塞在 `recv()` 上的 looper；幂等。
    pub async fn resume(&self) -> Result<(), tokio::sync::mpsc::error::SendError<UserMsg>> {
        self.user_speaker.send(UserMsg::Resume).await
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

    /// 取消是否已生效。
    ///
    /// 请求闩由 looper 在消费 [`UserMsg::Cancel`] 时写入（`cancel().await`
    /// 返回后、looper 实际消费消息前仍为 `false`）；drop 安全网也直接写入。
    pub fn is_cancelled(&self) -> bool {
        self.cancel_request.load(Ordering::Acquire)
    }

    /// looper 是否处于暂停态（`PausedDuringTools` / `PausedBeforeRequest`）。
    ///
    /// 这是 looper 在状态迁移时写入的镜像 —— `pause().await` 返回后、
    /// looper 实际消费消息前仍为 `false`。
    pub fn is_paused(&self) -> bool {
        self.pause_mirror.load(Ordering::Acquire)
    }

    /// looper 后台任务是否仍在运行。
    pub fn is_running(&self) -> bool {
        match self.task_handle.inner.try_lock() {
            Ok(guard) => guard.as_ref().is_some_and(|h| !h.is_finished()),
            Err(_) => false,
        }
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
            cancel_request: Arc::clone(&self.cancel_request),
            pause_mirror: Arc::clone(&self.pause_mirror),
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
///     agent, session, event_speaker, config, persister,
/// );
/// let result = looper.run(user_listener).await?;
/// ```
pub struct AgentLooper {
    // ── 静态配置 ──
    agent: Arc<Agent>,
    max_iterations: usize,
    config: LooperConfig,
    /// 稳定前缀：`agent.system_prompt()` + `config.environment`，
    /// 构造时计算一次并缓存，避免每次 turn 重新拼接。
    /// 此后每轮仅在此基础上追加 dynamic context。
    stable_prefix: String,

    /// 当前 turn 缓存的动态上下文字符串。
    /// 在 [`Self::prepare_and_send_request`] 检测到新 query 时更新，
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
    /// [`Self::reset_iteration_counters`] 清除。
    dynamic_context_resolved: bool,

    // ── 会话 ──
    /// Session 持有全部对话状态（committed + staging + pending + turn_index + usage）。
    session: Box<Session>,

    // ── 状态机 ──
    /// 唯一权威状态。
    ///
    /// 所有状态变化必须经 [`Self::set`] / [`Self::set_quiet`] 完成 ——
    /// 它们负责写本字段、维护 `pause_mirror` 镜像、（`set`）发事件。
    /// 禁止直接改本字段。
    state: LooperState,
    /// 暂停挂起意图：运行位收到 [`UserMsg::Pause`] 时置位，只在两个边界
    /// 检查点（`react_step` 的 PreparingRequest 入口 / ExecutingTools spawn 前）
    /// 落地并清零。**不是状态**：不进 [`Self::has_pending_work`]，重发窗口
    /// 天然存活（[`Self::begin_retry`] 不清它）。
    pause_pending: bool,
    react_ctx: ReActContext,

    // ── 运行时追踪（不可持久化）──
    /// 本轮失败原因；`None` 表示尚未失败 / 正常完成
    failure_reason: Option<TurnFailureReason>,

    // ── ReAct 迭代计数（非对话轮次）──
    /// ReAct 循环迭代计数：当前对话轮次中已发出的模型调用次数。
    ///
    /// 与 Session 的 `turn_index`（对话轮次）不同，此计数器在每次用户输入
    /// 开始新对话轮次时重置为 0，每次回到 `PreparingRequest` 时递增。
    /// 用于 `max_iterations` 限制——限制的是单次对话轮次内得到最终结果
    /// 所需的模型调用次数，而非对话轮数。
    react_loop_iteration: usize,

    // ── pending 调度 ──
    /// pending 队列是否「现在该跑」—— 由用户意图驱动，与「有没有 pending」正交。
    /// `Resume` / 新 `Query` 置位，`Cancel` 收尾与启动一轮后清零，初值 `true`。
    /// 取消不自动开新轮（pending 保留、随快照落盘），「保留」不等于「该跑」。
    pending_armed: bool,

    // ── 退出信号 ──
    /// 收到 [`UserMsg::Shutdown`]。**是字段而非返回值**：`Shutdown` 不关闭通道，
    /// `recv` 的 `None` 替代不了它；且消费点里含非阻塞排空，返回值会被丢掉。
    /// 字段能穿过去，循环顶一句 `if self.shutdown_requested { break }` 即可。
    shutdown_requested: bool,

    // ── 事件输出 ──
    event_speaker: Speaker<LooperEvent>,

    // ── 取消请求闩（内部自建；控制入口是 UserMsg::Cancel，本闩供
    //    循环顶/react_step 内部检查点读取，drop 安全网与消息路径写入）──
    cancel_request: Arc<AtomicBool>,

    // ── 暂停状态镜像（state.is_paused() 的投影；looper 写入，
    //    供 handle.is_paused() 跨线程查询，控制入口是 UserMsg::Pause/Resume）──
    pause_mirror: Arc<AtomicBool>,

    // ── 持久化 ──
    persister: Arc<dyn crate::persistence::SessionPersister>,

    // ── 纯运行时状态（不可持久化）──
    /// streaming 模式：活跃的 [`GenerateStream`]
    active_stream: Option<GenerateStream>,
    /// streaming 模式：中立块组装器（跨 chunk 持久）
    stream_assembler: BlockAssembler,
    /// 活跃的 tool 执行任务集（增量执行模式：Spawn → Poll → 完成）
    active_tool_tasks: Option<tokio::task::JoinSet<(usize, ToolCallResult)>>,
    /// 最近一次请求的估算上下文 token（响应到达后与实际 usage 对照）。
    /// 估算器单点在 [`super::context`]。
    last_request_estimated_tokens: Option<usize>,

    // ── 统一重试（纯运行时状态，不落盘）──
    /// 本用户轮内已用掉的重发次数（全部 [`RetryCause`] 合计，单计数单上限）。
    ///
    /// 与 `react_loop_iteration` **同生命周期**（单个用户轮），因此凡是要重置
    /// 前者的地方都必须重置后者 —— 见 [`Self::reset_iteration_counters`]。
    retries_used: usize,
    /// 本轮是否抬过输出预算（粘性布尔，只由截断重发置位）。
    ///
    /// 必须是独立布尔而非 `retries_used > 0`：瞬时重发递增同一个计数，
    /// 但绝不能抬预算（抬了可能超出模型真实上限 → 400，且改变了原样重发
    /// 的语义）。随 `reset_iteration_counters` 清零，粘性不出轮 —— 本轮抬过一次
    /// 后，后续 ReAct 迭代继续用抬高的预算，否则 tool 调用之后的下一次
    /// 迭代可能以同样的方式再截断一次。
    budget_raised: bool,
    /// 下次重发的退避截止时刻。`None` = 无需等待。
    /// 由 [`Self::begin_retry`] 设置（截断重发同样走退避），在
    /// [`Self::prepare_and_send_request`] 入口以可取消的切片 sleep 等待 ——
    /// 等待贴在请求发出前，回退与通知立即发生。
    /// 等待**只读不清**（见 [`Self::wait_retry_backoff`]）：`react_step` future
    /// 被 drop 后重入仍能继续等剩余时间。
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
    /// - `config` — looper 配置（事件 buffer、hook 链等）
    ///
    /// 取消/暂停标志在内部创建 —— 控制入口是 `UserMsg::{Cancel,Pause,Resume}`
    /// （走 user channel），标志只是 looper 写、外部读的状态
    /// （`spawn()` 取共享克隆给 handle 供 `is_cancelled()`/`is_paused()` 查询
    /// 与 drop 安全网使用）。
    pub fn new(
        agent: Arc<Agent>,
        session: Box<Session>,
        event_speaker: Speaker<LooperEvent>,
        config: LooperConfig,
        persister: Arc<dyn crate::persistence::SessionPersister>,
    ) -> Self {
        let max_iterations = agent.max_iterations();
        // 在 config 被 move 进结构体之前求值稳定前缀
        let stable_prefix =
            compose_stable_prefix(&agent.system_prompt(), config.environment.as_deref());

        AgentLooper {
            agent,
            max_iterations,
            config,
            stable_prefix,
            dynamic_context: None,
            dynamic_context_resolved: false,
            session,
            state: LooperState::Idle,
            pause_pending: false,
            react_ctx: ReActContext::default(),
            failure_reason: None,
            react_loop_iteration: 0,
            pending_armed: true,
            shutdown_requested: false,
            event_speaker,
            cancel_request: Arc::new(AtomicBool::new(false)),
            pause_mirror: Arc::new(AtomicBool::new(false)),
            persister,
            active_stream: None,
            stream_assembler: BlockAssembler::new(),
            active_tool_tasks: None,
            last_request_estimated_tokens: None,
            retries_used: 0,
            budget_raised: false,
            retry_deadline: None,
        }
    }

    /// 一键创建 AgentLooper 并返回 `LooperHandle`。
    ///
    /// 一次性完成：intercom 创建拆分、spawn 后台任务。标志由 `new()`
    /// 内部创建，此处取共享克隆给 handle（`is_cancelled()`/`is_paused()`
    /// 查询 + drop 安全网）。外部只需操作返回的 `LooperHandle`。
    pub fn spawn(
        agent: Arc<Agent>,
        session: Box<Session>,
        config: LooperConfig,
        persister: Arc<dyn crate::persistence::SessionPersister>,
    ) -> LooperHandle {
        let (looper_side, caller_side) =
            make_async_intercom_pair::<LooperEvent, UserMsg>(config.event_buffer);
        let (event_speaker, user_listener) = looper_side.split();
        let (user_speaker, event_listener) = caller_side.split();

        let mut looper = AgentLooper::new(agent, session, event_speaker, config, persister);
        // 同一实例的共享克隆：looper 写、handle 读（查询 + OwnedTask drop 安全网）
        let cancel_request = Arc::clone(&looper.cancel_request);
        let pause_mirror = Arc::clone(&looper.pause_mirror);

        let agent_name = looper.agent.config().agent.name.clone();
        let session_id = looper.session.id().to_owned();

        debug!(
            agent = %agent_name,
            session_id = %session_id,
            "AgentLooper spawned"
        );

        let handle = tokio::spawn(async move { looper.run(user_listener).await });

        let task_handle = OwnedTask::new(handle, cancel_request.clone());

        LooperHandle {
            user_speaker,
            event_listener: Arc::new(tokio::sync::Mutex::new(event_listener)),
            cancel_request,
            pause_mirror,
            task_handle,
        }
    }

    // ── 内部辅助方法 ────────────────────────────────────────────────────────

    /// 检查取消请求闩是否被触发。
    fn is_cancelled(&self) -> bool {
        self.cancel_request.load(Ordering::Acquire)
    }

    /// 非阻塞发送普通事件；若 channel 满或接收端关闭则丢弃。
    /// 适用于高频增量事件（TextDelta、ToolCallDelta 等）。
    ///
    /// 唯一例外是带 `artifacts` 的 `ToolResult`：声明行已从 `result` 剥离，
    /// 事件即产物的唯一载体，丢一次就再也找不回 —— 这类失败必须留痕。
    fn emit_event(&self, event: LooperEvent) {
        // 失败的 `try_send` 会把事件还回来，据此判断丢的是不是产物声明。
        let (reason, event) = match self.event_speaker.try_send(event) {
            Ok(()) => return,
            Err(TrySendError::Full(event)) => ("full", event),
            Err(TrySendError::Closed(event)) => ("closed", event),
        };
        let LooperEvent::ToolResult {
            name, artifacts, ..
        } = event
        else {
            return;
        };
        if artifacts.is_empty() {
            return;
        }
        warn!(
            session_id = %self.session.id(),
            tool = %name,
            count = artifacts.len(),
            reason = %reason,
            "Artifact declarations lost: event channel unavailable"
        );
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

    // ── 状态迁移（唯一写点）────────────────────────────────────────────────

    /// 状态的唯一迁移入口：写权威状态 → 同步派生镜像 → 发
    /// [`LooperEvent::StateChange`]。三步同帧有序。
    ///
    /// 顺序不可颠倒 —— 外部收到 `StateChange { to: 暂停态 }` 时
    /// `is_paused()` 必须已经为 true，否则事件面与查询面互相矛盾
    /// （镜像晚写就会出现「暂停事件已到、镜像还是 false」的窗口）。
    ///
    /// 返回迁移前的状态，供需要旧状态做后续判断的调用点使用。
    /// `prev == next` 时是无副作用的 no-op（不发事件）。
    fn set(&mut self, next: LooperState) -> LooperState {
        let prev = self.state;
        if prev == next {
            return prev;
        }
        self.state = next;
        // 镜像先于事件写入：外部读到暂停态时事件已可见
        self.pause_mirror.store(next.is_paused(), Ordering::Release);
        debug!(
            agent = %self.agent.config().agent.name,
            session_id = %self.session.id(),
            from = ?prev,
            to = ?next,
            "Looper state changed"
        );
        self.emit_event(LooperEvent::StateChange {
            from: prev,
            to: next,
        });
        prev
    }

    /// 与 [`Self::set`] 相同，但**不发事件** —— 供退出复位与测试构造状态使用：
    /// 退出复位与现状逐位一致，静默即不引入新的可见事件（测试借此绕开事件面）。
    fn set_quiet(&mut self, next: LooperState) {
        if self.state == next {
            return;
        }
        self.state = next;
        self.pause_mirror.store(next.is_paused(), Ordering::Release);
    }

    // ── Hook 调用辅助函数 ──────────────────────────────────────────────────
    //
    // NOTE: 这些是关联函数（非方法），直接接收 hooks 切片，避免 `&self` 跨越 await point。
    // 由于 AgentLooper 不是 Sync（含 non-Sync 的 GenerateStream），
    // `&self` 不能在 tokio::spawn 的 future 中跨 await 持有。

    async fn invoke_on_before_request(
        hooks: &[Arc<dyn LooperHook>],
        turn_index: usize,
        messages: &mut Vec<Arc<InputItem>>,
    ) -> HookAction {
        for hook in hooks {
            match hook.on_before_request(turn_index, messages).await {
                HookAction::Continue => continue,
                other => return other,
            }
        }
        HookAction::Continue
    }

    async fn invoke_on_after_response(
        hooks: &[Arc<dyn LooperHook>],
        turn_index: usize,
        response: &GenerateResult,
    ) -> HookAction {
        for hook in hooks {
            match hook.on_after_response(turn_index, response).await {
                HookAction::Continue => continue,
                other => return other,
            }
        }
        HookAction::Continue
    }

    async fn invoke_on_text_delta(
        hooks: &[Arc<dyn LooperHook>],
        turn_index: usize,
        delta: &str,
        accumulated: &str,
    ) -> HookAction {
        for hook in hooks {
            match hook.on_text_delta(turn_index, delta, accumulated).await {
                HookAction::Continue => continue,
                other => return other,
            }
        }
        HookAction::Continue
    }

    async fn invoke_on_before_tool(
        hooks: &[Arc<dyn LooperHook>],
        turn_index: usize,
        tool_call: &ToolCall,
    ) -> ToolHookAction {
        for hook in hooks {
            match hook.on_before_tool(turn_index, tool_call).await {
                ToolHookAction::Continue => continue,
                other => return other,
            }
        }
        ToolHookAction::Continue
    }

    async fn invoke_on_after_tool(
        hooks: &[Arc<dyn LooperHook>],
        turn_index: usize,
        tool_call: &ToolCall,
        result: &str,
        is_error: bool,
    ) {
        for hook in hooks {
            hook.on_after_tool(turn_index, tool_call, result, is_error)
                .await;
        }
    }

    async fn invoke_on_turn_complete(
        hooks: &[Arc<dyn LooperHook>],
        turn_index: usize,
        failure: Option<&TurnFailureReason>,
        usage: &Usage,
        session: &Session,
    ) {
        for hook in hooks {
            hook.on_turn_complete(turn_index, failure, usage, session)
                .await;
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

    async fn invoke_on_boundary_reduction(
        hooks: &[Arc<dyn LooperHook>],
        session: &Session,
        reduction: &BoundaryReduction,
    ) {
        for hook in hooks {
            hook.on_boundary_reduction(session, reduction).await;
        }
    }

    /// 对照请求前的估算 token 与 API 返回的实际 input_tokens。
    ///
    /// 只在 debug 级别记录 — 数据用于长期观测估算器偏差，非运行时告警。
    fn log_estimate_calibration(&mut self, turn_index: usize, actual_input_tokens: u32) {
        if let Some(estimated) = self.last_request_estimated_tokens.take() {
            let ratio = if actual_input_tokens > 0 {
                estimated as f64 / actual_input_tokens as f64
            } else {
                0.0
            };
            debug!(
                turn_index,
                estimated,
                actual = actual_input_tokens,
                ratio = format!("{ratio:.2}"),
                "Token estimate calibration (estimated / actual)"
            );
        }
    }

    async fn invoke_on_state_change(
        hooks: &[Arc<dyn LooperHook>],
        from: LooperState,
        to: LooperState,
    ) {
        for hook in hooks {
            hook.on_state_change(from, to).await;
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

    /// 获取当前状态（唯一权威 [`LooperState`]）。
    pub fn state(&self) -> LooperState {
        self.state
    }

    /// 获取已执行轮数（从 Session 读取）。
    pub fn turn_count(&self) -> usize {
        self.session.turn_index()
    }

    /// 获取聚合 token 用量（从 Session 读取）。
    pub fn total_usage(&self) -> Usage {
        self.session.total_usage()
    }

    // ── run() 主循环 ──────────────────────────────────────────────────────

    /// 执行 agent run 循环 —— 四步，每步只问一个问题。
    ///
    /// ```text
    /// ① 取消息（无待办才阻塞）
    /// ② 收尾（取消 / 失败，互斥）
    /// ③ 启动 pending 轮（唯一的「pending → turn」入口）
    /// ④ 推进一步 react_step（仅 is_active()）
    /// ```
    ///
    /// 控制消息只在 ① 消费：在途 future（commit→save、prepare 重发、工具 poll 回收）
    /// 永不被丢弃，消息因此在**步进边界**生效（工具=≤200ms、batch=整段生成）。
    /// **例外**：运行位收到的 `Pause` 不在此边界落地 —— 只记 `pause_pending` 意图，
    /// 在两个边界检查点（请求发出前 / 工具执行前）落地（见 [`Self::react_step`]），
    /// 取消仍在 chunk 边界生效。
    ///
    /// **不空转不变量**：每轮迭代要么在 ① 阻塞，要么严格推进一个状态 ——
    /// `has_pending_work` 的每个「非阻塞」触发点都有消解者（`cancel_request` /
    /// `Failed` / `pending_armed && has_pending` / 运行位分别由
    /// [`Self::finalize_cancel`] / [`Self::finalize_failure`] /
    /// [`Self::start_turn_from_pending`] / [`Self::react_step`]）。**新增触发点必须
    /// 同时给出消解者**，否则热自旋。
    ///
    /// 通道关闭后：无在途轮直接退，有在途轮跑完回到 Idle 才退（不静默丢轮）。
    pub async fn run(
        &mut self,
        mut user_listener: Listener<UserMsg>,
    ) -> Result<ModelResponse, AgentError> {
        info!(
            agent = %self.agent.config().agent.name,
            session_id = %self.session.id(),
            max_iterations = self.max_iterations,
            "AgentLooper::run() started"
        );

        // Ensure deferred MCP connections are established before first tool use.
        self.agent.mcp_manager().ensure_connected().await;

        loop {
            // ── ① 取消息 ──────────────────────────────────────────────────
            // 有待办时只非阻塞排空（排完立刻回 ②③④），无待办才阻塞等一条，真暂停
            // 天然落进后者。排空不可省 —— 在途轮期间的 Cancel / Pause / Query 全靠
            // 它被消费。
            if self.has_pending_work() {
                self.drain_user_msgs(&mut user_listener);
            } else if !self.recv_user_msgs(&mut user_listener).await {
                // 通道已关，不会再有消息解除暂停 —— 按取消语义收尾（冻结在途轮
                // 为 Cancelled + 强制落盘，pending 不丢）。
                if self.state.is_paused() {
                    self.finalize_cancel().await;
                }
                // 无在途轮 → 无可推进，退出。有在途轮 → 落穿到 ②③④ 跑完，
                // 回到 Idle 后再阻塞一次 recv() 拿到 None 才退。
                if matches!(self.state, LooperState::Idle) {
                    break;
                }
            }
            if self.shutdown_requested {
                break;
            }

            // ── ② 收尾（互斥）────────────────────────────────────────────
            // 取消优先，但 `finalize_cancel` **继承**已有 failure_reason —— 只看状态，
            // 不看谁先到，无顺序耦合。
            if self.is_cancelled() {
                self.finalize_cancel().await;
            } else if matches!(self.state, LooperState::Failed) {
                self.finalize_failure().await;
            }

            // ── ③ 启动 pending 轮 —— 唯一的「pending → turn」入口 ─────────
            // 只放行 `Idle`：暂停态不放行 —— 暂停只在在途轮的边界落地，排队
            // 输入由 pending 队列持有，`Resume` 后内层跑完本轮、commit 回
            // Idle，③ 再启动续接轮。
            if matches!(self.state, LooperState::Idle)
                && matches!(self.session.state(), SessionState::Idle)
                && self.pending_armed
                && self.session.has_pending()
            {
                self.start_turn_from_pending();
            }

            // ── ④ 推进 ───────────────────────────────────────────────────
            // Idle / Paused* 回顶，① 会阻塞在 `recv()` 上。
            if self.state.is_active() {
                self.react_step().await;
            }
        }

        let (usage, turns) = {
            let u = self.session.total_usage();
            let t = self.session.turn_index();
            (u, t)
        };

        // 退出即丢弃积压消息（Cancel/Shutdown 收到就退，后面的消息随 listener
        // drop 消失）。送出方只会看到 channel 接收成功，故必须留痕，别让消息
        // 凭空不见。
        let mut dropped = 0usize;
        while user_listener.try_recv().is_ok() {
            dropped += 1;
        }
        if dropped > 0 {
            warn!(dropped, "Dropped queued control messages on looper exit");
        }

        // 暂停中退出（Shutdown/input_closed）时复位镜像，避免 is_paused() 残留 true。
        // 退出路径与现状逐位一致：不发事件，状态一并静默归位 Idle。
        self.set_quiet(LooperState::Idle);

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

    // ── 用户消息处理 ──────────────────────────────────────────────────────

    /// 阻塞取一条用户消息，随后非阻塞排空同批剩余。
    ///
    /// **「阻塞后补排空」不可省**：只阻塞收一条会让一批 `[Q1, Q2, Q3]` 一条占一个
    /// 迭代，退化成「Q1 一轮 + Q2Q3 续接一轮」。`StopReading`（Cancel / Shutdown）时
    /// 后面的消息解释前提已变，一律丢弃，故随时直接返回、不再排空。
    ///
    /// 返回 `false` = 通道已关闭（所有 `Speaker` 被 drop）。
    async fn recv_user_msgs(&mut self, user_listener: &mut Listener<UserMsg>) -> bool {
        let Some(msg) = user_listener.recv().await else {
            return false;
        };
        if matches!(self.handle_user_msg(msg), ReadDirective::StopReading) {
            return true;
        }
        self.drain_user_msgs(user_listener);
        true
    }

    /// 非阻塞排空积压用户消息，直到 Empty / Disconnected / `StopReading`。
    fn drain_user_msgs(&mut self, user_listener: &mut Listener<UserMsg>) {
        use tokio::sync::mpsc::error::TryRecvError;
        loop {
            match user_listener.try_recv() {
                Ok(msg) => {
                    if matches!(self.handle_user_msg(msg), ReadDirective::StopReading) {
                        return;
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        }
    }

    /// 还有可推进的工作 → 不阻塞，非阻塞取消息后立刻干活。`run()` 里唯一的调度
    /// 开关，也是终止性的关键（见其「不空转不变量」）。运行位那项
    /// 不可省：少了它，③ 启动一轮后回顶就阻塞在 `recv()` 上，整轮停摆。
    fn has_pending_work(&self) -> bool {
        match self.state {
            // 真暂停只等消息（阻塞 recv 才能被 Resume / Cancel 唤醒）
            LooperState::PausedDuringTools | LooperState::PausedBeforeRequest => false,
            // 停靠：只在有取消闩或待跑 pending 时不阻塞
            LooperState::Idle => {
                self.is_cancelled() || (self.pending_armed && self.session.has_pending())
            }
            // 运行位（含 Failed：等 ② 收尾）必须能推进
            _ => true,
        }
    }

    /// 处理一条用户消息 —— 消费点共用的唯一分发，同步、零 await 重活。
    ///
    /// 三条不变量：① 只有 `Query` 触碰 Session 内容，其余四条只动 flag /
    /// `state` / `pause_pending`；② `StopReading` 的判据 = 本条消息改变了后续
    /// 消息的解释前提，只有 `Cancel` / `Shutdown` 满足（见 [`ReadDirective`]）；
    /// ③ `pending_armed` 由用户意图驱动 —— `Resume` / 新 `Query` 表示「想要它跑」，
    /// `Cancel` 表示「停」。
    fn handle_user_msg(&mut self, msg: UserMsg) -> ReadDirective {
        match msg {
            UserMsg::Query(content) => {
                self.session.enqueue_pending(content);
                self.pending_armed = true;
                ReadDirective::Continue
            }
            UserMsg::Pause => {
                match self.state {
                    // 内层在途：记录意图，等边界落地
                    LooperState::PreparingRequest
                    | LooperState::ResolvingResponse
                    | LooperState::Streaming
                    | LooperState::ExecutingTools => self.pause_pending = true,
                    // 空闲 / 已完成 / 已失败 / 已暂停：无操作（空闲不响应挂起；
                    // 已暂停幂等）
                    _ => {}
                }
                ReadDirective::Continue
            }
            UserMsg::Resume => {
                match self.state {
                    LooperState::PausedDuringTools => {
                        self.pending_armed = true;
                        // spawn 阶段重入（active_tool_tasks 仍为 None）
                        self.set(LooperState::ExecutingTools);
                    }
                    LooperState::PausedBeforeRequest => {
                        self.pending_armed = true;
                        self.set(LooperState::PreparingRequest);
                    }
                    // 非暂停态：撤回尚未落地的暂停意图（运行位收到 Pause 后、
                    // 落地前，用户可再点一次撤销）；无挂起意图时无操作。
                    _ => self.pause_pending = false,
                }
                ReadDirective::Continue
            }
            UserMsg::Cancel => {
                // 只置闩，收尾交给 ②（finalize_cancel 继承已有的 failure_reason）。
                self.cancel_request.store(true, Ordering::Release);
                ReadDirective::StopReading
            }
            UserMsg::Shutdown => {
                self.shutdown_requested = true;
                ReadDirective::StopReading
            }
        }
    }

    /// 取消收尾 —— 中止当前 ReAct 轮，**looper 继续运行**，收尾后停在干净 Idle。
    /// 三条来源共用：消息路径 [`UserMsg::Cancel`]、handle 被 drop 的安全网标志、
    /// 暂停中通道关闭。
    async fn finalize_cancel(&mut self) {
        // ① 按状态回收在途资源（各状态的差异只在这一处）
        match self.state {
            // `active_stream` 握着活的 HTTP 连接 —— 取消不再 drop looper，不显式
            // 丢就是跨轮连接泄漏。
            LooperState::Streaming => self.active_stream = None,
            // 丢弃已收敛但尚未解析的响应。
            LooperState::ResolvingResponse => self.react_ctx.resolved = None,
            LooperState::ExecutingTools => self.abort_inflight_tools(),
            // 两个暂停态：落地时未 spawn / 请求未发出，无在途任务，无需回收
            // （这是落地点 B 只在 spawn 前生效的直接红利：暂停态永远不握活
            // 连接、不握 JoinSet）。其余状态无在途资源（PreparingRequest 的
            // retry_deadline 由 reset_iteration_counters 清）。
            _ => {}
        }
        // 取消优先：暂停意图作废
        self.pause_pending = false;

        // ② 冻结 + 强制落盘。有在途轮时**继承**已有失败原因（取消一个已失败的轮不该
        //    改写成 Cancelled）。`had_inflight` 必须在此刻取 —— finalize 会改变 session。
        let had_inflight = self.has_inflight_turn();
        let reason = if had_inflight {
            self.failure_reason
                .take()
                .unwrap_or(TurnFailureReason::Cancelled)
        } else {
            TurnFailureReason::Cancelled
        };
        // 取消路径不合成收尾：收尾器只服务 `max_iterations`（见
        // `compose_epilogue_if_needed`），且此刻用户已经在等停。
        self.apply_failure(reason.clone(), /* force_persist */ true, None)
            .await;
        // 无条件回填：`apply_failure` 在「无在途轮」时不回写原因，
        // 不补这句 `shutdown_reason` 就退化成 "done"，看不出是取消。
        self.failure_reason = Some(reason);

        // ③ 补落盘：无在途轮时 finalize 产不出 plan，也就不会落盘 —— 而 pending
        //    可能正排在内存里。取消不丢 pending，所以补一次。
        if !had_inflight && self.session.has_pending() {
            self.persist_boundary_snapshot().await;
        }

        // ④ 归位到干净 Idle —— 留着幽灵 Paused 会让后续消息只入队不启动。
        // 状态与镜像统一由 `set` 写入；已 Idle 时 `prev == next` 早退。
        self.pending_armed = false;
        self.cancel_request.store(false, Ordering::Release); // 一次性，否则回到 Idle 会反复命中
        self.set(LooperState::Idle);
    }

    /// 失败收尾。续接交给 `run()` 的 ③ —— 收尾的契约是「把状态搬到可推进的位置」，
    /// 不回报布尔让调用方决定续接。
    ///
    /// 撞上 `max_iterations` 时在冻结**之前**补一次收尾报告合成（见
    /// [`Self::compose_epilogue_if_needed`]）—— 唯一的 `.await` 落点：`plan_failure`
    /// 是同步自由函数，而收尾文本必须赶在它之前备好。
    async fn finalize_failure(&mut self) {
        // 终局失败：该轮已判死、无可暂停对象，用户的暂停意图就此丢弃。若此时
        // session 里已排了 Query，失败收尾回到 Idle 后 ③ 会**不带暂停**地起下一轮。
        self.pause_pending = false;
        let reason = self
            .failure_reason
            .take()
            .unwrap_or(TurnFailureReason::Other("failed".into()));
        let epilogue = self.compose_epilogue_if_needed(&reason).await;
        self.apply_failure(reason, /* force_persist */ false, epilogue)
            .await;
    }

    /// 撞上 `max_iterations` 时合成收尾报告，其余失败原因一律返回 `None`。
    ///
    /// 三个前置守卫（任一不满足即不合成，行为与不装收尾器时逐字节一致）：
    /// 原因必须是 `MaxIterationsExceeded`（`Cancelled` / 瞬时故障没有「还没做完」可言）、
    /// 必须装配了收尾器、必须确有在途轮（与 `plan_failure` ① 同判据）。
    ///
    /// 结果只影响「多不多一条 assistant 消息」。失败、空产出、超时、被取消
    /// 都退回 `None`，由 [`Session::interrupt_turn`] 走原有的固定中断说明。
    async fn compose_epilogue_if_needed(&mut self, reason: &TurnFailureReason) -> Option<String> {
        if !matches!(reason, TurnFailureReason::MaxIterationsExceeded) {
            return None;
        }
        let summarizer = self.config.epilogue.clone()?;
        if !session_has_inflight(&self.session) {
            return None;
        }

        // `staging_all` 而非 `staging_messages`：转录要含本轮的用户提问，
        // 否则模型看不出这轮在回答什么。
        let transcript = super::compaction::build_flat_transcript(&self.session.staging_all());

        match Self::compose_epilogue_cancellable(&summarizer, &transcript).await {
            Some(text) if !text.trim().is_empty() => {
                info!(chars = text.chars().count(), "Turn epilogue composed");
                Some(text)
            }
            Some(_) => {
                warn!("Turn epilogue was empty; falling back to fixed interrupt notice");
                None
            }
            // 具体原因已由 `compose_epilogue_cancellable` 记过日志，此处不重复
            None => None,
        }
    }

    /// 带超时地调一次收尾合成。
    ///
    /// `Some(text)` = 合成成功；`None` = 失败/超时，调用方一律回退。
    ///
    /// 超时是硬上界 —— 也是这段路径唯一需要的边界：`run()` 此刻不在循环顶，
    /// 消息通道不被排空，取消要等它返回才看得见（见 [`EPILOGUE_TIMEOUT`]）。
    /// 不再额外观察取消标志：`Cancel` 本就排在通道里等着，这里主动放弃合成既
    /// 省不下延迟，又要多养一条并发臂。
    ///
    /// 取 `&Arc<dyn TurnSummarizer>` 而非 `&self`：`AgentLooper` 不 `Sync`，
    /// `&self` 跨 await 会让 `run()` 的 future 失去 `Send`（同
    /// [`Self::save_snapshot`] 的理由）。
    async fn compose_epilogue_cancellable(
        summarizer: &Arc<dyn super::compaction::TurnSummarizer>,
        transcript: &str,
    ) -> Option<String> {
        // 「没合成出来」在此记一次日志 —— 调用方只认 Some/None，不重复记账
        // （否则超时会既 warn 又 error）。
        let work = async {
            match summarizer.summarize_inflight(transcript).await {
                Ok(text) => Some(text),
                Err(e) => {
                    error!(error = %e, "Turn epilogue composition failed (non-fatal)");
                    None
                }
            }
        };

        match tokio::time::timeout(EPILOGUE_TIMEOUT, work).await {
            Ok(result) => result,
            Err(_) => {
                warn!(
                    timeout_secs = EPILOGUE_TIMEOUT.as_secs(),
                    "Turn epilogue timed out"
                );
                None
            }
        }
    }

    fn has_inflight_turn(&self) -> bool {
        session_has_inflight(&self.session)
    }

    /// 按会话元数据落盘一份快照 —— 轮提交、压缩后、失败收尾、取消补落盘四处
    /// 共用，元数据字段只在此处拼装。
    ///
    /// 取 `&mut self` 而非 `&self`：`AgentLooper` 不 `Sync`，`&self` 跨 await
    /// 会让 `run()` 的 future 失去 `Send`。
    async fn save_snapshot(
        &mut self,
        snapshot: &SessionSnapshot,
    ) -> Result<PersistResult, PersistError> {
        self.persister
            .save(
                snapshot,
                self.session.id(),
                self.session.description(),
                self.session.created_at(),
            )
            .await
    }

    /// 无在途轮时的补落盘。
    ///
    /// `snapshot` 需要 `TurnBoundaryToken`，而 `rollback_turn` 是唯一「无 state
    /// 守卫、必回 Idle」的 token 来源。此处只在 Idle + staging 已清时调用，是幂等
    /// 空操作 —— 只为取 token。
    async fn persist_boundary_snapshot(&mut self) {
        let token = match self.session.rollback_turn(false) {
            Ok(t) => t,
            Err(e) => {
                error!(error = %e, "Failed to obtain boundary token; pending not persisted");
                return;
            }
        };
        let snapshot = self.session.snapshot(&token);
        if let Err(e) = self.save_snapshot(&snapshot).await {
            error!(error = %e, "Failed to persist pending inputs at cancel boundary");
        }
    }

    /// 回收在途工具任务：先非阻塞捞回已完成的写进 `pending_tool_calls`，再 abort
    /// 其余。`active_tool_tasks` 为 `None` 时 no-op（只有 `ExecutingTools` 持有任务）。
    fn abort_inflight_tools(&mut self) {
        let Some(mut joinset) = self.active_tool_tasks.take() else {
            return;
        };
        let drained =
            Self::drain_finished_join_results(&mut joinset, &mut self.react_ctx.pending_tool_calls);
        if drained > 0 {
            debug!(drained, "Salvaged completed tool results before abort");
        }
        joinset.abort_all();
    }

    // ── 轮次启动 ──────────────────────────────────────────────────────────

    /// **唯一**的「pending → turn」入口。
    ///
    /// 失败即解除武装 —— 否则 `has_pending_work` 的「`pending_armed && has_pending`」
    /// 无人消解会让 `run()` 空转（见其「不空转不变量」）。启动失败时 pending 原封
    /// 不动（`dequeue_and_start_turn` 的前置检查保证），下一句输入会重新武装。
    fn start_turn_from_pending(&mut self) {
        match self.session.dequeue_and_start_turn() {
            Ok(true) => {}
            Ok(false) | Err(_) => {
                // Ok(false) 是「队列空了」的竞态；Err 是 Session 非 Idle 的
                // 状态守卫失败（在途轮尚未收尾）。两者都不该重试。
                self.pending_armed = false;
                return;
            }
        }

        // ★ 新对话轮次：重置 ReAct 循环计数
        self.reset_iteration_counters();
        self.failure_reason = None;
        self.pending_armed = false;

        // Idle → PreparingRequest；新轮不继承旧的暂停意图（`reset_iteration_counters`
        // 已清 `pause_pending`）。
        self.set(LooperState::PreparingRequest);

        // 事件面保持文本视图；部件整体已在 Session 里（合并后为一条 user 消息）
        let user_input = match self
            .session
            .staging_user_input()
            .map(|am| am.message.as_ref())
        {
            Some(InputItem::Message { content, .. }) => content.text_view().into_owned(),
            _ => String::new(),
        };

        self.emit_event(LooperEvent::TurnStart {
            turn_index: self.session.turn_index(),
            user_input,
        });
    }

    // ── ReAct 状态机步进 ─────────────────────────────────────────────────

    /// 执行状态机的一步。
    ///
    /// 仅在 `is_active()`（运行位，含 `Failed` 的 no-op 分支）时由 `run()`
    /// 循环调用（分派点守卫）；停靠/暂停时 `run()` 阻塞在 `recv()` 上，不推进。
    ///
    /// 两个暂停落地检查点都在本函数入口：
    /// - **落地点 A**（`PreparingRequest`）：覆盖首开发前、瞬时重发前
    ///   （`begin_retry` 把状态放回 `PreparingRequest`）、工具批完成后下一次
    ///   请求前（`finish_tool_execution` 置 `PreparingRequest`）三条到达路径；
    /// - **落地点 B**（`ExecutingTools` 的 spawn 阶段前）：覆盖
    ///   `ResolvingResponse` 解析出 tool_calls 后的到达。poll 阶段
    ///   （`active_tool_tasks` 为 Some）不打断在途批。
    ///
    /// 落地后 `set` 返回，本轮步进结束回 ①；`has_pending_work` 对两个暂停态
    /// 均为 `false`（阻塞等 `recv()`），无热自旋。
    async fn react_step(&mut self) {
        let old_state = self.state;
        // ★ Session 零锁：turn_index() 是字段访问，无需缓存
        let turn_index = self.session.turn_index();

        match self.state {
            // ── 落地点 A：请求发出前 ──
            LooperState::PreparingRequest => {
                if self.pause_pending {
                    self.pause_pending = false;
                    self.set(LooperState::PausedBeforeRequest); // 不发请求
                } else {
                    self.prepare_and_send_request(turn_index).await;
                }
            }

            // ── batch / stream 共用：响应解析 ──
            LooperState::ResolvingResponse => {
                self.resolve_response(turn_index).await;
            }

            // ── streaming 分支 ──
            LooperState::Streaming => {
                self.consume_stream_chunk(turn_index).await;
            }

            // ── 落地点 B：工具 spawn 前 ──
            LooperState::ExecutingTools => {
                // spawn 阶段（active_tool_tasks.is_none()）才检查；
                // poll 阶段不打断在途批
                if self.pause_pending && self.active_tool_tasks.is_none() {
                    self.pause_pending = false;
                    self.set(LooperState::PausedDuringTools); // 不 spawn
                } else {
                    self.execute_tools_step(turn_index).await;
                }
            }

            LooperState::Done => {
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
                        self.set(LooperState::Failed);
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
                        turn_index,
                        outcome: outcome.clone(),
                        usage: usage.clone(),
                    },
                )
                .await;
                Self::invoke_on_turn_complete(
                    &self.config.hooks,
                    turn_index,
                    outcome.failure_reason(),
                    &usage,
                    &self.session,
                )
                .await;

                // ★ 持久化：turn 边界触发（commit 后）
                let snapshot = self.session.snapshot(&_token);
                if let Err(e) = self.save_snapshot(&snapshot).await {
                    error!(error = %e, "Failed to persist session after turn commit");
                }
                // ★ 本轮已入史，在途检查点作废（早于压缩：压缩只改 committed 内容）。
                self.clear_inflight_checkpoint().await;

                // ★ 上下文滚动压缩：turn 边界（提交并持久化后、pending 续接前）。
                //   编排（削减扫描 → 折叠 → 观测结算）在 apply_boundary 内完成；
                //   非致命：失败只记日志，不影响会话继续（此处不能用 `?` —— 既有契约）。
                if let Some(policy) = &self.config.compaction {
                    match policy.apply_boundary(&mut self.session).await {
                        Ok(r) => {
                            // ① 折叠事件（折叠载荷来自本次折叠）
                            if let Some(outcome) = &r.compaction {
                                // 降级摘要（摘要模型连续失败后的机械兜底）走 warn：
                                // 驱逐照常发生，但 pinned 摘要已退化为转录碎片，必须能被看见。
                                if outcome.degraded {
                                    warn!(
                                        evicted_turns = outcome.evicted_turns,
                                        tokens_before = outcome.estimated_tokens_before,
                                        tokens_after = outcome.estimated_tokens_after,
                                        degraded = outcome.degraded,
                                        "Context compacted with degraded summary at turn boundary"
                                    );
                                } else {
                                    info!(
                                        evicted_turns = outcome.evicted_turns,
                                        tokens_before = outcome.estimated_tokens_before,
                                        tokens_after = outcome.estimated_tokens_after,
                                        degraded = outcome.degraded,
                                        "Context compacted at turn boundary"
                                    );
                                }
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
                            }

                            // ② 落盘：只看生效值（折叠改变了 committed 与 pinned；
                            //    削减评估值不改会话，为其落盘纯属浪费 IO）
                            if r.mutated() {
                                let snapshot = self.session.snapshot(&_token);
                                if let Err(e) = self.save_snapshot(&snapshot).await {
                                    error!(error = %e, "Failed to persist session after compaction");
                                }
                            }

                            // ③ 折叠钩子（持久化完成后通知，纯观察）
                            if let Some(outcome) = &r.compaction {
                                Self::invoke_on_context_compacted(&self.config.hooks, outcome)
                                    .await;
                            }
                            // ④ 边界观测钩子（未折叠的边界也要发 — shadow 每边界一行）
                            Self::invoke_on_boundary_reduction(
                                &self.config.hooks,
                                &self.session,
                                &r,
                            )
                            .await;
                        }
                        Err(e) => {
                            error!(error = %e, "Context compaction failed (non-fatal)");
                        }
                    }
                }

                // 提交完就交还循环 —— 续接排队输入由 `run()` 的 ③ 决定。`commit_turn`
                // 已把 session 置回 Idle，此处只需把 looper 带回停靠位。
                // 事件已随 `set` 发出；hook 在步尾统一调用。
                self.set(LooperState::Idle);
            }

            LooperState::Failed => {
                // 纯同步 no-op，收尾交给 `run()` 循环顶 —— task 从不被 abort，
                // 收尾两段（冻结 + 落盘）都能跑完；在此 await 会把异步尾巴拉进
                // react_step 的调用栈，模糊「步进」与「收尾」的边界。
                return;
            }

            // ④ 的分派守卫（`is_active()`）已排除停靠与暂停态；此处为
            // match 穷举而保留，真实出现即为调度 bug。
            LooperState::Idle
            | LooperState::PausedDuringTools
            | LooperState::PausedBeforeRequest => {
                warn!(state = ?self.state, "Unexpected docked/paused state in react_step");
                return;
            }
        }

        // 步内状态有迁移时调用合并后的 hook（事件已随每次 `set` 即时发出）
        if old_state != self.state {
            Self::invoke_on_state_change(&self.config.hooks, old_state, self.state).await;
        }
    }

    // ── 统一重试 ────────────────────────────────────────────────────────

    /// 新一轮对话开始时重置「本用户轮」的运行时状态。
    ///
    /// `react_loop_iteration`、`retries_used`、`budget_raised`、
    /// `retry_deadline`、`dynamic_context_resolved` 与 `pause_pending`
    /// 的生命周期都恰好是一个用户轮 —— 抽成方法就是为了让这条不变量无法被单独违反。
    /// `budget_raised` 尤其不能跨轮：粘性一旦泄漏，下一轮的截断重试会
    /// 被第 4 条判据误拦（生效预算看似已抬过）。
    fn reset_iteration_counters(&mut self) {
        self.react_loop_iteration = 0;
        self.retries_used = 0;
        self.budget_raised = false;
        self.retry_deadline = None;
        self.dynamic_context_resolved = false;
        // 新轮启动时丢弃未落地的暂停意图（流收敛无 tool_calls 时的「吸收」
        // 也靠这里兜底清零 —— 意图活不过一个用户轮）
        self.pause_pending = false;
    }

    /// 本用户轮已实际发起的模型调用次数（含刚失败的这次）。
    ///
    /// `react_loop_iteration` 在 [`Self::prepare_and_send_request`] 入口递增，
    /// 失败发生时它已包含当前尝试 —— 正是失败原因里 `attempts` 要报的数。
    fn total_attempts(&self) -> u32 {
        self.react_loop_iteration as u32
    }

    /// 本次重发是否应该发生。纯判定，无副作用，两种原因共用，判据顺序固定。
    ///
    /// 1. 次数上限（[`LooperConfig::retry_limit`]，`0` = 关闭）——
    ///    全部原因共用单计数，先查 limit 再查 headroom：headroom 判据
    ///    只会拒绝、不能放行。
    /// 2. 用户按了停止 —— 不再发起新的模型调用。
    /// 3. 迭代预算：重发要重新走 `prepare_and_send_request`，会消耗一次
    ///    `react_loop_iteration`。没有下一次调用额度时不重发 —— 否则状态机
    ///    刚被置回 `PreparingRequest` 就撞上 `MaxIterationsExceeded`，把「截断」
    ///    或「限流」这个真实原因换成「超出迭代次数」，诊断信息反而变差。
    /// 4. **仅截断**需要抬得动预算：抬不动（当前生效预算已 ≥
    ///    `retry_output_budget`）⇒ 重发与上一次逐字节相同，必然同样截断，
    ///    白烧一次调用。比的是**当前生效**预算而非配置值 —— 抬过一次后
    ///    再比配置值会误放行第二次注定失败的重试。未配置 `max_tokens` 时
    ///    按 0 计（provider 服务端默认远低于抬升目标），不会误判。
    ///
    /// 截断因此不需要独立限额：`budget_raised` 粘性到轮末，抬到
    /// `retry_output_budget` 后生效预算恒 ≥ 该值，第 4 条恒假。
    fn can_retry(&self, cause: &RetryCause) -> bool {
        if self.retries_used >= self.config.retry_limit as usize {
            return false;
        }
        if self.is_cancelled() {
            return false;
        }
        if self.react_loop_iteration >= self.max_iterations {
            return false;
        }
        if matches!(cause, RetryCause::Truncated { .. })
            && self.effective_output_budget() >= self.config.retry_output_budget
        {
            return false;
        }
        true
    }

    /// 本次请求实际会用的输出预算。把「一次性覆盖 → 配置值 → 未设」归一成一个数。
    ///
    /// 未设时按 0 计 —— 仅用于比较（见 [`Self::can_retry`] 第 4 条），
    /// 不用于发请求：发请求时 `None` 表示交给 provider 服务端默认值。
    fn effective_output_budget(&self) -> u32 {
        self.max_output_tokens_override()
            .or(self.agent.model_config().max_tokens)
            .unwrap_or(0)
    }

    /// 本 looper 当前生效的输出预算覆盖值（`None` = 沿用 `ModelConfig::max_tokens`）。
    ///
    /// **粘性**：本轮发生过截断重试（`budget_raised` 置位）后，后续 ReAct
    /// 迭代继续用抬高的预算。否则 tool 调用之后的下一次迭代可能以同样的
    /// 方式再截断一次，把刚省下的那次调用又浪费掉。随 [`Self::reset_iteration_counters`]
    /// 归零，粘性不出轮。瞬时重发不置位本标志 —— 它原样重发，抬预算可能
    /// 超出模型真实上限（网关 400）。
    fn max_output_tokens_override(&self) -> Option<u32> {
        self.budget_raised
            .then_some(self.config.retry_output_budget)
    }

    /// 单一重发入口（截断与瞬时共用）。回退 staging 到本次请求前的锚点，
    /// 计数，设退避截止时刻，打回 `PreparingRequest`。
    ///
    /// 返回 `false` 表示不重发 —— 调用方走既有失败路径。
    /// 返回 `true` 时状态已置回 [`LooperState::PreparingRequest`]，
    /// 下一次 `react_step()` 会重新走 [`Self::prepare_and_send_request`]。
    ///
    /// # 为什么必须回退 staging
    /// 两条非完成路径都是「先 [`Self::stage_output_blocks`] 再判状态」，被
    /// 丢弃的块已经写进 staging。不回退就重发，两次尝试的 assistant 块会
    /// 叠在一起，直接违反 provider 线格式不变量（有 tool 结果待配对期间
    /// 不得出现 assistant Message）。回退到请求前的下标，语义上等价于
    /// 「这次模型调用从未发生」—— 回退后 staging 的结尾与发请求前逐字节
    /// 相同，而那个状态本身是合法的（它来自上一次成功步骤）。
    ///
    /// # 为什么不会死循环、为什么不吞掉迭代预算
    /// 重发要重新走 `prepare_and_send_request`，因此**消耗一次
    /// `react_loop_iteration`**：重发是一次真实的模型调用，不占额度会让
    /// `max_iterations` 失去意义。次数另受 [`LooperConfig::retry_limit`] 约束，
    /// 两个上限都有界 ⇒ 不可能死循环。
    ///
    /// # 不设 `failure_reason`
    /// 本路径是**恢复**不是失败。故意不碰 `failure_reason`：重发最终跑到
    /// `Done` 时会撞上 `Done` 分支的 `debug_assert!(failure_reason.is_none())`。
    ///
    /// # 锚点由调用方在收敛点就地取
    /// `checkpoint` 是本次模型请求发出前 staging 的条数，由调用方在
    /// [`Self::stage_output_blocks`] 之前就地读取。
    ///
    /// # 不刷新在途检查点
    /// 锚点即 `Session::staging_checkpoint()`，与 [`stage_tool_results`]
    /// 落盘的 `InflightCheckpoint` 在同一口径上，且取锚点时常无其他 staging
    /// 写入，即 `锚点 == 检查点条数`。被丢弃的消息从不属于检查点，落盘的
    /// 检查点依然精确；反而若在此重写，会把已落地的工具结果从检查点抹掉。
    ///
    /// # 退避 deadline 只读不清
    /// 退避不在本函数内 sleep —— 只记 `retry_deadline`，由
    /// [`Self::prepare_and_send_request`] 在下次请求发出前经
    /// [`Self::wait_retry_backoff`] 以可取消的切片 sleep 等待（回退与通知
    /// 立即发生，取消检查天然衔接入口守卫）。deadline 只读不 `take()`，
    /// 理由见 [`Self::wait_retry_backoff`]。
    ///
    /// # 为什么发 [`LooperEvent::TruncationRetry`] 而不回退传输层
    /// 已下发的增量留在实时视图里，由一条通知解释它们为什么不属于最终答案。
    /// 用 `emit_event_guaranteed` 而非 `try_send`：丢一条通知 = 画面上多出
    /// 一段无法解释的残句、本特性整个没发生（同
    /// [`LooperEvent::ContextCompacted`] 的处置）。
    async fn begin_retry(
        &mut self,
        cause: RetryCause,
        turn_index: usize,
        checkpoint: usize,
    ) -> bool {
        if !self.can_retry(&cause) {
            return false;
        }

        let (dropped, discarded_text, had_output) = match self.rollback_attempt(checkpoint) {
            Some(r) => r,
            None => return false,
        };

        self.retries_used += 1;
        self.budget_raised |= matches!(cause, RetryCause::Truncated { .. });

        // 统一指数退避：base * 2^(n-1)，截断到上限。截断重发也等 ——
        // 少一条分支，代价是一次 500ms 起的等待，相对一次完整模型调用可忽略。
        let delay_ms = self
            .config
            .retry_base_delay_ms
            .saturating_mul(1u64 << (self.retries_used - 1).min(16))
            .min(self.config.retry_max_delay_ms);
        self.retry_deadline = Some(Instant::now() + Duration::from_millis(delay_ms));

        self.set(LooperState::PreparingRequest);
        // `trigger` 是故障类别的日志字段，显式取一次进 warn（Debug 派生
        // 不计入 dead_code 分析，只写 `cause = ?cause` 会让字段被判为未读）。
        let trigger = match cause {
            RetryCause::Transient { trigger } => trigger,
            RetryCause::Truncated { .. } => "max_tokens",
        };
        warn!(
            turn_index,
            cause = ?cause,
            trigger,
            dropped_staging_messages = dropped,
            attempt = self.retries_used,
            limit = self.config.retry_limit,
            delay_ms,
            "Retry scheduled"
        );

        // 通知必须在状态置回 `PreparingRequest` **之后**发：同一 Speaker FIFO
        // 保证它早于重发尝试的第一条 TextDelta，前端插的横幅因此落在残句与
        // 新正文之间。`discarded_text` 取自 `rollback_attempt` 的 `take`，
        // 是唯一真相源（丢弃指令，非保存）。
        //
        // 零产出（什么都没吐就故障了）不发：没有需要解释的残句，发出去只会
        // 让前端插一条无上下文的横幅。
        if had_output {
            Self::emit_event_guaranteed(
                &self.event_speaker,
                LooperEvent::TruncationRetry {
                    turn_index,
                    attempt: self.retries_used as u32,
                    limit: self.config.retry_limit,
                    output_tokens: cause.retry_notice_output_tokens(),
                    retry_budget: self.effective_output_budget(),
                    discarded_text,
                    reason: cause.retry_notice_reason(),
                },
            )
            .await;
        }
        true
    }

    /// 统一的模型故障出口。返回 `true` = 已安排重发（调用方直接 `return`）；
    /// 返回 `false` = 已写入 `failure_reason`，调用方置 [`LooperState::Failed`]。
    ///
    /// 只收 `&ProviderError`：三处站点的错误类型不同（两处请求是 `AgentError`、
    /// 流中 Err 是 `ProviderError` 直出），非 Provider 分支留在请求站点，
    /// 不进本函数。
    ///
    /// 分类只做一次（`classify()` 在此调用，`ClassifiedError` 按值传给
    /// [`model_failure_reason`]）—— 门控与失败原因共用同一份分类结果。
    async fn fail_or_retry(
        &mut self,
        e: &model_provider::ProviderError,
        turn_index: usize,
        checkpoint: usize,
    ) -> bool {
        let classified = e.classify();
        if classified.kind.is_transient()
            && self
                .begin_retry(
                    RetryCause::Transient {
                        trigger: classified.kind.as_str(),
                    },
                    turn_index,
                    checkpoint,
                )
                .await
        {
            return true;
        }
        self.failure_reason = Some(model_failure_reason(classified, self.total_attempts()));
        false
    }

    /// 回退本次模型尝试的公共骨架：staging 回退到锚点 + 清运行态。
    ///
    /// 全部重发原因共用（[`Self::begin_retry`]）。返回
    /// `(staging 回退条数, 丢弃的正文, 是否有产出)`；staging 回退失败
    /// （状态非 Active / 锚点越界）返回 `None` —— 此时 staging 未被改动，
    /// 调用方走既有失败路径，与无此特性完全一致。
    ///
    /// 被回退的正文**只丢弃，不归还**：它作为 `discarded_text` 随
    /// `TruncationRetry` 事件即发即弃（落库侧按后缀剥离用），不写入任何
    /// 字段 —— 失败收尾的历史由 [`plan_failure`] 的存活桩兜底。
    fn rollback_attempt(&mut self, checkpoint: usize) -> Option<(usize, String, bool)> {
        let dropped = match self.session.truncate_staging(checkpoint) {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, "Attempt rollback failed; not retrying");
                return None;
            }
        };

        let discarded_text = std::mem::take(&mut self.react_ctx.assistant_text);
        let discarded_reasoning = std::mem::take(&mut self.react_ctx.assistant_reasoning);
        let had_output = !discarded_text.is_empty() || !discarded_reasoning.is_empty();
        self.react_ctx.pending_tool_calls.clear();
        self.react_ctx.resolved = None;

        Some((dropped, discarded_text, had_output))
    }

    /// 退避等待：等到 `retry_deadline`（若有），每 ~200ms 切片查一次取消标志，被
    /// 打断立刻返回 `false`；无 deadline 时 no-op。过期 deadline 循环条件天然为
    /// 假，无需在此清除（由 [`Self::begin_retry`] 覆写、`reset_iteration_counters` 清零）。
    ///
    /// 打断只可能来自 handle 被 drop 的安全网 —— 消息路径的取消在步内不可见
    /// （Cancel 要先被 drain 消费才置位标志，步内没有 drain）。故 `false` 不是
    /// 「用户按了停」，而是「没人再要这轮结果」，放弃发送即可。
    ///
    /// **只读不 `take()`**：取走 deadline 会把它带进 future 栈帧，future 被 drop
    /// 即丢失，重建后零退避立即重发。留在 `self` 上则重入时继续等**剩余**时间。
    async fn wait_retry_backoff(&mut self) -> bool {
        let Some(deadline) = self.retry_deadline else {
            return true;
        };
        while Instant::now() < deadline {
            if self.is_cancelled() {
                return false;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let slice = remaining.min(STEP_POLL);
            tokio::time::sleep(slice).await;
        }
        true
    }

    // ── PreparingRequest — 分叉点 ────────────────────────────────────────

    /// 准备请求并分叉到 batch 或 streaming 路径。
    async fn prepare_and_send_request(&mut self, turn_index: usize) {
        // 0. 瞬时重发的退避等待（无 deadline 时 no-op）。被打断 = handle 已
        //    drop，发出去也没人接 —— 直接返回，收尾交给循环顶。
        if !self.wait_retry_backoff().await {
            return;
        }

        // 1. 检查 max_iterations 限制（限制当前对话轮次内的模型调用次数）
        if self.react_loop_iteration >= self.max_iterations {
            self.failure_reason = Some(TurnFailureReason::MaxIterationsExceeded);
            self.set(LooperState::Failed);
            return;
        }
        // ★ 递增模型调用计数
        self.react_loop_iteration += 1;

        // 2. 构建消息列表（Agent 层上下文策略，零拷贝 Arc 共享）
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
            Self::invoke_on_before_request(&self.config.hooks, turn_index, &mut session_messages)
                .await
        {
            self.failure_reason = Some(TurnFailureReason::HookAbort(reason));
            self.set(LooperState::Failed);
            return;
        }

        // 3. 分支：根据 ModelConfig.stream 决定路径
        let use_stream = self.agent.model_config().stream.unwrap_or(false);

        // 请求前估算 token — 响应到达后经 [`Self::log_estimate_calibration`]
        // 与实际 usage 对照。
        let estimated_request_tokens = estimate_str_tokens(&effective_prompt)
            + session_messages
                .iter()
                .map(|m| estimate_item_tokens(m))
                .sum::<usize>();
        self.last_request_estimated_tokens = Some(estimated_request_tokens);
        debug!(
            turn_index,
            estimated_request_tokens, "Request token estimate"
        );

        if use_stream {
            // ── Streaming 路径 ──
            let budget = self.max_output_tokens_override();
            match self
                .agent
                .generate_stream(
                    session_messages,
                    Some(effective_prompt),
                    override_output_budget(budget),
                )
                .await
            {
                Ok(stream) => {
                    self.active_stream = Some(stream);
                    self.stream_assembler = BlockAssembler::new();
                    self.set(LooperState::Streaming);
                }
                Err(e) => {
                    error!(error = %e, "Streaming generate request failed");
                    // 瞬时类（限流/网络/5xx）→ 退避后原样重发；
                    // 永久类（Auth/额度/上下文溢出…）→ 类型化失败原因。
                    // 此处 staging 未被本次尝试写入（阶段不变量），锚点即当前长度。
                    if let AgentError::Provider(pe) = &e {
                        if self
                            .fail_or_retry(pe, turn_index, self.session.staging_checkpoint())
                            .await
                        {
                            return;
                        }
                    } else {
                        self.failure_reason = Some(TurnFailureReason::Other(format!(
                            "Streaming request failed: {e}"
                        )));
                    }
                    self.set(LooperState::Failed);
                }
            }
        } else {
            // ── Batch 路径 ──
            let tools = self.agent.tool_executor().definitions();
            let budget = self.max_output_tokens_override();
            match self
                .agent
                .generate_with_tools(
                    session_messages,
                    Some(effective_prompt),
                    tools,
                    override_output_budget(budget),
                )
                .await
            {
                Ok(response) => {
                    self.react_ctx.resolved = Some(ResolvedResponse {
                        result: response,
                        origin: ResponseOrigin::Batch,
                    });
                    self.set(LooperState::ResolvingResponse);
                }
                Err(e) => {
                    error!(error = %e, "Batch generate request failed");
                    // 同流式路径：瞬时类退避重发，永久类类型化失败。
                    if let AgentError::Provider(pe) = &e {
                        if self
                            .fail_or_retry(pe, turn_index, self.session.staging_checkpoint())
                            .await
                        {
                            return;
                        }
                    } else {
                        self.failure_reason = Some(TurnFailureReason::Other(format!(
                            "Generate request failed: {e}"
                        )));
                    }
                    self.set(LooperState::Failed);
                }
            }
        }
    }

    // ── 共享：ResolvingResponse ───────────────────────────────────────────

    /// 解析已收敛的模型响应，决定下一步。batch 与 stream 共用。
    async fn resolve_response(&mut self, turn_index: usize) {
        // ★ 回退锚点。必须在 `stage_output_blocks` 之前取 —— 此时 staging
        //   尚未写入本次调用的产出。
        let checkpoint = self.session.staging_checkpoint();

        let ResolvedResponse { result, origin } = match self.react_ctx.resolved.take() {
            Some(r) => r,
            None => {
                error!("No resolved response in ResolvingResponse state");
                self.set(LooperState::Failed);
                return;
            }
        };

        // ★ Hook: on_after_response
        if let HookAction::Abort(reason) =
            Self::invoke_on_after_response(&self.config.hooks, turn_index, &result).await
        {
            self.failure_reason = Some(TurnFailureReason::HookAbort(reason));
            self.set(LooperState::Failed);
            return;
        }

        // 聚合 usage。流式 usage 已在 chunk 阶段记账，这里只处理 batch。
        if origin == ResponseOrigin::Batch {
            self.session.add_usage(result.usage.clone());
            self.log_estimate_calibration(turn_index, result.usage.input_tokens);
            self.emit_event(LooperEvent::ModelUsage {
                call_index: self.react_loop_iteration,
                usage: result.usage.clone(),
            });
        }

        // 写入 assistant 内容到 session staging（分块回填 InputItem，
        // 同时更新 assistant_text / assistant_reasoning）。
        self.stage_output_blocks(&result.output);

        // 服务端工具块只通知前端展示，不进 pending_tool_calls。
        // 流式在 `BlockEnd` 分支即时发出，batch 在此发出。
        if origin == ResponseOrigin::Batch {
            for block in &result.output {
                match block {
                    ContentBlock::ServerToolUse {
                        call_id,
                        name,
                        input,
                    } => {
                        self.emit_event(LooperEvent::ServerToolUse {
                            id: call_id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        });
                    }
                    ContentBlock::ServerToolResult {
                        call_id,
                        name,
                        content,
                    } => {
                        self.emit_event(LooperEvent::ServerToolResult {
                            id: call_id.clone(),
                            name: name.clone(),
                            content: content.clone(),
                        });
                    }
                    _ => {}
                }
            }
        }

        // 状态收敛：Failed 与 Incomplete 都视为异常终止（partial_text 已回填）
        if result.status != ResponseStatus::Completed {
            // ★ 截断重试：`Incomplete` 且 `finish_reason = MaxTokens`。
            //   `finish_reason` 缺失时为 `None`，不重试。
            if result.status == ResponseStatus::Incomplete
                && matches!(result.finish_reason, Some(FinishReason::MaxTokens))
                && self
                    .begin_retry(
                        RetryCause::Truncated {
                            output_tokens: result.usage.output_tokens,
                        },
                        turn_index,
                        checkpoint,
                    )
                    .await
            {
                return;
            }

            // ★ 流被掐断（`Incomplete` 且无 `finish_reason`）：按瞬时故障
            //   退避重发。仅流式适用。
            if origin == ResponseOrigin::Stream
                && result.status == ResponseStatus::Incomplete
                && result.finish_reason.is_none()
                && self
                    .begin_retry(
                        RetryCause::Transient {
                            trigger: "stream_cut",
                        },
                        turn_index,
                        checkpoint,
                    )
                    .await
            {
                return;
            }

            let msg = result
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| match result.status {
                    ResponseStatus::Incomplete => {
                        "model response was truncated (incomplete)".to_string()
                    }
                    _ => "model response failed".to_string(),
                });
            // finish_reason 区分截断、内容过滤与上游异常；output_tokens 用来
            // 判断截断是否顶到了输出上限；block_kinds 说明收到了哪些块。
            error!(
                turn_index,
                origin = ?origin,
                status = ?result.status,
                finish_reason = result.finish_reason.map_or("-", |r| r.as_str()),
                message = %msg,
                blocks = result.output.len(),
                block_kinds = ?block_kinds_of(&result.output),
                text_len = self.react_ctx.assistant_text.len(),
                reasoning_len = self.react_ctx.assistant_reasoning.len(),
                input_tokens = result.usage.input_tokens,
                output_tokens = result.usage.output_tokens,
                "Model response ended with non-completed status"
            );
            // 内容过滤单独归类，且不可重试。
            self.failure_reason = Some(
                if matches!(result.finish_reason, Some(FinishReason::ContentFilter)) {
                    TurnFailureReason::ContentFiltered { message: msg }
                } else if origin == ResponseOrigin::Stream
                    && result.status == ResponseStatus::Incomplete
                    && result.finish_reason.is_none()
                {
                    // 流被掐断的终局归因
                    TurnFailureReason::ModelUnavailable {
                        attempts: self.total_attempts(),
                        message: msg,
                    }
                } else {
                    TurnFailureReason::Other(msg)
                },
            );
            self.set(LooperState::Failed);
            return;
        }

        // 提取 tool calls（转 ToolCall）用于决定下一步
        let tool_calls: Vec<ToolCall> = result
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
            self.set(LooperState::ExecutingTools);
        } else {
            // batch 路径不发送 TextDelta（整段文本不是增量），
            // 统一由 Done 状态中的 TurnComplete 事件提供本轮最终文本。
            self.set(LooperState::Done);
        }
    }

    // ── Streaming 分支：Streaming ─────────────────────────────────────────

    /// 消费一个 stream chunk，处理后在同一个状态内循环直到流结束。
    ///
    /// 每收到一个 chunk 就返回（让 `run()` 循环顶有机会排空控制消息）。
    async fn consume_stream_chunk(&mut self, turn_index: usize) {
        let stream = match &mut self.active_stream {
            Some(s) => s,
            None => {
                error!("No active stream in Streaming state");
                self.set(LooperState::Failed);
                return;
            }
        };

        // 超时 = 一个「空步」：不改状态、不发事件，只把控制权还给 `run()` 让它
        // 排空消息、检查取消。丢掉的 future 是 cancel-safe 的 ——
        // 解析缓冲与重连退避都在 `SseState` 这个 pinned struct 内，跨 poll 保留。
        let next = match tokio::time::timeout(STEP_POLL, stream.next_chunk()).await {
            Err(_elapsed) => return,
            Ok(next) => next,
        };

        match next {
            Some(Ok(chunk)) => {
                // 双轨：delta 原样转发前端，BlockEnd 交由 assembler 折叠。
                self.stream_assembler.push(chunk.clone());

                match chunk {
                    StreamChunk::BlockStart { .. } => {}

                    StreamChunk::TextDelta { delta, .. } => {
                        // ★ Hook: on_text_delta — 可中止流式响应
                        if let HookAction::Abort(reason) = Self::invoke_on_text_delta(
                            &self.config.hooks,
                            turn_index,
                            &delta,
                            &self.react_ctx.assistant_text,
                        )
                        .await
                        {
                            self.failure_reason = Some(TurnFailureReason::HookAbort(reason));
                            self.set(LooperState::Failed);
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

                    StreamChunk::BlockEnd { block, .. } => match block {
                        // 服务端工具块只通知前端展示，不进 pending_tool_calls
                        // （provider 侧已执行完毕，客户端无可执行之物）。
                        ContentBlock::ServerToolUse {
                            call_id,
                            name,
                            input,
                        } => {
                            self.emit_event(LooperEvent::ServerToolUse {
                                id: call_id,
                                name,
                                input,
                            });
                        }
                        ContentBlock::ServerToolResult {
                            call_id,
                            name,
                            content,
                        } => {
                            self.emit_event(LooperEvent::ServerToolResult {
                                id: call_id,
                                name,
                                content,
                            });
                        }
                        // 其余块已由 assembler 折叠；ToolCallStart 统一在
                        // execute_tools_step 的 spawn 阶段发出，避免流式路径下重复发送。
                        _ => {}
                    },

                    StreamChunk::Usage { usage } => {
                        self.emit_event(LooperEvent::ModelUsage {
                            call_index: self.react_loop_iteration,
                            usage: usage.clone(),
                        });
                        self.log_estimate_calibration(turn_index, usage.input_tokens);
                        self.session.add_usage(usage);
                    }

                    // Finish 已由 assembler 记录，这里只做收尾组包。
                    StreamChunk::Finish { .. } => self.complete_stream(),
                }
            }

            Some(Err(e)) => {
                error!(error = %e, "Stream error");
                self.active_stream = None;
                // 流式路径的主分类 choke point：HTTP 非 200（校验阶段 yield Err）、
                // 流内 error payload、established-stream 传输中断都从这里进来。
                // 瞬时类回退 staging 后退避重发（chunk 不碰 staging，锚点即当前长度；
                // react_ctx 里的增量随回退被丢弃）；永久类类型化失败。
                if self
                    .fail_or_retry(&e, turn_index, self.session.staging_checkpoint())
                    .await
                {
                    return;
                }
                self.set(LooperState::Failed);
            }

            None => {
                // Stream ended without Finish event — 收敛 assembler，按成功处理。
                // 这里没有 Finish 可依，收敛结果完全取决于块是否闭合，属于异常路径，
                // 必须留下痕迹：否则「流被上游掐断」与「正常结束」在日志里长得一样。
                debug!(
                    turn_index,
                    "Model stream ended without a Finish chunk; converging on assembler state"
                );
                self.complete_stream();
            }
        }
    }

    /// 流结束（收到 `Finish` 或流自然关闭）后，把收敛结果装入
    /// [`ResolvedResponse`] 并迁移到 [`LooperState::ResolvingResponse`]。
    fn complete_stream(&mut self) {
        self.active_stream = None;
        let (blocks, usage, status, finish_reason, error) =
            std::mem::take(&mut self.stream_assembler).finish_with_reason();

        self.react_ctx.resolved = Some(ResolvedResponse {
            result: GenerateResult {
                // 流式响应没有上游 id。
                id: String::new(),
                output: blocks,
                usage,
                status,
                finish_reason,
                error,
            },
            origin: ResponseOrigin::Stream,
        });
        self.set(LooperState::ResolvingResponse);
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
                // 服务端工具块不入 session 历史：回放可省略，省上下文
                ContentBlock::ServerToolUse { .. } | ContentBlock::ServerToolResult { .. } => {}
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
    /// 用 `JoinSet` 将执行拆分为多个 `react_step()` 调用。每次调用要么收集一个
    /// 已完成 tool 的结果，要么在短超时后返回，让 `run()` 循环顶有机会排空控制
    /// 消息。取消不在这里检查 —— 收尾在 `run()` 循环顶发生，在途任务由
    /// [`Self::abort_inflight_tools`] 回收；本函数看到的取消标志恒为假（标志只在
    /// drain 消费消息时置位，步内无 drain）。
    ///
    /// 支持断点续执行：`result: Some(...)` 的已完成项自动跳过。
    async fn execute_tools_step(&mut self, turn_index: usize) {
        // ── Spawn 阶段：active_tool_tasks 为 None ───────────────────────
        if self.active_tool_tasks.is_none() {
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
                self.finish_tool_execution().await;
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

                match Self::invoke_on_before_tool(&self.config.hooks, turn_index, &call).await {
                    ToolHookAction::Continue => {
                        to_spawn.push(idx);
                    }
                    ToolHookAction::Override(result) => {
                        let overridden =
                            ToolCallResult::new(call.clone(), Content::Text(result), false);
                        let artifacts = overridden.artifacts.clone();
                        // 事件里的 result 必须用**剥离后**的文本，否则前端会看到
                        // 裸 `@@PECO_ARTIFACT` 行（而模型历史里那行已不存在）。
                        let text = overridden.result.text_view().into_owned();
                        self.react_ctx.pending_tool_calls[idx].result = Some(overridden);
                        self.emit_event(LooperEvent::ToolResult {
                            id: call.id.clone(),
                            name: call.function.name.clone(),
                            result: text,
                            images: Vec::new(),
                            artifacts,
                        });
                    }
                    ToolHookAction::Reject(reason) => {
                        let rejected =
                            ToolCallResult::new(call.clone(), Content::Text(reason), true);
                        let artifacts = rejected.artifacts.clone();
                        let text = rejected.result.text_view().into_owned();
                        self.react_ctx.pending_tool_calls[idx].result = Some(rejected);
                        self.emit_event(LooperEvent::ToolResult {
                            id: call.id.clone(),
                            name: call.function.name.clone(),
                            result: text,
                            images: Vec::new(),
                            artifacts,
                        });
                    }
                    ToolHookAction::Abort(reason) => {
                        self.failure_reason = Some(TurnFailureReason::HookAbort(reason));
                        self.set(LooperState::Failed);
                        return;
                    }
                }
            }

            // 如果所有 tool 已被 hook 处理（Override/Reject），直接收尾
            if to_spawn.is_empty() {
                self.finish_tool_execution().await;
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
                        Ok(r) => ToolCallResult::new(call.clone(), r, false),
                        // 错误路径保持纯文本
                        Err(e) => ToolCallResult::new(call.clone(), Content::Text(e), true),
                    };
                    (idx, result)
                });
            }

            self.active_tool_tasks = Some(joinset);
            // 立即返回 — 下一次 react_step 调用将进入 poll 阶段
            return;
        }

        // ── Poll 阶段：active_tool_tasks 为 Some ────────────────────────
        // 以短超时轮询下一个完成的 task
        let poll_result = tokio::time::timeout(
            STEP_POLL,
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
                    artifacts: tool_result.artifacts.clone(),
                });

                Self::invoke_on_after_tool(
                    &self.config.hooks,
                    turn_index,
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
                                artifacts: tr.artifacts.clone(),
                            });

                            Self::invoke_on_after_tool(
                                &self.config.hooks,
                                turn_index,
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
                    self.finish_tool_execution().await;
                }
            }

            // Task panic
            Ok(Some(Err(join_error))) => {
                error!(
                    error = %join_error,
                    "Tool execution task panicked; continuing with remaining tools"
                );
            }

            // 超时 — 返回 run() 循环顶以排空控制消息、检查取消
            Err(_elapsed) => {}

            // 所有 task 已全部完成
            Ok(None) => {
                self.active_tool_tasks = None;
                self.finish_tool_execution().await;
            }
        }
    }

    /// 把 `pending_tool_calls` 中 `result: Some` 的项写入 staging。**只写不清理**。
    ///
    /// 写入与清理彻底分开：`finish_tool_execution` 写完还要清 looper 状态，
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
    async fn finish_tool_execution(&mut self) {
        self.stage_completed_tool_results();

        // ★ 在途轮检查点：一批工具刚落地的时刻 —— 副作用已经发生，
        //   此刻进程若在后续模型调用中崩溃，这批结果靠它进历史。
        //   写入失败只 warn：检查点是尽力而为，不该影响正常路径。
        self.persist_inflight_checkpoint().await;

        // 清理本轮 tool 数据，进入下一轮
        self.react_ctx.pending_tool_calls.clear();
        self.react_ctx.assistant_text.clear();
        self.react_ctx.assistant_reasoning.clear();
        self.react_ctx.resolved = None;
        self.set(LooperState::PreparingRequest);
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

    /// 统一的 turn 失败收尾：冻结在途轮 → 落盘 → 发事件。
    ///
    /// `force_persist` 与 `persist_on_failure` 取或：后者门控普通失败轮是否落盘
    /// （CLI 走 `NullSessionPersister` 时置 false 即无副作用），前者供取消路径
    /// **强制**落盘 —— 取消不丢 pending，而 pending 只活在 Session 内存里。
    ///
    /// 契约是「把状态搬到可推进的位置」，不回报布尔让调用方决定续接 —— 续接交给
    /// `run()` 的 ③ 单一入口。
    ///
    /// `epilogue` 是已合成好的收尾报告（仅 `max_iterations` 路径可能为 `Some`），
    /// 透传给 `plan_failure` 后在冻结时取代固定中断说明；`None` 时行为不变。
    async fn apply_failure(
        &mut self,
        reason: TurnFailureReason,
        force_persist: bool,
        epilogue: Option<String>,
    ) {
        // `retry_happened` 决定 `plan_failure` 是否补存活桩：只有「回退已发生、
        // 新尝试零产出」才需要桩 —— 那正是回退把 staging 清空留下的洞。
        match plan_failure(
            &mut self.session,
            &mut self.react_ctx,
            self.config.persist_on_failure || force_persist,
            reason,
            self.retries_used > 0,
            epilogue.as_deref(),
        ) {
            Some(plan) => self.commit_failure_plan(plan).await,
            None => {
                // 无在途轮（plan_failure 守卫：session Idle + staging 空）——仍要把状态
                // 带回停靠位，否则 Failed 留在循环顶反复命中（防御未知路径产生
                // 「收尾后 state 仍为 Failed」的热自旋）。
                if self.state.is_active() {
                    self.set(LooperState::Idle);
                }
            }
        }
    }

    /// 消费 [`FinalizePlan`] 的异步段：事件 → hook → 落盘 → 回写原因。冻结 + 快照
    /// 已在 [`plan_failure`] 内同步完成，此处只做 I/O 与编排。
    async fn commit_failure_plan(&mut self, plan: FinalizePlan) {
        // `iteration` / `max_iterations` 取自此处的 `self` 而非 `plan`：两者是「本用户轮的
        // ReAct 深度」与「该轮预算」，`plan` 只承载 Session 侧的产物。计数在
        // `reset_iteration_counters`（下一轮启动）才清零，收尾时仍是本轮的终值 ——
        // `MaxIterationsExceeded` 下二者必然相等，其余失败原因则回答「预算用掉多少才挂的」。
        warn!(
            turn_index = plan.turn_index,
            reason = ?plan.reason,
            iteration = self.react_loop_iteration,
            max_iterations = self.max_iterations,
            frozen_staging_messages = plan.frozen_staging_messages,
            partial_text_len = plan.partial_text_len,
            "Turn interrupted; staging frozen into committed history"
        );

        // ★ 收尾报告先于 TurnComplete 下发。它不是流式产物（`generate_full` 一次
        //   成形），若不在这里补发，用户不刷新页面就看不到 —— 实时视图只认
        //   `text_delta`，而它会追加到最后一条 assistant 气泡。必达发送：
        //   丢一条就等于这轮白白多花了一次模型调用。
        if let Some(text) = &plan.epilogue {
            Self::emit_event_guaranteed(
                &self.event_speaker,
                LooperEvent::TextDelta {
                    delta: text.clone(),
                },
            )
            .await;
        }

        // 事件照发 —— 冻结失败只跳过落盘，不跳过 TurnComplete。
        // 「这轮失败了」这个消息不能让用户看不到。
        let usage = self.session.total_usage();
        Self::emit_event_guaranteed(
            &self.event_speaker,
            LooperEvent::TurnComplete {
                turn_index: plan.turn_index,
                outcome: plan.outcome.clone(),
                usage: usage.clone(),
            },
        )
        .await;
        Self::invoke_on_turn_complete(
            &self.config.hooks,
            plan.turn_index,
            plan.outcome.failure_reason(),
            &usage,
            &self.session,
        )
        .await;

        // 落盘门控：冻结无条件发生（内存），持久化由 persist_on_failure 决定
        if let Some(snapshot) = plan.snapshot
            && let Err(e) = self.save_snapshot(&snapshot).await
        {
            error!(error = %e, "Failed to persist session after turn failure");
        }
        // ★ 冻结已落进 committed，在途检查点作废（与落盘门控无关：
        //   门控关掉的是快照，检查点留着会在下次冷启动被判为陈旧）。
        self.clear_inflight_checkpoint().await;

        // ★ 回写失败原因：`take()` 之后 run() 末尾的 shutdown_reason 会退化成
        //   "done"，Shutdown 事件丢失失败原因。这次回写是刻意的，不是冗余赋值。
        self.failure_reason = Some(plan.reason.clone());

        // interrupt_turn / rollback_turn 已把 session 置回 Idle。
        // 任意 → Idle 无条件归位（含暂停态，恢复位置随迁移丢弃）；
        // 事件随 `set` 发出，hook 无条件调用（from==to 也调）。
        let prev = self.set(LooperState::Idle);
        Self::invoke_on_state_change(&self.config.hooks, prev, LooperState::Idle).await;
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
    turn_index: usize,
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
    /// 已合成好的收尾报告（仅 `max_iterations` 路径可能为 `Some`）。
    ///
    /// 冻结时已写进本轮历史，此字段只剩一个用途：让 `commit_failure_plan`
    /// 把它作为 `TextDelta` 补发给客户端。
    epilogue: Option<String>,
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
        TurnFailureReason::MaxIterationsExceeded => "max iterations exceeded".to_string(),
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

/// 把已分类的 [`model_provider::ClassifiedError`] 映射为类型化失败原因。
///
/// 分类在调用方（[`AgentLooper::fail_or_retry`]）只做一次，本函数按值取走。
/// `attempts` = 实际发起的尝试总数（含触发失败的那次），仅瞬时类变体使用。
/// `ClassifiedError.message` 原样带进变体 — 分类不吞原文，SSE error 文案
/// 与 session 中断标签都靠它保留 provider 诊断信息。
///
/// `AgentError` 的非 Provider 变体（Io/Config/MaxIterations…）不走本函数，
/// 由各自调用点直接构造原因。
fn model_failure_reason(
    classified: model_provider::ClassifiedError,
    attempts: u32,
) -> TurnFailureReason {
    use model_provider::ApiErrorKind;

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

/// 会话是否还有在途轮可收尾 —— 非 `Idle`，或 staging 里还留着本轮消息。
///
/// [`AgentLooper::has_inflight_turn`]（取消收尾的补落盘判据）与 [`plan_failure`] ①
/// 共用同一判据：两处漂移会让「取消补落盘」与「失败冻结」对相同状态得出不同结论。
fn session_has_inflight(session: &Session) -> bool {
    !matches!(session.state(), SessionState::Idle) || !session.staging_messages().is_empty()
}

/// 无 await，全部 session 变更在此完成。
///
/// **不变量：本函数全程禁止 `.await`。** 正因为是纯同步，「冻结」对取消免疫 ——
/// 若 `run()` 被 abort，最多丢事件与落盘，绝不会出现「落了盘但 committed 里
/// 没有这一轮」。严防后人往这段塞 `.await`。
///
/// 步骤顺序：① 守卫 → ② 取标量 → ③ 补轮次存活桩（新尝试的部分文本，
/// 或重发后的固定桩）→ ④ 冲刷已完成的工具结果 → ⑤ 冻结 → ⑥ 快照 →
/// ⑦ 清状态。③ 在 ⑤ 之前是硬要求：它决定 `interrupt_turn` 是冻结还是
/// 退化成 `rollback_turn`。④ 在 ⑤ 之前同样是硬要求：捞回来的工具结果
/// 必须和这一轮一起进 committed，且 `stage_item` 只在 `Active` 下可用
/// （⑤ 之后就 `Idle` 了）。
///
/// 返回 `None` 表示无在途轮可收尾 —— 挡的是「取消发生在 `Idle`」（根本没有在途轮，
/// 例如 looper 正空转等输入时用户点了停止）时发出语义为空的 `TurnComplete`。
fn plan_failure(
    session: &mut Session,
    ctx: &mut ReActContext,
    persist: bool,
    reason: TurnFailureReason,
    retry_happened: bool,
    epilogue: Option<&str>,
) -> Option<FinalizePlan> {
    // ① 无在途轮守卫
    if !session_has_inflight(session) {
        return None;
    }

    // ② 取标量 —— 必须在所有变更之前
    let turn_index = session.turn_index(); // interrupt_turn 后会 +1
    let partial_text = std::mem::take(&mut ctx.assistant_text);
    let partial_text_len = partial_text.len();

    // ③ 补回轮次存活桩。staging 只剩 `user_input` 时 `interrupt_turn` 会退化成
    //    `rollback_turn(false)`，这一轮（含用户提问）整轮丢失。
    //    补一条 assistant 消息让冻结路径照常成立：吐过文本用 partial_text，零产出
    //    补固定文案桩（「重试后无产出」/「取消」两种来源各补各的；取消后 looper 留在
    //    Idle，不补桩用户重载页面会发现自己那条消息凭空消失）。被回退的截断文本
    //    **不进历史** —— 实时视图的残句由 TruncationRetry 解释，重载后随快照消失。
    if session.staging_messages().is_empty() {
        if !partial_text.is_empty() {
            session.stage_salvage(partial_text.clone());
        } else if retry_happened {
            session.stage_salvage("[response discarded after retry]".into());
        } else if matches!(reason, TurnFailureReason::Cancelled) {
            session.stage_salvage("[cancelled before response]".into());
        }
    }

    let frozen_staging_messages = session.staging_messages().len(); // ④ 冲刷后会变

    // ④ 冲刷 poll 阶段已捞回的工具结果（只写不清理；见 `stage_tool_results`）。
    //    必须在 ⑤ 之前 —— 这些结果是「这一轮已完成的成果」，要和本轮一起冻结。
    stage_tool_results(session, &ctx.pending_tool_calls);

    // ⑤ 冻结在途轮（内部补齐悬空 tool_call，再追加中断说明或收尾报告）
    //    `epilogue` 有值时**取代**中断说明：两者并存会让历史以两条连续 assistant
    //    消息收尾。顺序由 `interrupt_turn_with_closing` 保证（补齐恒在前）。
    let label = failure_label(&reason);
    let token = match session.interrupt_turn_with_closing(&label, epilogue) {
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

    // ⑦ 清 ctx —— pending_tool_calls 必须清，否则下一轮续接的 execute_tools_step
    //    会把上一轮取消掉的 tool 重跑一遍（to_spawn 只 filter result.is_none()），
    //    即重放副作用。
    ctx.pending_tool_calls.clear();
    ctx.assistant_text.clear();
    ctx.assistant_reasoning.clear();
    ctx.resolved = None;

    Some(FinalizePlan {
        turn_index,
        outcome: TurnOutcome::Failed {
            reason: reason.clone(),
            partial_text,
        },
        reason,
        partial_text_len,
        snapshot,
        frozen_staging_messages,
        epilogue: epilogue.map(str::to_string),
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "agent_looper_tests.rs"]
mod tests;
