// ============================================================================
// 上下文滚动压缩（Rolling Compaction）
// ============================================================================
//
// 永续会话（Peco）的历史无界增长，verbatim 窗口只保最近 N token 的内容，
// 被驱逐轮次需要以**结构化摘要**的形式钉回上下文，否则信息 100% 蒸发。
//
// 设计参照 Claude Code 的 auto-compact 与 Letta 的递归摘要：
//   - 触发：turn 边界，估算上下文超过 `trigger_tokens`
//   - 驱逐：从最旧端选择 turn，直到剩余 verbatim ≤ `keep_recent_tokens`
//   - 摘要：Flash 模型生成结构化摘要，与旧摘要合并（递归摘要）
//   - 落盘：摘要写入 `Session::pinned_summary`，随快照持久化；
//     被驱逐轮次物理移出快照
//
// 摘要模板固定四段：用户画像与偏好 / 已做决定 / 未完成事项 / 关键事实。
// 模型只需维护少量明确规则 — 复杂度体现在驱逐选择而非提示词。

use std::sync::Arc;

use async_trait::async_trait;
use model_provider::{ContentBlock, GenerateRequest, InputItem, ReasoningConfig, Role};

use super::context::estimate_item_tokens;
use super::error::AgentError;
use crate::session::Session;

/// 摘要定界标签 — pinned 消息的 content 恒被包裹其中，
/// 合并时可据此剥离旧摘要的包装。
pub const SUMMARY_OPEN: &str = "<earlier_context_summary>";
pub const SUMMARY_CLOSE: &str = "</earlier_context_summary>";

/// 摘要器的系统提示词。
const SUMMARY_SYSTEM_PROMPT: &str = r#"You are a conversation summarizer. Merge an earlier portion of the conversation history into the existing session summary.

Output format (Markdown, four fixed section headings, nothing else):

## User Profile & Preferences
## Decisions Made
## Unfinished Items
## Key Facts & Conclusions

Rules:
1. If an "existing summary" is provided, merge the new conversation's information into the matching sections, dedupe repeated items, and prefer the new conversation on conflicts;
2. If there is no existing summary, extract directly from the new conversation;
3. Keep only factual content useful for future conversation; discard chitchat and procedural narration;
4. For tool calls keep only conclusions, not command details;
5. At most 8 items per section, one line each, total length under 500 characters. Write summary content in the same language as the conversation."#;

/// 收尾报告（撞上 `max_turns` 上限）的系统提示词。
///
/// 与摘要提示词同风格：英文指令 + 固定小节，正文语言跟随对话。三个小节是给用户
/// 看的结论骨架；末行「回复继续」是续接交互的**唯一**载体 —— 没有按钮、没有事件、
/// 没有端点，提示词弄丢它就静默失效。
///
/// 「不能调用工具」必须在提示词里再申明一次 —— 请求本身已 `tools: vec![]`，但模型
/// 仍可能以叙述口吻「继续做事」，那会让报告退化成第二轮臆想的工作。
const EPILOGUE_SYSTEM_PROMPT: &str = r#"You are a turn-ending report writer. A conversation turn was forcibly stopped after hitting its maximum number of tool-calling iterations.

You CANNOT call any tools. Write only a closing report, in the same language as the conversation, using exactly these three Markdown sections:

## 本轮已完成
## 尚未完成
## 续接建议

Rules:
1. Base every statement strictly on the transcript provided. Never invent results, file contents, or conclusions the transcript does not show;
2. Summarize tool calls by their conclusions, not by command details;
3. If the work is unfinished, say plainly what was in flight and what the next concrete step is;
4. The report must end with this line verbatim, as its own final line: 如需继续，请回复"继续"。;
5. Keep the whole report under 400 characters."#;

// ============================================================================
// TurnSummarizer
// ============================================================================

