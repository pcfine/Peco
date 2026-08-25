// ============================================================================
// 中立内容块词汇表 — provider 层与引擎层之间唯一的契约
// ============================================================================
//
// 本模块定义一套中立的、自有的 LLM 对话/流式词汇表，不绑定任何厂商协议。
// 换协议 = 加/改一个适配器（adapter），引擎层（peco-core）无感。
//

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::types::{ToolDefinition, Usage};

// ============================================================================
// ContentBlock — 内容块（顺序敏感）
// ============================================================================

/// 中立内容块。`Text` / `Reasoning` / `ToolCall` 各自独立成块，顺序即语义。
///
/// 采用 serde 外部标记（默认），JSON 自描述、可扩展。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ContentBlock {
    /// 可见文本。
    Text { text: String },
    /// 推理/思考内容（与可见文本分离）。
    Reasoning { text: String },
    /// 工具调用；`arguments` 为 raw JSON 字符串（与 wire 协议及工具执行边界一致）。
    ToolCall {
        call_id: String,
        name: String,
        arguments: String,
    },
}

// ============================================================================
// InputItem — 输入项（历史回放）
// ============================================================================

/// 会话历史是有序 `InputItem` 列表，比旧 `Message` 在 item 层更细粒度。
///
/// 与 [`ContentBlock`] 分开的原因：模型**输出**只有它自己能产出的内容
/// （`Text` / `Reasoning` / `ToolCall`，无角色、无工具结果）；**输入**（喂回模型
/// 的历史）额外需要 `FunctionCallOutput`（工具结果）与带 `role` 的 `Message`。
/// 两者形状不对称，合并成一个大枚举会丢掉类型级保证。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum InputItem {
    /// 单条文本消息（含角色）。
    Message { role: Role, content: String },
    /// 工具调用（回放历史时与相邻 assistant 消息合并）；`arguments` 为 raw JSON 字符串。
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    /// 工具结果。
    FunctionCallOutput { call_id: String, output: String },
    /// 推理（回传可选，多数情况下回放时丢弃）。
    Reasoning { content: String },
}

/// 消息角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    /// Responses 语义；DeepSeek 视同 system。
    Developer,
    User,
    Assistant,
}

// ============================================================================
// StreamChunk — 流式协议
// ============================================================================

/// 内容块类型（封闭协议层）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    Text,
    Reasoning,
    ToolCall,
}

/// 结束原因（`#[non_exhaustive]` 可扩展数据模型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FinishReason {
    Stop,
    ToolCalls,
    MaxTokens,
    Aborted,
    Error,
}

impl FinishReason {
    /// 返回稳定的字符串表示，用于日志与错误消息。
    pub fn as_str(&self) -> &'static str {
        match self {
            FinishReason::Stop => "stop",
            FinishReason::ToolCalls => "tool_calls",
            FinishReason::MaxTokens => "max_tokens",
            FinishReason::Aborted => "aborted",
            FinishReason::Error => "error",
        }
    }
}

/// 流式片段。封闭判别联合，switch 穷尽。
///
/// 核心约定：delta 靠 `index` 关联到块，`BlockEnd` 携带完整组装好的
/// [`ContentBlock`]，消费者不做 delta 重装。
#[derive(Debug, Clone)]
pub enum StreamChunk {
    /// 一个内容块开始（index + 块类型）。
    BlockStart { index: usize, block_type: BlockType },
    /// 文本增量。
    TextDelta { index: usize, delta: String },
    /// 推理增量。
    ReasoningDelta { index: usize, delta: String },
    /// 工具调用增量；`name` 仅首块出现。
    /// `arguments` 为「片段 / 完整对象」临时联合：未闭合为 `Value::String`，
    /// 完整为 `Value::Object`；组装完成后由适配器在 `BlockEnd` 落回 `String`。
    ToolCallDelta {
        index: usize,
        call_id: String,
        name: Option<String>,
        arguments: serde_json::Value,
    },
    /// 内容块结束，携带完整组装的 [`ContentBlock`]。
    BlockEnd { index: usize, block: ContentBlock },
    /// 累积用量。
    Usage { usage: Usage },
    /// 结束原因。
    Finish { reason: FinishReason },
}

// ============================================================================
// 请求 / 响应
// ============================================================================

/// 中立生成请求：系统提示独立于历史（对齐 Responses `instructions` / chat 的 system 消息）。
#[derive(Debug, Clone)]
pub struct GenerateRequest {
    pub model: String,
    /// 系统指令；独立于 `input` 历史。
    pub instructions: Option<String>,
    /// 历史以 `Arc` 共享，零拷贝：外层 `Arc<[..]>` 共享整段 slice，
    /// 内层 `Arc<InputItem>` 与 session 的 `AnnotatedMessage.message` 共享同一项，
    /// 从 `Vec<Arc<InputItem>>` 转来时不发生任何 `InputItem` 深拷贝。
    pub input: Arc<[Arc<InputItem>]>,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: Option<ToolChoice>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens: Option<u32>,
    pub reasoning: Option<ReasoningConfig>,
    /// 预留结构化输出。
    pub text: Option<TextConfig>,
    pub additional_params: Option<serde_json::Value>,
}

