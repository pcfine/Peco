// ============================================================================
// Session 核心类型定义
// ============================================================================
//
// 本文件定义了 Session 模块的新核心类型：
// - MessageId: 每条消息的唯一标识符（单调递增）
// - MessageSource: 消息来源标记（用于审计和调试）
// - AnnotatedMessage: 带元数据的消息（分层消息模型的基础）
// - SessionState: 会话运行状态机
// - PendingInput: 排队中的用户输入
// - SessionTimestamps: 会话时间戳集合

use std::sync::Arc;

use model_provider::Message;
use serde::{Deserialize, Serialize};

// ============================================================================
// MessageId
// ============================================================================

/// 消息唯一标识符，per-session 单调递增。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MessageId(pub u64);

impl MessageId {
    /// 创建新的 MessageId。
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for MessageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "msg_{}", self.0)
    }
}

// ============================================================================
// MessageSource
// ============================================================================

/// 消息来源标记，描述消息的产生方式。
///
/// 与 [`Message`] enum 不同，`MessageSource` 关注的是 **谁/什么** 产生了这条消息，
/// 而非消息的协议格式（role）。用于审计日志、调试追踪和持久化元数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageSource {
    /// 用户直接输入（对应 `Message::User`）
    UserInput,
    /// 模型生成的回复（对应 `Message::Assistant`）
    ModelGeneration,
    /// Tool 执行结果（对应 `Message::Tool`）
    ToolExecution {
        /// 被执行的工具名称
        tool_name: String,
    },
    /// 系统注入（如 skill 上下文、错误恢复提示、动态 prompt 等）
    SystemInjection {
        /// 注入原因
        reason: String,
    },
}

// ============================================================================
// AnnotatedMessage
// ============================================================================

/// 带元数据的消息条目。
///
/// 每个存储在 Session 中的消息都被包装为 `AnnotatedMessage`，
/// 携带足够的上下文信息以支持 rollback、调试、多视图过滤和审计。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotatedMessage {
    /// 唯一消息 ID（per-session 单调递增）
    pub id: MessageId,

    /// 所属 turn 编号（从 0 开始）
    pub turn_index: usize,

    /// 消息内容（LLM 协议格式，Arc 共享所有权，避免上下文构建时深度克隆）
    pub message: Arc<Message>,

    /// 消息写入时间（Unix 毫秒）
    pub timestamp_ms: u64,

    /// 该消息的 token 估计值
    ///
    /// - 对于 User 消息：输入 token 估计
    /// - 对于 Assistant 消息：输出 token 估计
    /// - 对于 Tool 消息：通常为 `None`
    pub estimated_tokens: Option<u32>,

    /// 消息来源
    pub source: MessageSource,
}

impl AnnotatedMessage {
    /// 创建新的带注释消息。
    pub fn new(id: MessageId, turn_index: usize, message: Message, source: MessageSource) -> Self {
        Self {
            id,
            turn_index,
            message: Arc::new(message),
            timestamp_ms: unix_timestamp_ms(),
            estimated_tokens: None,
            source,
        }
    }

    /// 判断此消息是否应在对话 UI 中展示。
    ///
    /// 展示规则：
    /// - `Message::User`：总是展示
    /// - `Message::Assistant`：仅当有文本内容且无 tool_calls（即最终回复）时展示
    /// - `Message::Tool` / `Message::System`：不展示
    ///
    /// # 示例
    ///
    /// ```ignore
    /// let displayable: Vec<_> = session.display_message_refs().collect();
    /// // 只包含用户提问和模型最终回复，不含中间 tool 调用过程
    /// ```
    pub fn is_displayable(&self) -> bool {
        match self.message.as_ref() {
            Message::User { .. } => true,
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                content.as_ref().is_some_and(|c| !c.is_empty())
                    && !tool_calls.as_ref().is_some_and(|t| !t.is_empty())
            }
            _ => false,
        }
    }

    /// 是否为模型最终回复（turn 终止点的 Assistant 消息）。
    pub fn is_final_response(&self) -> bool {
        matches!(self.message.as_ref(),
            Message::Assistant { content, tool_calls, .. }
            if content.as_ref().is_some_and(|c| !c.is_empty())
                && !tool_calls.as_ref().is_some_and(|t| !t.is_empty())
        )
    }

    /// 是否为带 tool_calls 的中间推理步骤。
    pub fn is_tool_invocation(&self) -> bool {
        matches!(self.message.as_ref(),
            Message::Assistant { tool_calls, .. }
            if tool_calls.as_ref().is_some_and(|t| !t.is_empty())
        )
    }
}