/// 元任务模型 — 把一段转录合成为短文本。
///
/// 服务两个入口：轮边界的上下文压缩（[`Self::summarize`]）与撞上 `max_turns` 时的
/// 轮末收尾报告（[`Self::summarize_inflight`]）。两者同范式 —— 复用主 Agent 的
/// provider 与 Flash 档模型、无工具、关 reasoning、失败非致命 —— 差异只有提示词，
/// 以及结果是否需要摘要定界标签。故由同一个实现（[`ModelSummarizer`]）承担。
#[async_trait]
pub trait TurnSummarizer: Send + Sync {
    /// 生成合并后的新摘要。
    ///
    /// * `previous_summary` — 既有的 pinned 摘要正文（已剥离定界标签），可为空
    /// * `evicted_transcript` — 被驱逐轮次的纯文本转录
    async fn summarize(
        &self,
        previous_summary: Option<&str>,
        evicted_transcript: &str,
    ) -> Result<String, AgentError>;

    /// 把**在途轮**（staging，尚未分轮）的转录合成为轮末收尾报告。
    ///
    /// 与 [`Self::summarize`] 的区别有二：提示词不同；返回值**不**包摘要定界标签
    /// —— 收尾报告是写给用户看的结论，不是被钉回上下文的历史摘要。
    async fn summarize_inflight(&self, inflight_transcript: &str) -> Result<String, AgentError>;
}

/// 基于 [`ModelProvider`] 的摘要器 — 复用主 Agent 的 provider，
/// 用 Flash 档模型做低成本摘要。
pub struct ModelSummarizer {
    provider: Arc<dyn model_provider::ModelProvider>,
    model: String,
    max_output_tokens: u32,
}

impl ModelSummarizer {
    pub fn new(provider: Arc<dyn model_provider::ModelProvider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            max_output_tokens: 1024,
        }
    }

    /// 元任务调用的公共路径：一次**不带工具、关 reasoning** 的 Flash 模型调用，
    /// 返回修剪后的正文。摘要与收尾报告都走这里，差异只在提示词与温度。
    ///
    /// `label` 只用于错误信息 —— 两条路径共用失败口径，但要能看出是谁挂了。
    async fn generate_text(
        &self,
        label: &str,
        system_prompt: &str,
        user_content: String,
        temperature: f64,
    ) -> Result<String, AgentError> {
        let request = GenerateRequest {
            model: self.model.clone(),
            instructions: Some(system_prompt.to_string()),
            input: vec![Arc::new(InputItem::Message {
                role: Role::User,
                content: user_content.into(),
            })]
            .into(),
            tools: vec![],
            tool_choice: None,
            temperature: Some(temperature),
            top_p: None,
            max_output_tokens: Some(self.max_output_tokens),
            // 元任务不需要推理 — 关闭 thinking 降低延迟与成本
            reasoning: Some(ReasoningConfig {
                enabled: false,
                effort: None,
            }),
            text: None,
            additional_params: None,
        };

        let result = self
            .provider
            .generate_full(&request)
            .await
            .map_err(AgentError::from)?;

        if result.status != model_provider::ResponseStatus::Completed {
            return Err(AgentError::Compaction(format!(
                "{label} generation incomplete: status={:?}, error={:?}",
                result.status, result.error
            )));
        }

        let text: String = result
            .output
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        if text.trim().is_empty() {
            return Err(AgentError::Compaction(format!(
                "{label} generation returned empty text"
            )));
        }

        Ok(text.trim().to_string())
    }
}

#[async_trait]
impl TurnSummarizer for ModelSummarizer {
    async fn summarize(
        &self,
        previous_summary: Option<&str>,
        evicted_transcript: &str,
    ) -> Result<String, AgentError> {
        let mut user_content = String::new();
        match previous_summary.filter(|s| !s.trim().is_empty()) {
            Some(prev) => {
                user_content.push_str("[Existing summary]\n");
                user_content.push_str(prev);
                user_content.push_str("\n\n");
            }
            None => user_content.push_str("[Existing summary] (none)\n\n"),
        }
        user_content.push_str("[Conversation to summarize]\n");
        user_content.push_str(evicted_transcript);

        let text = self
            .generate_text("summary", SUMMARY_SYSTEM_PROMPT, user_content, 0.1)
            .await?;
        Ok(wrap_summary(text))
    }

