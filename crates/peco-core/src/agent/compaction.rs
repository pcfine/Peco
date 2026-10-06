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
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use model_provider::{ContentBlock, GenerateRequest, InputItem, ReasoningConfig, Role};

use super::context::{estimate_item_tokens, estimate_viewable_item_tokens};
use super::error::AgentError;
use crate::session::{Session, StripCaps};
pub use crate::session::{StripEvaluation, StripOutcome};

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

/// 收尾报告（撞上 `max_iterations` 上限）的系统提示词。
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
/// 服务两个入口：轮边界的上下文压缩（[`Self::summarize`]）与撞上 `max_iterations` 时的
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

/// 元任务调用的输出预算。
///
/// 摘要提示词要求 500 字符以内，但实测正文常到 1500–1900 字符（约 800–1300 token）。
/// 这里取过 1024 与 4096：前者贴着真实输出长度，后者则被实测撞顶 —— 转录较长时
/// 模型会在预算内一路写满，响应成 `Incomplete`，而摘要链路把非 `Completed` 直接
/// 判为失败，压缩从此永不推进。16384 与 provider 默认档一致，留足余量；
/// 这是**上限**而非目标长度，超出提示词要求的部分照收。
const SUMMARIZER_MAX_OUTPUT_TOKENS: u32 = 16384;

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
            max_output_tokens: SUMMARIZER_MAX_OUTPUT_TOKENS,
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

/// 连续摘要失败达到该次数即放弃调用摘要模型，转为降级强制驱逐。
///
/// 阈值存在的意义是「给瞬时故障几次机会，但绝不允许死锁」：摘要请求由固定的 pinned
/// 摘要 + 固定的被驱逐转录拼成，只要失败原因是请求本身（超长、内容触发过滤），
/// 重试多少次都是同一个请求 —— 不设上限就是每轮边界白烧一次模型调用且永不推进。
const MAX_CONSECUTIVE_SUMMARY_FAILURES: u32 = 3;

/// 降级摘要保留的原始转录字符上限。
const DEGRADED_SUMMARY_MAX_CHARS: usize = 4_000;

/// 压缩策略参数 + 摘要器。
#[derive(Clone)]
pub struct CompactionPolicy {
    /// 触发阈值：pinned 摘要 + 全部 committed 的估算 token 超过该值时触发压缩。
    pub trigger_tokens: usize,
    /// 压缩后 verbatim 保留区目标 token（从最新轮往回保留）。
    pub keep_recent_tokens: usize,
    pub summarizer: Arc<dyn TurnSummarizer>,
    /// 跨 turn 边界累计的「摘要生成失败」计数，成功后清零。
    ///
    /// `Arc<AtomicU32>` 而非裸 `u32`：`CompactionPolicy` 必须保持 `Clone`
    /// （随 `PecoConfig` 被复制），而计数是要跨调用累积的状态 —— 若随克隆各持一份，
    /// 每个副本都从 0 开始，阈值永远够不着。私有字段：计数是策略内部状态，
    /// 调用方只能经 [`CompactionPolicy::new`] 构造（初始恒为 0）。
    consecutive_summary_failures: Arc<AtomicU32>,
    /// 历史削减策略；`None` = 不扫描不改写（legacy 行为）。
    strip: Option<StripPolicy>,
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
    /// 本次摘要是否为**降级摘要**（摘要模型连续失败后的机械兜底）。
    ///
    /// 降级仍然驱逐、仍然推进，只是 pinned 摘要退化为原始转录截断 ——
    /// 这条路径必须可观测，否则「上下文被悄悄换成了转录碎片」无人知晓。
    pub degraded: bool,
}

/// 一个 turn 边界的观测结算：token 口径、削减评估、折叠载荷的内存态汇总。
///
/// 不进快照 —— 观测数据由边界钩子送往服务端落库，会话本体只关心内容本身
/// （加进快照就等于给持久化格式加字段，旧快照兼容成本全部转嫁给读侧）。
#[derive(Debug, Clone)]
pub struct BoundaryReduction {
    /// 本边界的 turn 计数（历史累计，压缩不回退）。
    pub turn_index: usize,
    /// pinned 摘要估算 token（post-strip、pre-fold —— 即触发判定所见）。
    pub pinned_tokens: usize,
    /// 全部 committed 轮的全量估算 token（含 tool 输出与 reasoning，折叠触发口径）。
    pub committed_full_tokens: usize,
    /// 全部 committed 轮的 viewable 估算 token（非过滤窗口口径，观测用）。
    pub committed_view_tokens: usize,
    /// 本边界是否由 viewable 守卫触发（守卫未启用时恒假）。
    pub guard_triggered: bool,
    /// 削减生效值；启用生效削减时为 `Some`（零改写则 `changed_items` 为 0），
    /// shadow 观测路径恒 `None` —— 评估值不驱动任何落盘。
    pub strip: Option<StripOutcome>,
    /// 削减评估值（只算不做）；未装配削减策略时为 `None`。
    pub strip_evaluation: Option<StripEvaluation>,
    /// 折叠载荷；未触发折叠时为 `None`。
    pub compaction: Option<CompactionOutcome>,
}

