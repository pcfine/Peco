// ============================================================================
// Session 核心类型定义
// ============================================================================
//
// NOTE: Some types are used by peco-server or planned for future use.

#![allow(dead_code)]
//
// 本文件定义了 Session 模块的新核心类型：
// - MessageId: 每条消息的唯一标识符（单调递增）
// - MessageSource: 消息来源标记（用于审计和调试）
// - AnnotatedMessage: 带元数据的消息（分层消息模型的基础）
// - SessionState: 会话运行状态机
// - PendingInput: 排队中的用户输入
// - SessionTimestamps: 会话时间戳集合

use std::sync::Arc;

use model_provider::{Content, ContentPart, InputItem, Role};
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
    /// 用户直接输入（对应 `InputItem::Message { role: User }`）
    UserInput,
    /// 模型生成的回复（对应 `InputItem::Message { role: Assistant }` / `FunctionCall` / `Reasoning`）
    ModelGeneration,
    /// Tool 执行结果（对应 `InputItem::FunctionCallOutput`）
    ToolExecution {
        /// 被执行的工具名称
        tool_name: String,
    },
    /// 系统注入（如 skill 上下文、错误恢复提示、动态 prompt 等）
    SystemInjection {
        /// 注入原因
        reason: String,
    },
    /// 中断轮标记（该轮因中断被保留，非正常完成）。
    ///
    /// 由 [`Session::interrupt_turn`](super::Session::interrupt_turn) 挂在追加的
    /// 中断说明与合成的工具输出上。消费方据此识别「这一轮没跑完就入库了」，
    /// 不必去猜文案特征串。
    InterruptedTurn {
        /// 中断原因（人类可读，来自 `TurnFailureReason` 的文案化形式）
        reason: String,
    },
    /// pending 队列批量合并出的用户消息。
    ///
    /// 由 [`merge_contents`] 拼接（含 `---` 分隔标记，模型侧原样保留）；
    /// 展示层据此调用 [`strip_merge_markers`] 剥离标记后再渲染 ——
    /// 与 `InterruptedTurn` 同理：按来源识别，不猜文案特征串。
    /// 旧快照无此值，反序列化天然兼容。
    MergedPending,
    /// 撞上 `max_turns` 上限时合成的收尾报告。
    ///
    /// 由 [`Session::interrupt_turn_with_closing`](super::Session::interrupt_turn_with_closing)
    /// 追加在悬空工具调用补齐**之后**，**取代**通常的 `InterruptedTurn`
    /// 中断说明 —— 两者同时追加会让历史以两条连续 assistant 消息收尾。
    /// 与 `InterruptedTurn` 同理：按来源识别，不猜文案特征串。
    /// 旧快照无此值，反序列化天然兼容。
    TurnEpilogue,
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

    /// 消息内容（中立 InputItem，Arc 共享所有权，避免上下文构建时深度克隆）
    pub message: Arc<InputItem>,

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
    pub fn new(
        id: MessageId,
        turn_index: usize,
        message: InputItem,
        source: MessageSource,
    ) -> Self {
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
    /// - `InputItem::Message { role: User }`：总是展示
    /// - `InputItem::Message { role: Assistant }`：仅当有文本内容（即最终回复）时展示
    ///   （tool 调用已拆为独立的 `FunctionCall` item，不再附于 assistant 消息）
    /// - `FunctionCall` / `FunctionCallOutput` / `Reasoning` / `Message{System}`：不展示
    pub fn is_displayable(&self) -> bool {
        match self.message.as_ref() {
            InputItem::Message {
                role: Role::User, ..
            } => true,
            InputItem::Message {
                role: Role::Assistant,
                content,
            } => !content.text_view().is_empty(),
            _ => false,
        }
    }

    /// 是否为模型最终回复（turn 终止点的 Assistant 文本消息）。
    pub fn is_final_response(&self) -> bool {
        matches!(
            self.message.as_ref(),
            InputItem::Message {
                role: Role::Assistant,
                content
            } if !content.text_view().is_empty()
        )
    }

    /// 是否为工具调用（`InputItem::FunctionCall`）。
    pub fn is_tool_invocation(&self) -> bool {
        matches!(self.message.as_ref(), InputItem::FunctionCall { .. })
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

/// 排队中的用户输入。
///
/// 当 session 处于 Active 状态时收到的用户消息不直接写入 staging，
/// 而是放入 pending 队列。当前 turn 完成后整个队列被排空、合并为一条
/// user 消息启动新 turn（见 [`merge_contents`]）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingInput {
    /// 用户输入内容（纯文本或文本 + 图片部件混排）。
    ///
    /// `alias = "text"`：旧快照以 String 承载该字段，反序列化时按旧字段名读取。
    #[serde(alias = "text")]
    pub content: Content,
    /// 到达时间（Unix 毫秒）
    pub arrived_at_ms: u64,
}

impl PendingInput {
    /// 创建新的排队输入。
    pub fn new(content: Content) -> Self {
        Self {
            content,
            arrived_at_ms: unix_timestamp_ms(),
        }
    }
}

/// 把多条排队输入合并为一条 Content，作为单条 user 消息交给大模型自行判断。
///
/// - 单条原样返回（纯文本 / 带图均与合并前一致）。
/// - 多条时每条前置一行 `---` 标记再拼接，让模型能分辨独立输入的边界。
///   **纯文本分支与含图分支产出的 wire 文本一致**（`---\nA\n---\nB`：
///   chat 序列化将相邻文本部件直传拼接，标记部件自带换行、条目间补 `\n`）；
///   含图形态经 `text_view()` 会多出空行，展示层由 [`strip_merge_markers`] 归一。
/// - 图片部件不丢：任一条含图片时整体转 `Parts`，文本与图片部件按原顺序
///   穿插，分隔标记以 `Text` 部件插入。
pub fn merge_contents(items: Vec<Content>) -> Content {
    debug_assert!(!items.is_empty(), "merge_contents called with empty input");
    if items.len() == 1 {
        return items.into_iter().next().unwrap();
    }

    let has_image = items.iter().any(|c| c.image_count() > 0);
    if !has_image {
        // 全纯文本：字符串拼接，每条前置 --- 标记行
        let mut out = String::new();
        for item in items {
            out.push_str("---\n");
            out.push_str(&item.text_view());
            out.push('\n');
        }
        out.pop(); // 去掉末尾换行
        return Content::Text(out);
    }

    // 含图片：部件形态拼接，图片原顺序保留。
    // 标记部件自带 "\n"、条目间补 "\n" 部件 —— wire 侧相邻文本部件
    // 直传拼接后与纯文本分支逐字一致（见 doc）。
    let mut parts: Vec<ContentPart> = Vec::new();
    let last = items.len() - 1;
    for (i, item) in items.into_iter().enumerate() {
        parts.push(ContentPart::Text {
            text: "---\n".to_string(),
        });
        match item {
            Content::Text(t) => parts.push(ContentPart::Text { text: t }),
            Content::Parts(mut ps) => parts.append(&mut ps),
        }
        if i < last {
            parts.push(ContentPart::Text {
                text: "\n".to_string(),
            });
        }
    }
    Content::Parts(parts)
}

/// 剥离 [`merge_contents`] 的 `---` 分隔标记 —— 仅用于
/// [`MessageSource::MergedPending`] 的内容（其余来源不得调用，否则会
/// 误删用户自己输入的 `---` 行）。
///
/// 按标记行切段、去掉每段首尾的接缝空行（`text_view()` 在标记部件附近
/// 产生），段内空行保留（消息自身的段落），段间以单个换行拼接。
/// 纯文本合并形态 `---\nA\n---\nB` → `A\nB`；含图 `text_view()` 形态
/// （标记间多空行）同样归一为 `A\nB`。
pub fn strip_merge_markers(content: &Content) -> String {
    let text = content.text_view();
    let mut segments: Vec<Vec<&str>> = vec![Vec::new()];
    for line in text.lines() {
        if line.trim() == "---" {
            segments.push(Vec::new());
            continue;
        }
        segments
            .last_mut()
            .expect("segments starts non-empty")
            .push(line);
    }

    let trimmed: Vec<String> = segments
        .iter()
        .filter_map(|seg| {
            let mut s = seg.as_slice();
            while s.first().is_some_and(|l| l.trim().is_empty()) {
                s = &s[1..];
            }
            while s.last().is_some_and(|l| l.trim().is_empty()) {
                s = &s[..s.len() - 1];
            }
            if s.is_empty() {
                None
            } else {
                Some(s.join("\n"))
            }
        })
        .collect();
    trimmed.join("\n")
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