    async fn summarize_inflight(&self, inflight_transcript: &str) -> Result<String, AgentError> {
        // 温度略高于摘要（0.2 vs 0.1）：报告要读出「哪些还没做完」，比机械压缩
        // 多一点判断余地。返回值不加定界标签 —— 它是给用户看的结论。
        self.generate_text(
            "epilogue",
            EPILOGUE_SYSTEM_PROMPT,
            inflight_transcript.to_string(),
            0.2,
        )
        .await
    }
}

/// 用定界标签包裹摘要正文。
fn wrap_summary(text: impl Into<String>) -> String {
    format!("{}\n{}\n{}", SUMMARY_OPEN, text.into(), SUMMARY_CLOSE)
}

/// 剥离摘要定界标签，返回正文。
///
/// 标签残缺（如截断）时逐侧尽力剥离：剥过的那一侧不再回退到原文。
pub fn strip_summary_wrapper(content: &str) -> &str {
    let stripped = content.strip_prefix(SUMMARY_OPEN).unwrap_or(content);
    let stripped = stripped.strip_suffix(SUMMARY_CLOSE).unwrap_or(stripped);
    stripped.trim()
}

// ============================================================================
// CompactionPolicy
// ============================================================================

/// 压缩策略参数 + 摘要器。
#[derive(Clone)]
pub struct CompactionPolicy {
    /// 触发阈值：pinned 摘要 + 全部 committed 的估算 token 超过该值时触发压缩。
    pub trigger_tokens: usize,
    /// 压缩后 verbatim 保留区目标 token（从最新轮往回保留）。
    pub keep_recent_tokens: usize,
    pub summarizer: Arc<dyn TurnSummarizer>,
}

/// 一次压缩的结果。
#[derive(Debug, Clone)]
pub struct CompactionOutcome {
    /// 物理驱逐的轮数
    pub evicted_turns: usize,
    /// 合并后的新摘要（已包裹定界标签）
    pub summary: String,
    /// 压缩前估算 token
    pub estimated_tokens_before: usize,
    /// 压缩后估算 token
    pub estimated_tokens_after: usize,
}

impl CompactionPolicy {
    /// 在 turn 边界检查并执行压缩（若有必要）。
    ///
    /// 返回 `Ok(None)` 表示无需压缩或单轮即超预算（不驱逐）。
    /// 失败（摘要模型错误等）不影响会话本身 — 调用方按非致命处理。
    pub async fn maybe_compact(
        &self,
        session: &mut Session,
    ) -> Result<Option<CompactionOutcome>, AgentError> {
        let pinned_tokens = session
            .pinned_summary()
            .map(|am| estimate_item_tokens(&am.message))
            .unwrap_or(0);
        let turns = session.committed_turns();
        if turns.is_empty() {
            return Ok(None);
        }
        let turn_tokens: Vec<usize> = turns
            .iter()
            .map(|turn| {
                turn.iter()
                    .map(|am| estimate_item_tokens(&am.message))
                    .sum()
            })
            .collect();
        let total: usize = pinned_tokens + turn_tokens.iter().sum::<usize>();

        // 未超阈值 — 不压缩
        if total <= self.trigger_tokens {
            return Ok(None);
        }

        // 从最新轮往回累计，确定 verbatim 保留区（至少保留 1 轮）
        let mut keep_count = 0usize;
        let mut keep_tokens = 0usize;
        for &t in turn_tokens.iter().rev() {
            if keep_count > 0 && keep_tokens + t > self.keep_recent_tokens {
                break;
            }
            keep_count += 1;
            keep_tokens += t;
        }
        let evict_count = turn_tokens.len() - keep_count;
        if evict_count == 0 {
            return Ok(None);
        }

        // 组装被驱逐轮次的纯文本转录（每条消息截断，防止超长 tool 输出撑爆摘要请求）
        let transcript = build_transcript(&turns[..evict_count]);

        // 旧摘要正文（剥离定界标签后传入，递归合并）
        let previous = session
            .pinned_summary()
            .and_then(|am| match am.message.as_ref() {
                InputItem::Message { content, .. } => {
                    Some(strip_summary_wrapper(&content.text_view()).to_owned())
                }
                _ => None,
            })
            .filter(|s| !s.is_empty());

        let summary = self
            .summarizer
            .summarize(previous.as_deref(), &transcript)
            .await?;

        let evicted = session
            .compact(evict_count, summary.clone())
            .map_err(|e| AgentError::Compaction(format!("session compaction failed: {e}")))?;
        if evicted == 0 {
            return Ok(None);
        }

        // evicted == evict_count（compact 的 clamp 不会更小，因 keep_count ≥ 1），
        // 保留区 token 即 keep_tokens，无需重新遍历 committed
        let pinned_after = session
            .pinned_summary()
            .map(|am| estimate_item_tokens(&am.message))
            .unwrap_or(0);

        Ok(Some(CompactionOutcome {
            evicted_turns: evicted,
            summary,
            estimated_tokens_before: total,
            estimated_tokens_after: pinned_after + keep_tokens,
        }))
    }
}