/// 中立生成响应：有序内容块 + 状态 + 用量。
#[derive(Debug, Clone, PartialEq)]
pub struct GenerateResult {
    pub id: String,
    pub output: Vec<ContentBlock>,
    pub usage: Usage,
    pub status: ResponseStatus,
    pub error: Option<ResponseError>,
}

/// 响应状态（封闭协议层）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseStatus {
    Completed,
    Incomplete,
    Failed,
}

/// 推理开关与力度（中立化：关闭态由 `enabled` 显式表达，不靠 vendor 字符串）。
#[derive(Debug, Clone, PartialEq)]
pub struct ReasoningConfig {
    /// `false` = 关闭推理（适配器映射到 chat `thinking.type:"disabled"` / responses 关闭表达）。
    pub enabled: bool,
    /// 力度枚举；`None` = 使用 provider 默认。
    pub effort: Option<ReasoningEffort>,
}

/// 推理力度（`#[non_exhaustive]` 可扩展数据模型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    Max,
}

/// 响应错误（流式/非流式共用，`GenerateResult.error` 与 [`BlockAssembler`] 产物同源）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseError {
    pub code: Option<String>,
    pub message: String,
}

/// 预留结构化输出配置。
#[derive(Debug, Clone, PartialEq)]
pub struct TextConfig {
    pub format: Option<TextFormat>,
}

/// 文本输出格式。
#[derive(Debug, Clone, PartialEq)]
pub enum TextFormat {
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        schema: serde_json::Value,
    },
}

/// 工具选择策略。
#[derive(Debug, Clone, PartialEq)]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Named { name: String },
}

// ============================================================================
// BlockAssembler — 唯一折叠器
// ============================================================================

/// 引擎侧唯一的流式折叠实现。
///
/// 适配器必须保证 `BlockEnd` 携带完整块 + 正确的 `index`；本结构是**纯收集器 +
/// 按 index 落位**，不再维护 name/arguments 的 `HashMap` 累积。
#[derive(Debug)]
pub struct BlockAssembler {
    /// 按 `index` 落位的完整块；乱序 `BlockEnd` 以 index 归位（非到达顺序）。
    blocks: BTreeMap<usize, ContentBlock>,
    /// 已 `BlockStart` 未 `BlockEnd` 的块（index → 类型），用于 incomplete 检测。
    open: BTreeMap<usize, BlockType>,
    usage: Usage,
    status: ResponseStatus,
    error: Option<ResponseError>,
}

impl Default for BlockAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockAssembler {
    /// 创建空折叠器，初始状态为 `Completed`。
    pub fn new() -> Self {
        Self {
            blocks: BTreeMap::new(),
            open: BTreeMap::new(),
            usage: Usage::default(),
            status: ResponseStatus::Completed,
            error: None,
        }
    }

    /// 消费一个 [`StreamChunk`]。
    ///
    /// - `BlockEnd` 按 index 落位；
    /// - delta 直接丢弃（双轨：looper 已原样转发前端）；
    /// - `Finish{reason}` 映射到 status/error，并触发 `MaxTokens` 的未闭合 ToolCall 丢弃。
    pub fn push(&mut self, chunk: StreamChunk) {
        match chunk {
            StreamChunk::BlockStart { index, block_type } => {
                self.open.insert(index, block_type);
            }
            // 双轨语义：delta 由 looper 转发前端做实时渲染，此处不参与最终块组装。
            StreamChunk::TextDelta { .. }
            | StreamChunk::ReasoningDelta { .. }
            | StreamChunk::ToolCallDelta { .. } => {}
            StreamChunk::BlockEnd { index, block } => {
                self.blocks.insert(index, block);
                self.open.remove(&index);
            }
            StreamChunk::Usage { usage } => {
                self.usage = usage;
            }
            StreamChunk::Finish { reason } => match reason {
                // `ToolCalls` 与 `Stop` 同归 `Completed` — 引擎判定是否继续 ReAct
                // 只看 `output` 是否含 `ToolCall` 块，`ToolCalls` 变体仅为对齐 wire 语义。
                FinishReason::Stop | FinishReason::ToolCalls => {}
                // 未闭合的 ToolCall 不会产出块（隐式丢弃）；已 `BlockEnd` 的完整块保留。
                FinishReason::MaxTokens => {
                    self.status = ResponseStatus::Incomplete;
                }
                FinishReason::Aborted | FinishReason::Error => {
                    self.status = ResponseStatus::Failed;
                    self.error = Some(ResponseError {
                        code: None,
                        message: reason.as_str().to_string(),
                    });
                }
            },
        }
    }