// ============================================================================
// SessionState
// ============================================================================

/// 会话运行状态。
///
/// 由 Session 内部管理，AgentLooper 通过 Session API 读写。
///
/// 状态转换图：
/// ```text
/// Idle ──[start_turn]──→ Active ──[commit_turn]──→ Idle
///   ▲                      │
///   │                      ├──[cancel]──→ Cancelling ──[rollback]──→ Idle
///   │                      │
///   │                      └──[外部中断]──→ Interrupted ──[resume]──→ Active
///   │
///   └──[shutdown]── (Session 销毁)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    /// 空闲，等待用户输入
    Idle,
    /// 正在执行 ReAct 循环
    Active,
    /// 已被用户取消，正在清理 staging
    Cancelling,
    /// 已被外部中断，等待恢复
    Interrupted,
}

impl SessionState {
    /// 是否允许开启新的 turn。
    pub fn can_start_turn(&self) -> bool {
        matches!(self, Self::Idle)
    }

    /// 是否允许向 staging 追加消息。
    pub fn can_stage_message(&self) -> bool {
        matches!(self, Self::Active)
    }
}

// ============================================================================
// PendingInput
// ============================================================================

/// 输入优先级。
///
/// 当 pending 队列中存在多个输入时，高优先级输入优先处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum InputPriority {
    /// 普通用户输入（默认）
    Normal = 0,
    /// 高优先级中断（如用户说 "stop"、"cancel"），优先于 Normal 处理
    Interrupt = 1,
}

/// 排队中的用户输入。
///
/// 当 session 处于 Active 状态时收到的用户消息不直接写入 staging，
/// 而是放入 pending 队列。当前 turn 完成后自动取出并开始新 turn。
///
/// 高优先级输入（`InputPriority::Interrupt`）优先于普通输入处理。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingInput {
    /// 用户输入文本
    pub text: String,
    /// 到达时间（Unix 毫秒）
    pub arrived_at_ms: u64,
    /// 优先级（默认 Normal）
    pub priority: InputPriority,
}

impl PendingInput {
    /// 创建新的排队输入（默认 Normal 优先级）。
    pub fn new(text: String) -> Self {
        Self {
            text,
            arrived_at_ms: unix_timestamp_ms(),
            priority: InputPriority::Normal,
        }
    }

    /// 创建指定优先级的排队输入。
    pub fn with_priority(text: String, priority: InputPriority) -> Self {
        Self {
            text,
            arrived_at_ms: unix_timestamp_ms(),
            priority,
        }
    }
}

// ============================================================================
// SessionTimestamps
// ============================================================================

/// 会话时间戳集合（内部使用）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct SessionTimestamps {
    /// 会话创建时间（Unix 秒）
    pub created_at: u64,
    /// 最后一次变更时间（Unix 秒）
    pub updated_at: u64,
    /// 最后一次活跃时间（Unix 秒）
    pub last_active_at: u64,
}

impl SessionTimestamps {
    /// 创建新的时间戳集合，所有时间戳设为当前时间。
    pub fn now() -> Self {
        let now = unix_timestamp_secs();
        Self {
            created_at: now,
            updated_at: now,
            last_active_at: now,
        }
    }

    /// 更新 updated_at 和 last_active_at 为当前时间。
    pub fn touch(&mut self) {
        let now = unix_timestamp_secs();
        self.updated_at = now;
        self.last_active_at = now;
    }
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 获取当前 Unix 时间戳（秒）。
pub(crate) fn unix_timestamp_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 获取当前 Unix 时间戳（毫秒）。
pub(crate) fn unix_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