/// 单条消息在转录中的最大字符数。
const TRANSCRIPT_ITEM_MAX_CHARS: usize = 2000;
/// 整份转录的最大字符数（防止摘要请求本身超限）。
const TRANSCRIPT_MAX_CHARS: usize = 60_000;

/// 把一条消息格式化为转录行；不构成转录内容的消息（如 `Reasoning`）返回 `None`。
fn transcript_line(am: &crate::session::AnnotatedMessage) -> Option<String> {
    use model_provider::InputItem;
    match am.message.as_ref() {
        InputItem::Message { role, content } => {
            Some(format!("{}: {}", role_label(*role), content.text_view()))
        }
        InputItem::FunctionCall { name, .. } => Some(format!("[tool call {name}]")),
        InputItem::FunctionCallOutput { output, .. } => {
            Some(format!("[tool output] {}", output.text_view()))
        }
        InputItem::Reasoning { .. } => None,
        _ => None,
    }
}

/// 追加一条消息到转录（逐条截断，累计整份字符数）。
///
/// 返回 `false` 表示整份已达 [`TRANSCRIPT_MAX_CHARS`]，调用方应立即停止。
/// 跳过项（`transcript_line` 为 `None`）不算停止条件，与截断前的行为一致。
fn push_transcript_line(
    out: &mut String,
    total_chars: &mut usize,
    am: &crate::session::AnnotatedMessage,
) -> bool {
    let Some(line) = transcript_line(am) else {
        return true;
    };
    let truncated: String = line.chars().take(TRANSCRIPT_ITEM_MAX_CHARS).collect();
    *total_chars += truncated.chars().count() + 1;
    out.push_str(&truncated);
    out.push('\n');
    *total_chars < TRANSCRIPT_MAX_CHARS
}

/// 将驱逐的 turns 组装为 `role: content` 行的转录（轮间留一空行）。
fn build_transcript(evicted_turns: &[Vec<crate::session::AnnotatedMessage>]) -> String {
    let mut transcript = String::new();
    let mut total_chars = 0usize;
    'outer: for turn in evicted_turns {
        for am in turn {
            if !push_transcript_line(&mut transcript, &mut total_chars, am) {
                break 'outer;
            }
        }
        transcript.push('\n');
    }
    transcript
}