    /// 流结束时收敛为 `(有序块, 用量, 状态, 错误)`，与 [`GenerateResult`] 的 status/error 同构。
    ///
    /// - `BTreeMap` 按 index 升序迭代，乱序 `BlockEnd` 自动归位；
    /// - 若流结束时仍存在「已 `BlockStart` 未 `BlockEnd`」的块（连接中断、provider 异常），
    ///   至少把 status 降级为 `Incomplete`，不得静默丢弃。
    pub fn finish(
        self,
    ) -> (
        Vec<ContentBlock>,
        Usage,
        ResponseStatus,
        Option<ResponseError>,
    ) {
        let mut status = self.status;
        if !self.open.is_empty() && status == ResponseStatus::Completed {
            // 「回答莫名截断」的直接证据：适配器发了 BlockStart 却没有对应的 BlockEnd。
            tracing::warn!(
                target: "model_provider::response",
                open_indices = ?self.open.keys().collect::<Vec<_>>(),
                open_types = ?self.open.values().collect::<Vec<_>>(),
                blocks = self.blocks.len(),
                "流结束时仍有未闭合的内容块，状态降级为 Incomplete"
            );
            status = ResponseStatus::Incomplete;
        }
        (
            self.blocks.into_values().collect(),
            self.usage,
            status,
            self.error,
        )
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn text_block(text: &str) -> ContentBlock {
        ContentBlock::Text {
            text: text.to_string(),
        }
    }

    #[test]
    fn content_block_serde_roundtrip_external_tag() {
        let block = ContentBlock::ToolCall {
            call_id: "call_1".to_string(),
            name: "get_weather".to_string(),
            arguments: r#"{"city":"beijing"}"#.to_string(),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(
            json,
            r#"{"ToolCall":{"call_id":"call_1","name":"get_weather","arguments":"{\"city\":\"beijing\"}"}}"#
        );
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn input_item_serde_roundtrip() {
        let item = InputItem::Message {
            role: Role::User,
            content: "你好".to_string(),
        };
        let json = serde_json::to_string(&item).unwrap();
        assert_eq!(json, r#"{"Message":{"role":"user","content":"你好"}}"#);
        let back: InputItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, item);
    }

    #[test]
    fn role_serde_lowercase() {
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), r#""system""#);
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            r#""assistant""#
        );
    }

    #[test]
    fn assembler_single_block() {
        let mut a = BlockAssembler::new();
        a.push(StreamChunk::BlockStart {
            index: 0,
            block_type: BlockType::Text,
        });
        a.push(StreamChunk::TextDelta {
            index: 0,
            delta: "hello".to_string(),
        });
        a.push(StreamChunk::BlockEnd {
            index: 0,
            block: text_block("hello"),
        });
        a.push(StreamChunk::Finish {
            reason: FinishReason::Stop,
        });
        let (blocks, _, status, err) = a.finish();
        assert_eq!(blocks, vec![text_block("hello")]);
        assert_eq!(status, ResponseStatus::Completed);
        assert!(err.is_none());
    }

    #[test]
    fn assembler_out_of_order_block_end() {
        let mut a = BlockAssembler::new();
        // index 1 先于 index 0 完成（并行工具调用）
        a.push(StreamChunk::BlockEnd {
            index: 1,
            block: text_block("second"),
        });
        a.push(StreamChunk::BlockEnd {
            index: 0,
            block: text_block("first"),
        });
        a.push(StreamChunk::Finish {
            reason: FinishReason::Stop,
        });
        let (blocks, _, status, _) = a.finish();
        assert_eq!(blocks, vec![text_block("first"), text_block("second")]);
        assert_eq!(status, ResponseStatus::Completed);
    }

    #[test]
    fn assembler_max_tokens_drops_unclosed_tool_call() {
        let mut a = BlockAssembler::new();
        a.push(StreamChunk::BlockStart {
            index: 0,
            block_type: BlockType::Text,
        });
        a.push(StreamChunk::BlockEnd {
            index: 0,
            block: text_block("partial text"),
        });
        // 工具调用只收到 BlockStart，未收到 BlockEnd（被 MaxTokens 截断）
        a.push(StreamChunk::BlockStart {
            index: 1,
            block_type: BlockType::ToolCall,
        });
        a.push(StreamChunk::Finish {
            reason: FinishReason::MaxTokens,
        });
        let (blocks, _, status, _) = a.finish();
        assert_eq!(blocks, vec![text_block("partial text")]);
        assert_eq!(status, ResponseStatus::Incomplete);
    }

    #[test]
    fn assembler_usage_and_status_aggregation() {
        let mut a = BlockAssembler::new();
        a.push(StreamChunk::Usage {
            usage: Usage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
            },
        });
        a.push(StreamChunk::Finish {
            reason: FinishReason::Error,
        });
        let (_, usage, status, err) = a.finish();
        assert_eq!(usage.total_tokens, 30);
        assert_eq!(status, ResponseStatus::Failed);
        assert_eq!(
            err,
            Some(ResponseError {
                code: None,
                message: "error".to_string(),
            })
        );
    }

    #[test]
    fn assembler_open_nonempty_downgrades_to_incomplete() {
        let mut a = BlockAssembler::new();
        a.push(StreamChunk::BlockStart {
            index: 0,
            block_type: BlockType::Text,
        });
        // 无 BlockEnd、无 Finish — 连接中断
        let (blocks, _, status, _) = a.finish();
        assert!(blocks.is_empty());
        assert_eq!(status, ResponseStatus::Incomplete);
    }
}