impl BoundaryReduction {
    /// 本边界是否发生了折叠（轮次被物理驱逐）。
    pub fn folded(&self) -> bool {
        self.compaction.is_some()
    }

    /// 本边界是否需要落盘 —— 只看**生效**值。
    ///
    /// 评估值（shadow 观测）不改任何会话内容，为其落盘纯属浪费 IO；
    /// 实际改写与折叠都动了 committed / pinned，必须落盘。
    pub fn mutated(&self) -> bool {
        self.strip.as_ref().is_some_and(|s| s.mutated()) || self.compaction.is_some()
    }
}

/// 历史削减（strip）参数 —— 与折叠正交的机械内容削减，边界上先削后折。
///
/// 削减对象是「模型永不重发、摘要也永不读取」的内容（历史 reasoning、工具调用
/// 参数），削减让全量口径变轻从而降低折叠频率。评估与生效共用
/// [`Session::strip_stale_content`] 的同一份扫描。未装配（`None`）时行为
/// 与不引入削减的版本逐字节一致。
#[derive(Debug, Clone)]
pub struct StripPolicy {
    /// 最近 N 轮已完成对话不参与削减 —— UI 默认页可见区恒无损。
    pub keep_recent_turns: usize,
    /// 删除历史 reasoning 条目。
    pub strip_reasoning: bool,
    /// 清空历史工具调用参数。
    pub strip_tool_args: bool,
    /// 历史工具输出截断上限（字符数）：`0` = 关闭。
    ///
    /// 非 0 时不小于 [`TRANSCRIPT_TOOL_OUTPUT_MIN_CAP`] —— 配置层校验，
    /// 违规值在读取端回退默认。
    pub tool_output_max_chars: usize,
    /// 生效开关：`true` 时就地改写历史，`false` 只评估不改写（shadow 观测）。
    ///
    /// 默认 `false` —— 未显式开启时边界结算与不引入削减的版本逐字节一致。
    pub enforce: bool,
}

impl StripPolicy {
    fn caps(&self) -> StripCaps {
        StripCaps {
            strip_reasoning: self.strip_reasoning,
            strip_tool_args: self.strip_tool_args,
            tool_output_max_chars: self.tool_output_max_chars,
        }
    }
}

impl CompactionPolicy {
    pub fn new(
        trigger_tokens: usize,
        keep_recent_tokens: usize,
        summarizer: Arc<dyn TurnSummarizer>,
    ) -> Self {
        Self {
            trigger_tokens,
            keep_recent_tokens,
            summarizer,
            consecutive_summary_failures: Arc::new(AtomicU32::new(0)),
            strip: None,
        }
    }

    /// 装配历史削减：每个 turn 边界在折叠前先跑削减扫描。
    ///
    /// builder 而非改 `new` 签名 —— 既有调用点与测试不关心削减，
    /// 未显式装配即完全不走扫描路径。
    pub fn with_strip(mut self, strip: StripPolicy) -> Self {
        self.strip = Some(strip);
        self
    }

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

        // 摘要生成：失败只累计计数，不改变会话。连续失败到阈值时**不再重试**，
        // 改用机械摘要 —— 宁可 pinned 摘要退化，也不能让驱逐永远不发生。
        let (summary, degraded) = match self
            .summarizer
            .summarize(previous.as_deref(), &transcript)
            .await
        {
            Ok(summary) => (summary, false),
            Err(e) => {
                let failures = self
                    .consecutive_summary_failures
                    .fetch_add(1, Ordering::SeqCst)
                    + 1;
                if failures < MAX_CONSECUTIVE_SUMMARY_FAILURES {
                    return Err(e);
                }
                (degraded_summary(&transcript, failures), true)
            }
        };

        let evicted = session
            .compact(evict_count, summary.clone())
            .map_err(|e| AgentError::Compaction(format!("session compaction failed: {e}")))?;
        if evicted == 0 {
            return Ok(None);
        }

