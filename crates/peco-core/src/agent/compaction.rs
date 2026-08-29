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
const SUMMARY_SYSTEM_PROMPT: &str = r#"你是会话摘要器。将一段更早的对话历史合并进既有的会话摘要。

输出格式（Markdown，四段固定标题，无其它内容）：

## 用户画像与偏好
## 已做决定
## 未完成事项
## 关键事实与结论

规则：
1. 若提供了「既有摘要」，新对话中的信息**合并**进对应段落，重复条目去重，冲突时以新对话为准；
2. 若无既有摘要，直接从新对话中提取；
3. 只保留对后续对话有用的事实性内容，丢弃寒暄、过程性描述；
4. 工具调用只保留结论，不保留命令细节；
5. 每段最多 8 条，每条一行，总长度不超过 500 字。"#;

// ============================================================================
// TurnSummarizer
// ============================================================================

/// 摘要器 — 将被驱逐的轮次转录合并为结构化摘要。
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
                user_content.push_str("【既有摘要】\n");
                user_content.push_str(prev);
                user_content.push_str("\n\n");
            }
            None => user_content.push_str("【既有摘要】（无）\n\n"),
        }
        user_content.push_str("【被摘要的对话】\n");
        user_content.push_str(evicted_transcript);

        let request = GenerateRequest {
            model: self.model.clone(),
            instructions: Some(SUMMARY_SYSTEM_PROMPT.to_string()),
            input: vec![Arc::new(InputItem::Message {
                role: Role::User,
                content: user_content,
            })]
            .into(),
            tools: vec![],
            tool_choice: None,
            temperature: Some(0.1),
            top_p: None,
            max_output_tokens: Some(self.max_output_tokens),
            // 摘要不需要推理 — 关闭 thinking 降低延迟与成本
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
                "summary generation incomplete: status={:?}, error={:?}",
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
            return Err(AgentError::Compaction(
                "summary generation returned empty text".to_string(),
            ));
        }

        Ok(wrap_summary(text.trim()))
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
                InputItem::Message { content, .. } => Some(strip_summary_wrapper(content)),
                _ => None,
            })
            .filter(|s| !s.is_empty());

        let summary = self.summarizer.summarize(previous, &transcript).await?;

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

/// 将驱逐的 turns 组装为 `role: content` 行的转录。
fn build_transcript(evicted_turns: &[Vec<crate::session::AnnotatedMessage>]) -> String {
    use model_provider::InputItem;

    let mut transcript = String::new();
    let mut total_chars = 0usize;
    'outer: for turn in evicted_turns {
        for am in turn {
            let line = match am.message.as_ref() {
                InputItem::Message { role, content } => {
                    format!("{}: {}", role_label(*role), content)
                }
                InputItem::FunctionCall { name, .. } => format!("[调用工具 {name}]"),
                InputItem::FunctionCallOutput { output, .. } => format!("[工具输出] {output}"),
                InputItem::Reasoning { .. } => continue,
                _ => continue,
            };
            let truncated: String = line.chars().take(TRANSCRIPT_ITEM_MAX_CHARS).collect();
            total_chars += truncated.chars().count() + 1;
            transcript.push_str(&truncated);
            transcript.push('\n');
            if total_chars >= TRANSCRIPT_MAX_CHARS {
                break 'outer;
            }
        }
        transcript.push('\n');
    }
    transcript
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::User => "用户",
        Role::Assistant => "助手",
        Role::System | Role::Developer => "系统",
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::MessageSource;

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
    }

    fn make_session_with_turns(n: usize) -> Session {
        let mut s = Session::new("test".to_string(), "test".to_string());
        for i in 0..n {
            s.start_turn(format!(
                "这是第 {i} 轮的问题，内容足够长以产生 token 占用。"
            ))
            .unwrap();
            s.stage_item(
                MessageSource::ModelGeneration,
                InputItem::Message {
                    role: Role::Assistant,
                    content: format!("这是第 {i} 轮的回答，同样足够长以产生 token 占用。"),
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
            strip_summary_wrapper(match pinned.message.as_ref() {
                InputItem::Message { content, .. } => content.as_str(),
                _ => panic!("expected message"),
            }),
            "v2"
        );
    }
}