/// 将一段扁平消息序列组装为转录，截断规则与 [`build_transcript`] 完全一致。
///
/// 供**在途轮**（staging，尚未分轮）使用 —— 收尾报告要在冻结前拿到本轮转录。
/// 两级截断（逐条 [`TRANSCRIPT_ITEM_MAX_CHARS`] / 整份 [`TRANSCRIPT_MAX_CHARS`]）
/// 与格式化只此一处，两个入口不各写一份常量。
pub(crate) fn build_flat_transcript(messages: &[crate::session::AnnotatedMessage]) -> String {
    let mut transcript = String::new();
    let mut total_chars = 0usize;
    for am in messages {
        if !push_transcript_line(&mut transcript, &mut total_chars, am) {
            break;
        }
    }
    transcript
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System | Role::Developer => "system",
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::MessageSource;

    /// 收尾提示词里那行「回复继续」是整个续接交互的**唯一**载体 —— 没有按钮、
    /// 没有事件、没有端点，提示词弄丢它就静默失效。不带工具同理：请求已
    /// `tools: vec![]`，仍须在提示词里重申。
    #[test]
    fn test_epilogue_prompt_keeps_continue_hint_and_tool_ban() {
        assert!(
            EPILOGUE_SYSTEM_PROMPT.contains("如需继续，请回复"),
            "收尾提示词必须保留续接提示，否则用户拿不到「怎么继续」的指引"
        );
        assert!(
            EPILOGUE_SYSTEM_PROMPT.contains("CANNOT call any tools"),
            "请求已 tools: vec![]，提示词仍须重申，否则模型可能改以叙述口吻继续做事"
        );
    }

    /// 固定输出的假 provider —— 直接驱动 [`ModelSummarizer::generate_text`]，
    /// 覆盖摘要与收尾共用的成/败两条分支。
    struct FixedProvider {
        text: &'static str,
        status: model_provider::ResponseStatus,
        fail: bool,
    }

    #[async_trait]
    impl model_provider::ModelProvider for FixedProvider {
        fn name(&self) -> &str {
            "fixed"
        }

        async fn generate_full(
            &self,
            _request: &model_provider::GenerateRequest,
        ) -> Result<model_provider::GenerateResult, model_provider::ProviderError> {
            if self.fail {
                return Err(model_provider::ProviderError::Request("boom".into()));
            }
            Ok(model_provider::GenerateResult {
                id: "r1".to_string(),
                output: vec![ContentBlock::Text {
                    text: self.text.to_string(),
                }],
                usage: model_provider::Usage::default(),
                status: self.status,
                finish_reason: None,
                error: None,
            })
        }

        async fn generate_stream(
            &self,
            _request: &model_provider::GenerateRequest,
        ) -> Result<model_provider::GenerateStream, model_provider::ProviderError> {
            unimplemented!("meta tasks never stream")
        }
    }

    fn meta_summarizer(
        text: &'static str,
        status: model_provider::ResponseStatus,
        fail: bool,
    ) -> ModelSummarizer {
        ModelSummarizer::new(Arc::new(FixedProvider { text, status, fail }), "m")
    }

    /// 收尾正文被修剪，且**不带**摘要定界标签 —— 它是给用户看的结论而非历史摘要。
    #[tokio::test]
    async fn test_summarize_inflight_returns_trimmed_text_without_wrapper() {
        let s = meta_summarizer(
            "  报告正文  ",
            model_provider::ResponseStatus::Completed,
            false,
        );
        let out = s.summarize_inflight("转录").await.unwrap();
        assert_eq!(out, "报告正文");
        assert!(
            !out.contains(SUMMARY_OPEN),
            "收尾不得包摘要定界标签，否则会被当成 pinned 历史摘要"
        );
    }

    #[tokio::test]
    async fn test_summarize_inflight_incomplete_status_is_error() {
        let s = meta_summarizer("x", model_provider::ResponseStatus::Incomplete, false);
        assert!(s.summarize_inflight("转录").await.is_err());
    }

    #[tokio::test]
    async fn test_summarize_inflight_empty_text_is_error() {
        let s = meta_summarizer("   ", model_provider::ResponseStatus::Completed, false);
        assert!(s.summarize_inflight("转录").await.is_err());
    }

    #[tokio::test]
    async fn test_summarize_inflight_provider_error_propagates() {
        let s = meta_summarizer("x", model_provider::ResponseStatus::Completed, true);
        assert!(s.summarize_inflight("转录").await.is_err());
    }

    /// 固定输出的假摘要器（摘要远小于原文 — 压缩必然减小 token）。
    struct MockSummarizer;

    #[async_trait]
    impl TurnSummarizer for MockSummarizer {
        async fn summarize(
            &self,
            _previous: Option<&str>,
            _evicted: &str,
        ) -> Result<String, AgentError> {
            Ok(wrap_summary(
                "用户偏好中文交流；已决定用 Rust；待办：写测试",
            ))
        }

        async fn summarize_inflight(&self, _transcript: &str) -> Result<String, AgentError> {
            unimplemented!("compaction tests never compose turn epilogues")
        }
    }

    fn make_session_with_turns(n: usize) -> Session {
        let mut s = Session::new("test".to_string(), "test".to_string());
        for i in 0..n {
            s.start_turn(format!("这是第 {i} 轮的问题，内容足够长以产生 token 占用。").into())
                .unwrap();
            s.stage_item(
                MessageSource::ModelGeneration,
                InputItem::Message {
                    role: Role::Assistant,
                    content: format!("这是第 {i} 轮的回答，同样足够长以产生 token 占用。").into(),
                },
            )
            .unwrap();
            let _ = s.commit_turn().unwrap();
        }
        s
    }

    #[test]
    fn test_summary_wrapper_roundtrip() {
        let wrapped = wrap_summary("正文");
        assert_eq!(strip_summary_wrapper(&wrapped), "正文");
        assert_eq!(strip_summary_wrapper("无标签"), "无标签");
        // 标签残缺：只剥存在的一侧，且不把已剥掉的前缀带回来
        assert_eq!(
            strip_summary_wrapper("<earlier_context_summary>残缺"),
            "残缺"
        );
        assert_eq!(
            strip_summary_wrapper("残缺</earlier_context_summary>"),
            "残缺"
        );
    }

    /// 扁平入口与分轮入口共享同一套逐条格式化与截断，差异只该是「轮间空行」。
    ///
    /// 用超长单条消息覆盖截断路径 —— 这是两个入口最容易漂移的地方。
    #[test]
    fn test_flat_transcript_shares_format_and_truncation_with_turn_entry() {
        let long = "甲".repeat(TRANSCRIPT_ITEM_MAX_CHARS + 500);

        let mut session = Session::new("t".to_string(), "d".to_string());
        session.start_turn("问题".into()).unwrap();
        session
            .stage_item(
                MessageSource::ModelGeneration,
                InputItem::Message {
                    role: Role::Assistant,
                    content: long.clone().into(),
                },
            )
            .unwrap();
        session
            .stage_item(
                MessageSource::ModelGeneration,
                InputItem::FunctionCall {
                    call_id: "c1".into(),
                    name: "shell".into(),
                    arguments: "{}".into(),
                },
            )
            .unwrap();
        session
            .stage_item(
                MessageSource::ToolExecution {
                    tool_name: "shell".into(),
                },
                InputItem::FunctionCallOutput {
                    call_id: "c1".into(),
                    output: "输出".into(),
                },
            )
            .unwrap();
        let _token = session.commit_turn().unwrap();

        let turn = &session.committed_turns()[0];
        let flat = build_flat_transcript(turn);
        let by_turn = build_transcript(std::slice::from_ref(turn));

        // 分轮入口 = 扁平内容 + 一个轮间空行
        assert_eq!(
            by_turn,
            format!("{flat}\n"),
            "两条入口除轮间空行外必须逐字节相同"
        );
        // 逐条截断生效：超长正文被截到上限（不含前缀与换行）
        assert!(
            !flat.contains(&long),
            "超长条目必须被逐条截断，不能整段进转录"
        );
        assert!(
            flat.chars().count() < long.chars().count(),
            "截断后转录必然短于原文"
        );
    }

    #[tokio::test]
    async fn test_no_compaction_below_trigger() {
        let policy = CompactionPolicy {
            trigger_tokens: usize::MAX,
            keep_recent_tokens: 1000,
            summarizer: Arc::new(MockSummarizer),
        };
        let mut session = make_session_with_turns(3);
        assert!(policy.maybe_compact(&mut session).await.unwrap().is_none());
        assert_eq!(session.committed_turns().len(), 3);
    }

    #[tokio::test]
    async fn test_compaction_evicts_oldest_and_pins() {
        // 每 turn 约 2 条 × 25 字 × 0.6 ≈ 30 token。阈值 80 触发，保留区 40。
        let policy = CompactionPolicy {
            trigger_tokens: 80,
            keep_recent_tokens: 40,
            summarizer: Arc::new(MockSummarizer),
        };
        let mut session = make_session_with_turns(4);
        let outcome = policy.maybe_compact(&mut session).await.unwrap().unwrap();

        assert!(outcome.evicted_turns >= 1);
        assert!(outcome.estimated_tokens_after < outcome.estimated_tokens_before);
        assert!(session.pinned_summary().is_some());
        // 剩余轮数 + pinned = 5 条引用
        let refs: Vec<_> = session.all_message_refs().collect();
        assert_eq!(refs.len(), 1 + (4 - outcome.evicted_turns) * 2);
        // turn_index 重编号无空洞
        let max_turn = session
            .committed_turns()
            .last()
            .and_then(|t| t.last())
            .map(|am| am.turn_index)
            .unwrap();
        assert_eq!(max_turn, 4 - outcome.evicted_turns - 1);
    }

    #[tokio::test]
    async fn test_single_oversized_turn_not_evicted() {
        let policy = CompactionPolicy {
            trigger_tokens: 1, // 任何内容都触发
            keep_recent_tokens: 0,
            summarizer: Arc::new(MockSummarizer),
        };
        let mut session = make_session_with_turns(1);
        // 单轮：keep_count 恒为 1，无可驱逐
        assert!(policy.maybe_compact(&mut session).await.unwrap().is_none());
        assert_eq!(session.committed_turns().len(), 1);
    }

    #[tokio::test]
    async fn test_recursive_merge_passes_previous_summary() {
        struct CaptureSummarizer {
            seen_previous: std::sync::Mutex<Option<String>>,
        }
        #[async_trait]
        impl TurnSummarizer for CaptureSummarizer {
            async fn summarize(
                &self,
                previous: Option<&str>,
                _evicted: &str,
            ) -> Result<String, AgentError> {
                *self.seen_previous.lock().unwrap() = previous.map(str::to_string);
                Ok(wrap_summary("v2"))
            }

            async fn summarize_inflight(&self, _transcript: &str) -> Result<String, AgentError> {
                unimplemented!("compaction tests never compose turn epilogues")
            }
        }

        let summarizer = Arc::new(CaptureSummarizer {
            seen_previous: std::sync::Mutex::new(None),
        });
        let policy = CompactionPolicy {
            trigger_tokens: 1,
            keep_recent_tokens: 0,
            summarizer: summarizer.clone(),
        };

        let mut session = make_session_with_turns(3);
        let _ = session.compact(1, wrap_summary("v1")).unwrap();
        policy.maybe_compact(&mut session).await.unwrap().unwrap();

        // 合并时传入的是剥离标签后的旧摘要正文 "v1"
        let seen = summarizer
            .seen_previous
            .lock()
            .unwrap()
            .clone()
            .expect("summarizer should have been invoked");
        assert_eq!(seen, "v1");

        // 最后一次压缩后 pinned = "v2"
        let pinned = session.pinned_summary().unwrap();
        assert_eq!(
            strip_summary_wrapper(
                match pinned.message.as_ref() {
                    InputItem::Message { content, .. } => content.text_view(),
                    _ => panic!("expected message"),
                }
                .as_ref(),
            ),
            "v2"
        );
    }
}