        // 驱逐成功即摘要链路已在推进 —— 清零计数。降级路径同样清零：否则下一个
        // 边界上计数仍贴着阈值，一次普通失败就会立刻再降级。
        self.consecutive_summary_failures.store(0, Ordering::SeqCst);

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
            degraded,
        }))
    }

    /// turn 边界编排入口：削减（enforce 时改写，否则只评估）→ 结算 token 口径 → 折叠。
    ///
    /// 返回观测结算与折叠载荷，调用方按 [`BoundaryReduction::mutated`] 决定
    /// 落盘、按 `compaction` / `strip_evaluation` 分别通知钩子 —— 编排留在
    /// core 一处，服务端只消费结果，口径不可能在两端各算一遍。
    ///
    /// 评估与生效是同一份扫描：`strip`（生效值）由 `strip_evaluation` 的
    /// 计数直接转录，两条路径的数值不可能对不上。
    ///
    /// 折叠失败按非致命上抛（`Err`），由调用方 `match` 记日志后继续 ——
    /// 此处不用 `?` 向 looper 冒泡是有意的既有契约。
    pub async fn apply_boundary(
        &self,
        session: &mut Session,
    ) -> Result<BoundaryReduction, AgentError> {
        // ① 削减（先削后折：结算口径必须等于折叠触发所见）
        let (strip_evaluation, strip) = match &self.strip {
            Some(policy) => {
                let eval = session
                    .strip_stale_content(policy.keep_recent_turns, policy.caps(), policy.enforce)
                    .map_err(|e| {
                        AgentError::Compaction(format!("strip stale content failed: {e}"))
                    })?;
                let outcome = policy.enforce.then_some(StripOutcome {
                    changed_items: eval.would_change_items,
                    released_tokens: eval.would_release_tokens,
                    skipped_turns: eval.skipped_turns,
                });
                (Some(eval), outcome)
            }
            None => (None, None),
        };

        // ② 结算 token 口径（post-strip、pre-fold）
        let pinned_tokens = session
            .pinned_summary()
            .map(|am| estimate_item_tokens(&am.message))
            .unwrap_or(0);
        let (committed_full_tokens, committed_view_tokens) = session
            .committed_turns()
            .iter()
            .flat_map(|turn| turn.iter())
            .fold((0usize, 0usize), |(full, view), am| {
                (
                    full + estimate_item_tokens(&am.message),
                    view + estimate_viewable_item_tokens(&am.message),
                )
            });

        // ③ 折叠（触发口径与签名不变 —— maybe_compact 是唯一的折叠实现）
        let compaction = self.maybe_compact(session).await?;

        Ok(BoundaryReduction {
            turn_index: session.turn_index(),
            pinned_tokens,
            committed_full_tokens,
            committed_view_tokens,
            guard_triggered: false,
            strip,
            strip_evaluation,
            compaction,
        })
    }
}

/// 机械摘要：摘要模型连续失败时，以被驱逐转录的前 [`DEGRADED_SUMMARY_MAX_CHARS`]
/// 个字符充当 pinned 摘要。
///
/// 前缀必须写明降级原因 —— 读到这条摘要的模型（以及人）要知道它是原始转录碎片，
/// 而不是模型的结论。截断走 `chars().take()`：转录是中文，按字节切会切坏 UTF-8。
fn degraded_summary(transcript: &str, failures: u32) -> String {
    let head: String = transcript
        .chars()
        .take(DEGRADED_SUMMARY_MAX_CHARS)
        .collect();
    wrap_summary(format!(
        "[降级摘要] 摘要模型连续 {failures} 次失败，以下为被折叠历史的原始转录截断：\n{head}"
    ))
}

/// 单条消息在转录中的最大字符数。
///
/// pub：工具输出截断的默认上限与下界（[`TRANSCRIPT_TOOL_OUTPUT_MIN_CAP`]）
/// 都从它派生 —— 削减口径与转录实现必须是同一个数，各自定义一个 2000
/// 迟早漂移。
pub const TRANSCRIPT_ITEM_MAX_CHARS: usize = 2000;
/// 整份转录的最大字符数（防止摘要请求本身超限）。
const TRANSCRIPT_MAX_CHARS: usize = 60_000;
/// tool 输出的转录行前缀（[`transcript_line`] 拼接用）。
const TRANSCRIPT_TOOL_OUTPUT_PREFIX: &str = "[tool output] ";

/// 历史工具输出截断上限的下界（字符数）。
///
/// 削到 S 字符后行 = `前缀 + S`；per-item 截断等式要求削后行不短于单条
/// 上限，否则取到的是削后整行而非与原行相同的前缀片段 —— 前缀 14 字符
/// ⇒ 下界 1986。
pub const TRANSCRIPT_TOOL_OUTPUT_MIN_CAP: usize =
    TRANSCRIPT_ITEM_MAX_CHARS - TRANSCRIPT_TOOL_OUTPUT_PREFIX.len();

/// 把一条消息格式化为转录行；不构成转录内容的消息（如 `Reasoning`）返回 `None`。
fn transcript_line(am: &crate::session::AnnotatedMessage) -> Option<String> {
    use model_provider::InputItem;
    match am.message.as_ref() {
        InputItem::Message { role, content } => {
            Some(format!("{}: {}", role_label(*role), content.text_view()))
        }
        InputItem::FunctionCall { name, .. } => Some(format!("[tool call {name}]")),
        InputItem::FunctionCallOutput { output, .. } => Some(format!(
            "{TRANSCRIPT_TOOL_OUTPUT_PREFIX}{}",
            output.text_view()
        )),
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

    const THRESHOLD_LOOP_CAP: u32 = 8;

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
        let policy = CompactionPolicy::new(usize::MAX, 1000, Arc::new(MockSummarizer));
        let mut session = make_session_with_turns(3);
        assert!(policy.maybe_compact(&mut session).await.unwrap().is_none());
        assert_eq!(session.committed_turns().len(), 3);
    }

    #[tokio::test]
    async fn test_compaction_evicts_oldest_and_pins() {
        // 每 turn 约 2 条 × 25 字 × 0.6 ≈ 30 token。阈值 80 触发，保留区 40。
        let policy = CompactionPolicy::new(80, 40, Arc::new(MockSummarizer));
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
        // 任何内容都触发
        let policy = CompactionPolicy::new(1, 0, Arc::new(MockSummarizer));
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
        let policy = CompactionPolicy::new(1, 0, summarizer.clone());

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

    /// 追加一轮到既有会话，内容量级与 [`make_session_with_turns`] 一致。
    fn push_turn(session: &mut Session, i: usize) {
        session
            .start_turn(format!("这是第 {i} 轮的问题，内容足够长以产生 token 占用。").into())
            .unwrap();
        session
            .stage_item(
                MessageSource::ModelGeneration,
                InputItem::Message {
                    role: Role::Assistant,
                    content: format!("这是第 {i} 轮的回答，同样足够长以产生 token 占用。").into(),
                },
            )
            .unwrap();
        let _ = session.commit_turn().unwrap();
    }

    /// 记录请求的假 provider —— 只关心摘要器发出的输出预算。
    struct CaptureProvider {
        seen_max_output_tokens: std::sync::Mutex<Option<u32>>,
    }

    #[async_trait]
    impl model_provider::ModelProvider for CaptureProvider {
        fn name(&self) -> &str {
            "capture"
        }

        async fn generate_full(
            &self,
            request: &model_provider::GenerateRequest,
        ) -> Result<model_provider::GenerateResult, model_provider::ProviderError> {
            *self.seen_max_output_tokens.lock().unwrap() = request.max_output_tokens;
            Ok(model_provider::GenerateResult {
                id: "r1".to_string(),
                output: vec![ContentBlock::Text {
                    text: "摘要".to_string(),
                }],
                usage: model_provider::Usage::default(),
                status: model_provider::ResponseStatus::Completed,
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

    /// 摘要请求的输出预算必须留出余量：预算一旦被撞顶，响应即成 `Incomplete`，
    /// 摘要链路据此判失败，压缩永不推进（实测 4096 已不足）。
    #[tokio::test]
    async fn test_summarizer_requests_full_output_budget() {
        let capture = Arc::new(CaptureProvider {
            seen_max_output_tokens: std::sync::Mutex::new(None),
        });
        let provider: Arc<dyn model_provider::ModelProvider> = capture.clone();
        let summarizer = ModelSummarizer::new(provider, "m");

        summarizer.summarize(None, "转录").await.unwrap();

        assert_eq!(
            *capture.seen_max_output_tokens.lock().unwrap(),
            Some(SUMMARIZER_MAX_OUTPUT_TOKENS),
            "摘要请求必须带上摘要器的输出预算"
        );
        assert_eq!(
            SUMMARIZER_MAX_OUTPUT_TOKENS, 16_384,
            "预算须显著高于实测摘要长度，贴顶会让压缩长期失败"
        );
    }

    /// 恒定失败的摘要器 —— 驱动连续失败 → 降级驱逐。
    struct FailingSummarizer;

    #[async_trait]
    impl TurnSummarizer for FailingSummarizer {
        async fn summarize(
            &self,
            _previous: Option<&str>,
            _evicted: &str,
        ) -> Result<String, AgentError> {
            Err(AgentError::Compaction(
                "summary generation incomplete".to_string(),
            ))
        }

        async fn summarize_inflight(&self, _transcript: &str) -> Result<String, AgentError> {
            unimplemented!("compaction tests never compose turn epilogues")
        }
    }

    /// 摘要模型持续失败时，压缩**必须**在有限次后仍然驱逐 —— 否则每轮边界重发同一个
    /// 必然失败的请求，白烧 token 且上下文无界增长。
    #[tokio::test]
    async fn test_consecutive_summary_failures_degrade_to_forced_eviction() {
        let policy = CompactionPolicy::new(1, 0, Arc::new(FailingSummarizer));
        let mut session = make_session_with_turns(4);

        // 阈值之前：维持「非致命失败」语义，一轮都不驱逐
        for attempt in 1..MAX_CONSECUTIVE_SUMMARY_FAILURES.min(THRESHOLD_LOOP_CAP) {
            assert!(
                policy.maybe_compact(&mut session).await.is_err(),
                "第 {attempt} 次失败应原样返回 Err"
            );
            assert_eq!(session.committed_turns().len(), 4, "摘要失败不得驱逐任何轮");
            assert!(session.pinned_summary().is_none());
        }

        // 达到阈值：不再调用模型，机械摘要 + 强制驱逐
        let outcome = policy
            .maybe_compact(&mut session)
            .await
            .expect("降级路径必须返回 Ok")
            .expect("降级路径必须产出 outcome");

        assert!(outcome.degraded, "机械摘要必须标记为降级");
        assert!(outcome.evicted_turns > 0, "降级路径必须真的驱逐");
        assert_eq!(
            session.committed_turns().len(),
            4 - outcome.evicted_turns,
            "会话轮数必须实际减少"
        );
        assert!(outcome.summary.starts_with(SUMMARY_OPEN));
        assert!(outcome.summary.ends_with(SUMMARY_CLOSE));
        let body = strip_summary_wrapper(&outcome.summary);
        assert!(
            body.contains("[降级摘要]"),
            "降级摘要必须自带降级标注，否则无从知晓 pinned 已退化为转录碎片"
        );

        // 驱逐后 pinned 即该降级摘要
        let pinned = session.pinned_summary().unwrap();
        assert_eq!(
            match pinned.message.as_ref() {
                InputItem::Message { content, .. } => content.text_view().to_string(),
                _ => panic!("expected message"),
            },
            outcome.summary
        );
    }

    /// 脚本化摘要器：按序弹出 `true` = 失败 / `false` = 成功。
    struct ScriptedSummarizer {
        script: std::sync::Mutex<std::collections::VecDeque<bool>>,
    }

    #[async_trait]
    impl TurnSummarizer for ScriptedSummarizer {
        async fn summarize(
            &self,
            _previous: Option<&str>,
            _evicted: &str,
        ) -> Result<String, AgentError> {
            let fail = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .expect("script exhausted");
            if fail {
                Err(AgentError::Compaction("boom".to_string()))
            } else {
                Ok(wrap_summary("成功摘要"))
            }
        }

        async fn summarize_inflight(&self, _transcript: &str) -> Result<String, AgentError> {
            unimplemented!("compaction tests never compose turn epilogues")
        }
    }

    /// 成功路径清零计数：失败 → 成功 → 失败，第三次失败仍是普通 Err 而非降级。
    #[tokio::test]
    async fn test_success_resets_consecutive_failure_counter() {
        let policy = CompactionPolicy::new(
            1,
            0,
            Arc::new(ScriptedSummarizer {
                script: std::sync::Mutex::new([true, false, true].into_iter().collect()),
            }),
        );
        let mut session = make_session_with_turns(4);

        // 1. 失败
        assert!(policy.maybe_compact(&mut session).await.is_err());
        assert_eq!(session.committed_turns().len(), 4);

        // 2. 成功 —— 计数清零（补两轮以保证仍有可驱逐轮）
        push_turn(&mut session, 4);
        push_turn(&mut session, 5);
        let ok = policy.maybe_compact(&mut session).await.unwrap().unwrap();
        assert!(!ok.degraded);
        assert!(ok.evicted_turns > 0);

        // 3. 再失败一次：若计数没清零，这里会直接降级
        let remaining = session.committed_turns().len();
        push_turn(&mut session, 6);
        push_turn(&mut session, 7);
        assert!(
            policy.maybe_compact(&mut session).await.is_err(),
            "成功已清零计数，单次失败不得触发降级"
        );
        assert_eq!(
            session.committed_turns().len(),
            remaining + 2,
            "未达阈值不得驱逐"
        );
    }

    /// 降级摘要是**按字符**截断：转录含中文时按字节切会切坏 UTF-8（panic）。
    #[test]
    fn test_degraded_summary_truncates_on_char_boundary() {
        let transcript = "汉".repeat(DEGRADED_SUMMARY_MAX_CHARS * 2 + 7);
        let summary = degraded_summary(&transcript, 3);

        assert!(summary.starts_with(SUMMARY_OPEN));
        assert!(summary.ends_with(SUMMARY_CLOSE));
        let body = strip_summary_wrapper(&summary);
        assert!(body.contains("[降级摘要]"), "降级摘要须自带标注");
        assert!(body.contains("连续 3 次失败"), "标注须写明失败次数");

        // 正文末行即被截断的转录，长度恰为字符上限（而非字节数的一半）
        let head = body.rsplit('\n').next().unwrap();
        assert_eq!(head.chars().count(), DEGRADED_SUMMARY_MAX_CHARS);
        assert!(head.chars().all(|c| c == '汉'));
    }

    /// 端到端：多字节超长转录走降级路径，摘要长度受控且未切坏字符。
    #[tokio::test]
    async fn test_degraded_eviction_bounds_multibyte_transcript() {
        let policy = CompactionPolicy::new(1, 0, Arc::new(FailingSummarizer));
        let mut session = Session::new("test".to_string(), "test".to_string());
        for i in 0..4 {
            session
                .start_turn(format!("第{i}轮：{}", "汉".repeat(3_000)).into())
                .unwrap();
            let _ = session.commit_turn().unwrap();
        }

        // 失败路径不驱逐，转录在三次调用间不变 —— 先算出它，好逐字符比对
        let expected_transcript = build_transcript(&session.committed_turns()[..3]);
        let expected_head: String = expected_transcript
            .chars()
            .take(DEGRADED_SUMMARY_MAX_CHARS)
            .collect();
        assert!(expected_transcript.chars().count() > DEGRADED_SUMMARY_MAX_CHARS);

        for _ in 1..MAX_CONSECUTIVE_SUMMARY_FAILURES.min(THRESHOLD_LOOP_CAP) {
            assert!(policy.maybe_compact(&mut session).await.is_err());
        }
        let outcome = policy.maybe_compact(&mut session).await.unwrap().unwrap();
        assert!(outcome.degraded);

        let body = strip_summary_wrapper(&outcome.summary);
        let head = body.split_once('\n').expect("降级摘要须有标注行").1;
        // 逐字符相等即证明按字符边界截断：按字节切会 panic，丢弃式解码会出替换字符
        assert_eq!(head, expected_head, "降级摘要须为转录前 N 字符的精确前缀");
        assert_eq!(head.chars().count(), DEGRADED_SUMMARY_MAX_CHARS);
    }

    // ── apply_boundary（削减评估 → 折叠 → 结算）────────────────────────

    fn strip_policy(keep_recent_turns: usize, enforce: bool) -> StripPolicy {
        StripPolicy {
            keep_recent_turns,
            strip_reasoning: true,
            strip_tool_args: true,
            // 本组测试不涉工具输出截断 —— 输出截断由独立用例覆盖
            tool_output_max_chars: 0,
            enforce,
        }
    }

    /// 每轮 = 用户提问 + reasoning + 助手回答 —— 评估与两口径结算的共同夹具。
    fn make_session_with_reasoning_turns(n: usize) -> Session {
        let mut s = Session::new("test".to_string(), "test".to_string());
        for i in 0..n {
            s.start_turn(format!("这是第 {i} 轮的问题，内容足够长以产生 token 占用。").into())
                .unwrap();
            s.stage_item(
                MessageSource::ModelGeneration,
                InputItem::Reasoning {
                    content: format!("第 {i} 轮的推理，足够长以产生 token 占用。"),
                },
            )
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

    #[tokio::test]
    async fn test_apply_boundary_evaluates_strip_without_mutation() {
        let policy = CompactionPolicy::new(usize::MAX, 1000, Arc::new(MockSummarizer))
            .with_strip(strip_policy(1, false));
        let mut session = make_session_with_reasoning_turns(3);
        let before = serde_json::to_string(session.committed_turns()).unwrap();

        let r = policy.apply_boundary(&mut session).await.unwrap();

        let eval = r.strip_evaluation.expect("装配削减策略必须产出评估值");
        // scope = 3 − 1 = 2 轮，每轮恰 1 条 reasoning
        assert_eq!(eval.would_change_items, 2);
        assert!(eval.would_release_tokens > 0);
        assert_eq!(eval.skipped_turns, 0);

        assert!(r.compaction.is_none());
        assert!(!r.folded());
        assert!(!r.mutated(), "评估值不得触发落盘");
        assert!(r.strip.is_none(), "shadow 路径不产出生效值");
        assert!(!r.guard_triggered);

        // 口径与独立重算一致 — 字段漏算或重复计数在这里现形
        let full: usize = session
            .committed_turns()
            .iter()
            .flatten()
            .map(|am| estimate_item_tokens(&am.message))
            .sum();
        let view: usize = session
            .committed_turns()
            .iter()
            .flatten()
            .map(|am| estimate_viewable_item_tokens(&am.message))
            .sum();
        assert_eq!(r.committed_full_tokens, full);
        assert_eq!(r.committed_view_tokens, view);
        assert!(r.committed_view_tokens <= r.committed_full_tokens);
        assert_eq!(r.pinned_tokens, 0);
        assert_eq!(r.turn_index, 3);

        assert_eq!(
            serde_json::to_string(session.committed_turns()).unwrap(),
            before,
            "评估路径不得改写会话"
        );
    }

    #[tokio::test]
    async fn test_apply_boundary_records_prefold_caliber_when_folding() {
        // 阈值 80 必然触发折叠；口径在折叠前结算 ⇒ 记录的是触发所见
        let policy = CompactionPolicy::new(80, 40, Arc::new(MockSummarizer))
            .with_strip(strip_policy(1, false));
        let mut session = make_session_with_reasoning_turns(4);

        let r = policy.apply_boundary(&mut session).await.unwrap();

        let outcome = r.compaction.as_ref().expect("超阈值必须折叠");
        assert!(r.folded());
        assert!(r.mutated(), "折叠必须触发落盘");
        assert!(outcome.evicted_turns >= 1);
        assert!(
            r.committed_full_tokens > policy.trigger_tokens,
            "committed_full_tokens 必须是折叠前的触发对象"
        );
        assert_eq!(r.pinned_tokens, 0, "口径在折叠前结算，pinned 尚未写入");
        assert!(r.strip_evaluation.is_some(), "评估先于折叠，不受折叠影响");

        // 折叠后 committed 收缩 — 反证结算发生在折叠前
        let full_after: usize = session
            .committed_turns()
            .iter()
            .flatten()
            .map(|am| estimate_item_tokens(&am.message))
            .sum();
        assert!(full_after < r.committed_full_tokens);
    }

    #[tokio::test]
    async fn test_apply_boundary_without_strip_policy_skips_scan() {
        let policy = CompactionPolicy::new(usize::MAX, 1000, Arc::new(MockSummarizer));
        let mut session = make_session_with_reasoning_turns(2);

        let r = policy.apply_boundary(&mut session).await.unwrap();

        assert!(r.strip_evaluation.is_none(), "未装配削减策略不跑扫描");
        assert!(r.strip.is_none(), "未装配削减策略不产出生效值");
        assert!(!r.mutated());
        assert!(r.committed_full_tokens > 0);
        assert!(r.committed_view_tokens > 0);
    }

    /// enforce 路径：边界真的改写历史，生效值与评估值同源，二次边界幂等。
    #[tokio::test]
    async fn test_apply_boundary_enforce_strips_and_reports_outcome() {
        let policy = CompactionPolicy::new(usize::MAX, 1000, Arc::new(MockSummarizer))
            .with_strip(strip_policy(1, true));
        let mut session = make_session_with_reasoning_turns(3);
        let before = serde_json::to_string(session.committed_turns()).unwrap();
        let keep_window_before = serde_json::to_string(&session.committed_turns()[2..]).unwrap();

        let r = policy.apply_boundary(&mut session).await.unwrap();

        let strip = r.strip.expect("enforce 必须产出生效值");
        let eval = r.strip_evaluation.expect("enforce 同时产出评估值");
        // 同一份扫描：生效值与评估值逐字段一致
        assert_eq!(strip.changed_items, eval.would_change_items);
        assert_eq!(strip.released_tokens, eval.would_release_tokens);
        // scope = 3 − 1 = 2 轮，每轮恰 1 条 reasoning
        assert_eq!(strip.changed_items, 2);
        assert!(strip.released_tokens > 0);
        assert_eq!(strip.skipped_turns, 0);
        assert!(strip.mutated());
        assert!(r.mutated(), "生效改写必须触发落盘");

        // 会话真的被改写了，且保留窗（最近 1 轮）逐字节未动
        let committed = session.committed_turns();
        assert_ne!(
            serde_json::to_string(committed).unwrap(),
            before,
            "enforce 路径必须改写会话"
        );
        assert_eq!(
            serde_json::to_string(&committed[2..]).unwrap(),
            keep_window_before,
            "保留窗轮不受削减影响"
        );
        // scope 内 reasoning 已删（I3：轮次结构与 turn_index 不变，只删条目）
        assert!(
            committed[0]
                .iter()
                .all(|am| !matches!(am.message.as_ref(), InputItem::Reasoning { .. })),
            "scope 轮的 reasoning 必须被删除"
        );
        assert_eq!(committed.len(), 3, "I1：削减不驱逐轮次");

        // 幂等：第二次边界零改写
        let r2 = policy.apply_boundary(&mut session).await.unwrap();
        let strip2 = r2.strip.expect("enforce 路径始终产出生效值");
        assert_eq!(strip2.changed_items, 0, "幂等：二次边界零改写");
        assert!(!strip2.mutated());
        assert!(!r2.mutated(), "零改写 + 不折叠 ⇒ 不再落盘");
    }

    /// 无损不变式（分粒度）：
    /// (a) 改字段类 —— 存活条目逐条 `trunc2000(transcript_line)` 前后相等；
    /// (b) 删除类 —— 整轮 `build_transcript` 前后逐字节相等。
    ///
    /// 截断（`chars().take(TRANSCRIPT_ITEM_MAX_CHARS)`）在转录组装时发生，
    /// 两条断言都对截断后的文本比对 —— 清参的 FC 转录行本就不含 arguments；
    /// 长 tool 输出削到单条上限后行仍满 2000，逐条截断后同为原行前 2000 字符。
    #[test]
    fn test_strip_apply_is_transcript_lossless() {
        let mut s = make_session_with_mixed_turns(3);
        let before: Vec<Vec<crate::session::AnnotatedMessage>> = s.committed_turns().to_vec();

        let caps = StripCaps {
            strip_reasoning: true,
            strip_tool_args: true,
            tool_output_max_chars: TRANSCRIPT_ITEM_MAX_CHARS,
        };
        let eval = s.strip_stale_content(1, caps, true).unwrap();
        // scope 2 轮 × (1 条 reasoning + 1 条非空参数 FC + 1 条超上限输出)
        assert_eq!(eval.would_change_items, 6);
        assert!(eval.would_release_tokens > 0);

        let after = s.committed_turns();

        // (b) turn 级：删除类（reasoning）与改字段类（清参、截断到单条上限）都不改变轮级转录
        assert_eq!(
            build_transcript(&before[..2]),
            build_transcript(&after[..2]),
            "削减不得改变 scope 轮的转录"
        );
        assert_eq!(
            build_transcript(&before[2..]),
            build_transcript(&after[2..]),
            "保留窗轮的转录恒不变"
        );

        // (a) per-item：每个存活条目按 id 对齐，截断转录行前后相等
        let trunc =
            |line: &str| -> String { line.chars().take(TRANSCRIPT_ITEM_MAX_CHARS).collect() };
        let before_items: Vec<&crate::session::AnnotatedMessage> =
            before.iter().flatten().collect();
        let mut survivors = 0usize;
        for am in after.iter().flatten() {
            let orig = before_items
                .iter()
                .find(|b| b.id == am.id)
                .expect("存活条目必须能在削减前找到同 id 对应物");
            assert_eq!(
                trunc(&transcript_line(am).unwrap_or_default()),
                trunc(&transcript_line(orig).unwrap_or_default()),
                "item {} 的转录行经削减后必须一致",
                am.id.0
            );
            survivors += 1;
        }
        // 生效性检查 —— 否则恒等断言在「什么都没削」时也空转通过
        assert_eq!(
            survivors,
            before_items.len() - 2,
            "scope 内恰删 2 条 reasoning"
        );
        let fc_args = after[0].iter().find_map(|am| match am.message.as_ref() {
            InputItem::FunctionCall { arguments, .. } => Some(arguments.as_str()),
            _ => None,
        });
        assert_eq!(fc_args, Some(""), "FC 参数必须真的被清空");
        // 生效性：长 tool 输出真的被截到上限（否则恒等断言在「没截」时也空转通过）
        let out_len = after[0].iter().find_map(|am| match am.message.as_ref() {
            InputItem::FunctionCallOutput {
                output: model_provider::Content::Text(t),
                ..
            } => Some(t.chars().count()),
            _ => None,
        });
        assert_eq!(
            out_len,
            Some(TRANSCRIPT_ITEM_MAX_CHARS),
            "长 tool 输出必须真的被截到上限"
        );
    }

    /// tool 输出截断下界的来源：`"[tool output] "` 前缀 14 字符 ⇒ 削到
    /// S 字符后行仍满单条上限才保 per-item 等式 ⇒ S ≥ 1986。
    ///
    /// 1986 恒等、1985 不成立 —— 下界两侧的失败模式各由断言钉死：
    /// 低于下界时削后行变短，截断后取到的是整条削后行而非与原行相同的前缀片段。
    #[test]
    fn test_tool_output_strip_threshold_bounds_lossless_invariant() {
        let trunc =
            |line: &str| -> String { line.chars().take(TRANSCRIPT_ITEM_MAX_CHARS).collect() };
        let scope_output = |s: &Session| -> String {
            transcript_line(
                s.committed_turns()[0]
                    .iter()
                    .find(|am| matches!(am.message.as_ref(), InputItem::FunctionCallOutput { .. }))
                    .expect("夹具必须含工具输出"),
            )
            .expect("工具输出必有转录行")
        };

        for (cap, expect_lossless) in [
            (TRANSCRIPT_TOOL_OUTPUT_MIN_CAP, true),
            (TRANSCRIPT_TOOL_OUTPUT_MIN_CAP - 1, false),
        ] {
            let mut s = make_session_with_mixed_turns(2);
            let before_line = scope_output(&s);
            let eval = s
                .strip_stale_content(
                    1,
                    StripCaps {
                        strip_reasoning: false,
                        strip_tool_args: false,
                        tool_output_max_chars: cap,
                    },
                    true,
                )
                .unwrap();
            assert_eq!(eval.would_change_items, 1, "cap={cap} 时输出必须命中截断");
            let after_line = scope_output(&s);
            assert!(
                after_line.chars().count() < before_line.chars().count(),
                "cap={cap} 时截断必须真的发生"
            );
            assert_eq!(
                trunc(&after_line) == trunc(&before_line),
                expect_lossless,
                "cap={cap}（下界 {}）时 per-item 等式应为 {expect_lossless}",
                TRANSCRIPT_TOOL_OUTPUT_MIN_CAP
            );
        }
    }

    /// 含 reasoning / 长参数 FC / 长 tool 输出的多轮会话 —— 无损不变式夹具。
    fn make_session_with_mixed_turns(n: usize) -> Session {
        let mut s = Session::new("test".to_string(), "test".to_string());
        for i in 0..n {
            s.start_turn(format!("问题 {i}").into()).unwrap();
            s.stage_item(
                MessageSource::ModelGeneration,
                InputItem::Reasoning {
                    content: format!("推理 {i}"),
                },
            )
            .unwrap();
            s.stage_item(
                MessageSource::ModelGeneration,
                InputItem::FunctionCall {
                    call_id: format!("c{i}"),
                    name: "shell".to_string(),
                    arguments: format!(r#"{{"cmd":"{}"}}"#, "x".repeat(3000)),
                },
            )
            .unwrap();
            s.stage_item(
                MessageSource::ToolExecution {
                    tool_name: "shell".to_string(),
                },
                InputItem::FunctionCallOutput {
                    call_id: format!("c{i}"),
                    output: format!("out {}", "y".repeat(3000)).into(),
                },
            )
            .unwrap();
            s.stage_item(
                MessageSource::ModelGeneration,
                InputItem::Message {
                    role: Role::Assistant,
                    content: format!("回答 {i}").into(),
                },
            )
            .unwrap();
            let _ = s.commit_turn().unwrap();
        }
        s
    }
}
