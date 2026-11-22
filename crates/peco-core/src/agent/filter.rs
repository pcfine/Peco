// ============================================================================
// PecoContextFilter — Peco 永续对话的统一上下文组装器
// ============================================================================
//
// 单一截断点设计（参照 OpenHands 的 Condenser 抽象）：
// 全部「选哪些消息进入上下文」的决策集中于此过滤器，LooperConfig 的
// context_strategy 保持 FullHistory 直通，避免 filter + strategy 双层窗口
// 职责重叠。
//
// 组装规则：
//   1. 钉扎层：pinned 摘要（compaction 产物，Role::System）恒保留在最前；
//   2. Verbatim 层：历史轮按「校准 token 预算」从最新往回整轮保留，
//      轮内仅保留 User / Assistant 文本（丢弃 tool 过程与 reasoning）；
//   3. 当前轮：有请求预算时从旧到新削减工具输出 / 调用参数（最近几条保持
//      原样，配对关系不破坏）；无预算时完整保留（含 tool_call /
//      tool_result / reasoning）。
//
// 当前轮削减是「请求级护栏」：压缩只在轮次边界运行，单轮内工具输出暴涨
// 仍会把请求推过模型窗口。这里在每次请求组装时按 `窗口 - 输出预算 - 余量`
// 先裁掉最旧的工具证据，避免硬中断。

use tracing::warn;

use super::{MessageFilter, estimate_item_tokens, is_viewable_item};
use crate::session::{AnnotatedMessage, MessageSource};
use model_provider::{Content, InputItem};

/// 当前轮工具输出逐条截断上限（字符）。
const CURRENT_TURN_TOOL_OUTPUT_MAX_CHARS: usize = 4_000;
/// 当前轮保留原样的最近工具输出条数。
const CURRENT_TURN_KEEP_RECENT_TOOL_OUTPUTS: usize = 2;
/// 当前轮历史工具调用参数的截断上限（字符）。
const CURRENT_TURN_TOOL_ARGUMENTS_MAX_CHARS: usize = 2_000;
/// 工具输出被整条省略时的占位文本（保留 call_id 配对）。
const OMITTED_TOOL_OUTPUT: &str = "[older tool output omitted to fit context]";

/// Peco 永续对话专用上下文过滤器。
///
/// 相比早期的「10 条消息滑动窗口」，以 token 预算整轮选择历史，
/// 并与 compaction 的 pinned 摘要协同（摘要永不驱逐）。
pub struct PecoContextFilter {
    /// 历史轮 verbatim 保留区 token 预算（不含当前轮与 pinned 摘要）。
    history_token_budget: usize,
    /// 请求消息体总预算（pinned + 历史窗口 + 当前轮）。`None` = 不约束当前轮。
    request_token_budget: Option<usize>,
}

impl PecoContextFilter {
    /// 创建过滤器。
    ///
    /// * `history_token_budget` — 历史轮保留区 token 预算
    pub fn new(history_token_budget: usize) -> Self {
        Self {
            history_token_budget,
            request_token_budget: None,
        }
    }

    /// 设置请求消息体总预算：超过时从旧到新削减当前轮工具输出。
    ///
    /// 调用方按 `模型窗口 - 输出预算 - 系统提示/动态上下文余量` 计算；
    /// 不设置时当前轮完整保留，行为与引入护栏前一致。
    pub fn with_request_budget(mut self, request_token_budget: usize) -> Self {
        self.request_token_budget = Some(request_token_budget);
        self
    }
}

impl MessageFilter for PecoContextFilter {
    fn filter(&self, messages: &[&AnnotatedMessage]) -> Vec<AnnotatedMessage> {
        if messages.is_empty() {
            return vec![];
        }

        // ── 1. 切分：pinned 摘要 / 历史轮 / 当前轮 ─────────────────
        // 当前轮 = 最后一条消息的 turn_index
        let current_turn = messages.last().unwrap().turn_index;

        let mut pinned: Vec<AnnotatedMessage> = Vec::new();
        // (turn_index, Vec<克隆消息>)
        let mut history_turns: Vec<(usize, Vec<AnnotatedMessage>)> = Vec::new();
        let mut current: Vec<AnnotatedMessage> = Vec::new();

        for m in messages.iter().map(|m| (*m).clone()) {
            // pinned 摘要：按 MessageSource 判定（compaction 的 SystemInjection 标记），
            // 不按 Role 判定 —— 避免未来其他 System/Developer 消息被错误提升到
            // 顶部并绕过预算驱逐。SystemInjection 目前仅有 compaction 一个生产者。
            if matches!(m.source, MessageSource::SystemInjection { .. }) {
                pinned.push(m);
            } else if m.turn_index >= current_turn {
                current.push(m);
            } else {
                match history_turns.last_mut() {
                    Some((turn, msgs)) if *turn == m.turn_index => msgs.push(m),
                    _ => history_turns.push((m.turn_index, vec![m])),
                }
            }
        }

        // ── 2. 历史轮整轮选择：从最新往回，校准 token 预算 ──────────
        // 选择算法与可观测性指标共用（见 select_history_turns），
        // 保证「指标显示的 Verbatim 占用 = 模型实际收到的上下文」。
        let history_msgs: Vec<Vec<AnnotatedMessage>> =
            history_turns.into_iter().map(|(_, msgs)| msgs).collect();
        let (selected, history_tokens) =
            select_history_turns(&history_msgs, self.history_token_budget);

        // ── 3. 当前轮护栏：超请求预算时从旧到新削减工具证据 ────────
        if let Some(budget) = self.request_token_budget {
            let pinned_tokens: usize = pinned
                .iter()
                .map(|am| estimate_item_tokens(&am.message))
                .sum();
            let current_tokens: usize = current
                .iter()
                .map(|am| estimate_item_tokens(&am.message))
                .sum();
            let total = pinned_tokens
                .saturating_add(history_tokens)
                .saturating_add(current_tokens);
            if total > budget {
                trim_current_turn(&mut current, total - budget, budget, total);
            }
        }

        // ── 4. 组装：pinned 摘要 + 历史（旧→新）+ 当前轮 ────────────
        let mut result = Vec::new();
        result.extend(pinned);
        for i in selected {
            // 轮内 view：仅 User / Assistant 文本（丢弃 tool 过程与 reasoning）
            result.extend(
                history_msgs[i]
                    .iter()
                    .filter(|am| is_viewable_item(&am.message))
                    .cloned(),
            );
        }
        result.extend(current);
        result
    }
}

/// 当前轮削减结果 — 供测试与日志断言。
#[derive(Debug, Default, PartialEq, Eq)]
struct CurrentTurnTrim {
    truncated_outputs: usize,
    omitted_outputs: usize,
    truncated_arguments: usize,
    released_tokens: usize,
    residual_excess: usize,
}

/// 从旧到新削减当前轮，直到估算总量回落到请求预算内。
///
/// 三遍递进，所有改动都保留 `FunctionCall` / `FunctionCallOutput` 的
/// `call_id` 配对（wire 格式要求）：先截断工具输出正文，再把仍超限的旧
/// 输出整条换成占位符，最后截断旧工具调用参数。最近
/// [`CURRENT_TURN_KEEP_RECENT_TOOL_OUTPUTS`] 条输出保持原样 —— 模型正在
/// 依赖的证据不过度破坏。
fn trim_current_turn(
    current: &mut [AnnotatedMessage],
    excess: usize,
    budget: usize,
    estimated_before: usize,
) -> CurrentTurnTrim {
    let mut report = CurrentTurnTrim::default();
    let mut remaining = excess;

    let output_indices: Vec<usize> = current
        .iter()
        .enumerate()
        .filter(|(_, am)| matches!(am.message.as_ref(), InputItem::FunctionCallOutput { .. }))
        .map(|(i, _)| i)
        .collect();
    let keep_from = output_indices
        .len()
        .saturating_sub(CURRENT_TURN_KEEP_RECENT_TOOL_OUTPUTS);
    let output_candidates = &output_indices[..keep_from];

    // 第一遍：逐条截断输出正文（保留头尾），旧 → 新。
    for &i in output_candidates {
        if remaining == 0 {
            break;
        }
        let before = estimate_item_tokens(&current[i].message);
        let Some(replacement) = shrink_output(&current[i].message) else {
            continue;
        };
        current[i].message = std::sync::Arc::new(replacement);
        current[i].estimated_tokens = None;
        let after = estimate_item_tokens(&current[i].message);
        let released = before.saturating_sub(after);
        remaining = remaining.saturating_sub(released);
        report.released_tokens += released;
        if released > 0 {
            report.truncated_outputs += 1;
        }
    }

    // 第二遍：仍超限 → 旧输出整条换成占位符（call_id 保留）。
    for &i in output_candidates {
        if remaining == 0 {
            break;
        }
        let before = estimate_item_tokens(&current[i].message);
        let Some(replacement) = omit_output(&current[i].message) else {
            continue;
        };
        current[i].message = std::sync::Arc::new(replacement);
        current[i].estimated_tokens = None;
        let after = estimate_item_tokens(&current[i].message);
        let released = before.saturating_sub(after);
        remaining = remaining.saturating_sub(released);
        report.released_tokens += released;
        if released > 0 {
            report.omitted_outputs += 1;
        }
    }

    // 第三遍：旧工具调用的参数（写文件、传大 JSON 等）截断。
    let call_indices: Vec<usize> = current
        .iter()
        .enumerate()
        .filter(|(_, am)| matches!(am.message.as_ref(), InputItem::FunctionCall { .. }))
        .map(|(i, _)| i)
        .collect();
    let keep_from = call_indices
        .len()
        .saturating_sub(CURRENT_TURN_KEEP_RECENT_TOOL_OUTPUTS);
    for &i in &call_indices[..keep_from] {
        if remaining == 0 {
            break;
        }
        let before = estimate_item_tokens(&current[i].message);
        let Some(replacement) = shrink_arguments(&current[i].message) else {
            continue;
        };
        current[i].message = std::sync::Arc::new(replacement);
        current[i].estimated_tokens = None;
        let after = estimate_item_tokens(&current[i].message);
        let released = before.saturating_sub(after);
        remaining = remaining.saturating_sub(released);
        report.released_tokens += released;
        if released > 0 {
            report.truncated_arguments += 1;
        }
    }

    report.residual_excess = remaining;
    if remaining > 0 {
        warn!(
            budget,
            estimated_before,
            residual_excess = remaining,
            truncated_outputs = report.truncated_outputs,
            omitted_outputs = report.omitted_outputs,
            truncated_arguments = report.truncated_arguments,
            "current turn still over request budget after trim; cannot guarantee fit"
        );
    } else if report.released_tokens > 0 {
        warn!(
            budget,
            estimated_before,
            released_tokens = report.released_tokens,
            truncated_outputs = report.truncated_outputs,
            omitted_outputs = report.omitted_outputs,
            truncated_arguments = report.truncated_arguments,
            "current turn trimmed to fit request budget"
        );
    }
    report
}

/// 把工具输出正文截到 [`CURRENT_TURN_TOOL_OUTPUT_MAX_CHARS`] 字符，保留头尾。
///
/// 图片部件在文本视图里本就不计 token；折叠为纯文本顺带丢弃它们（DeepSeek
/// 等不支持图片输入的 provider 本来也会剥离）。
fn shrink_output(item: &InputItem) -> Option<InputItem> {
    let InputItem::FunctionCallOutput { call_id, output } = item else {
        return None;
    };
    let text = output.text_view();
    let total_chars = text.chars().count();
    if total_chars <= CURRENT_TURN_TOOL_OUTPUT_MAX_CHARS {
        return None;
    }
    let head_len = CURRENT_TURN_TOOL_OUTPUT_MAX_CHARS * 3 / 4;
    let tail_len = CURRENT_TURN_TOOL_OUTPUT_MAX_CHARS - head_len;
    let head: String = text.chars().take(head_len).collect();
    let tail: String = text.chars().skip(total_chars - tail_len).collect();
    Some(InputItem::FunctionCallOutput {
        call_id: call_id.clone(),
        output: Content::Text(format!("{head}\n…[truncated to fit context]…\n{tail}")),
    })
}

/// 用占位文本替换工具输出；`call_id` 保留，wire 配对不破坏。
fn omit_output(item: &InputItem) -> Option<InputItem> {
    let InputItem::FunctionCallOutput { call_id, .. } = item else {
        return None;
    };
    Some(InputItem::FunctionCallOutput {
        call_id: call_id.clone(),
        output: Content::Text(OMITTED_TOOL_OUTPUT.to_string()),
    })
}

/// 截断工具调用参数（保留 `call_id` / 工具名）。
fn shrink_arguments(item: &InputItem) -> Option<InputItem> {
    let InputItem::FunctionCall {
        call_id,
        name,
        arguments,
    } = item
    else {
        return None;
    };
    if arguments.chars().count() <= CURRENT_TURN_TOOL_ARGUMENTS_MAX_CHARS {
        return None;
    }
    let truncated: String = arguments
        .chars()
        .take(CURRENT_TURN_TOOL_ARGUMENTS_MAX_CHARS)
        .collect();
    Some(InputItem::FunctionCall {
        call_id: call_id.clone(),
        name: name.clone(),
        arguments: format!("{truncated}…[truncated to fit context]"),
    })
}

/// Verbatim 层历史轮选择 — [`PecoContextFilter`] 与可观测性指标
/// （宿主侧的边界观测）共用的唯一实现，防两处口径漂移。
///
/// 从最新轮往回整轮计入 viewable token，严格预算：超预算的历史轮一律裁掉，
/// 且更早轮不再尝试（连续窗口）— 旧信息由 pinned 摘要承载，
/// 不做「至少保留一轮」的降级例外。
///
/// * `history_turns` — 历史轮（旧→新），每轮为该轮全部消息
///
/// 返回 `(选中轮下标（旧→新），选中轮 viewable token 总和)`。
pub fn select_history_turns(
    history_turns: &[Vec<AnnotatedMessage>],
    budget: usize,
) -> (Vec<usize>, usize) {
    let mut selected: Vec<usize> = Vec::new();
    let mut remaining = budget;
    for (i, turn_msgs) in history_turns.iter().enumerate().rev() {
        let view_tokens: usize = turn_msgs
            .iter()
            .filter(|am| is_viewable_item(&am.message))
            .map(|am| estimate_item_tokens(&am.message))
            .sum();
        if view_tokens > remaining {
            break;
        }
        remaining -= view_tokens;
        selected.push(i);
    }
    selected.reverse();
    (selected, budget - remaining)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::estimate_str_tokens;
    use crate::session::MessageId;
    use model_provider::{Content, ContentPart, InputItem, Role};

    fn user(text: impl Into<Content>) -> InputItem {
        InputItem::Message {
            role: Role::User,
            content: text.into(),
        }
    }
    fn assistant(text: impl Into<Content>) -> InputItem {
        InputItem::Message {
            role: Role::Assistant,
            content: text.into(),
        }
    }
    fn system(text: impl Into<Content>) -> InputItem {
        InputItem::Message {
            role: Role::System,
            content: text.into(),
        }
    }
    fn function_call(call_id: &str, name: &str) -> InputItem {
        InputItem::FunctionCall {
            call_id: call_id.into(),
            name: name.into(),
            arguments: "ls".into(),
        }
    }
    fn function_call_output(call_id: &str) -> InputItem {
        InputItem::FunctionCallOutput {
            call_id: call_id.into(),
            output: "out".into(),
        }
    }

    fn make_annotated(turn: usize, msg: InputItem) -> AnnotatedMessage {
        AnnotatedMessage::new(MessageId(0), turn, msg, MessageSource::UserInput)
    }

    fn make_pinned(msg: InputItem) -> AnnotatedMessage {
        AnnotatedMessage::new(
            MessageId(0),
            0,
            msg,
            MessageSource::SystemInjection {
                reason: "compaction".to_string(),
            },
        )
    }

    #[test]
    fn test_empty_messages() {
        let filter = PecoContextFilter::new(10_000);
        let refs: Vec<&AnnotatedMessage> = vec![];
        assert!(filter.filter(&refs).is_empty());
    }

    #[test]
    fn test_pinned_summary_always_kept_first() {
        let filter = PecoContextFilter::new(1); // 预算为 0 也不驱逐摘要
        let msgs = [
            make_pinned(system(
                "<earlier_context_summary>摘要</earlier_context_summary>",
            )),
            make_annotated(0, user("旧问题")),
            make_annotated(0, assistant("旧回答")),
            make_annotated(1, user("新问题")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);
        // 历史 0 轮因预算被裁掉，但 pinned 摘要 + 当前轮仍在
        assert_eq!(result.len(), 2);
        assert!(matches!(
            result[0].message.as_ref(),
            InputItem::Message {
                role: Role::System,
                ..
            }
        ));
        assert!(matches!(
            result[1].message.as_ref(),
            InputItem::Message {
                role: Role::User,
                ..
            }
        ));
    }

    #[test]
    fn test_non_injection_system_message_not_pinned() {
        // Role::System 但来源不是 SystemInjection 的消息不得被提升为 pinned —
        // pinned 判定依据 MessageSource，而非 Role。
        let filter = PecoContextFilter::new(10_000);
        let msgs = [
            make_annotated(0, system("普通 system 消息")),
            make_annotated(1, user("问题")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);
        // 历史 turn 0 的 system 消息不可见（仅 User/Assistant）→ 丢弃；仅剩当前轮
        assert_eq!(result.len(), 1);
        assert!(matches!(
            result[0].message.as_ref(),
            InputItem::Message {
                role: Role::User,
                ..
            }
        ));
    }

    #[test]
    fn test_current_turn_keeps_full_tool_context() {
        let filter = PecoContextFilter::new(10_000);
        let msgs = [
            make_annotated(0, user("旧问题")),
            make_annotated(0, assistant("旧回答")),
            make_annotated(1, user("新问题")),
            make_annotated(1, assistant("run")),
            make_annotated(1, function_call("c1", "shell")),
            make_annotated(1, function_call_output("c1")),
            make_annotated(1, assistant("done")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);
        // 历史: User + Asst(text) = 2, 当前: 5
        assert_eq!(result.len(), 7);
    }

    #[test]
    fn test_history_budget_evicts_oldest_turns_whole() {
        let filter = PecoContextFilter::new(25);
        // 每轮 2 条中文消息 ≈ 20 token（校准估算：15 CJK × 0.6 + 3 ASCII × 0.3 ≈ 10/条）；
        // 预算 25 → 历史只保留最近 1 轮，更早轮整轮裁掉
        let msgs: Vec<AnnotatedMessage> = (0..3)
            .flat_map(|i| {
                vec![
                    make_annotated(i, user(format!("这是第 {i} 轮的提问内容，长度足够。"))),
                    make_annotated(i, assistant(format!("这是第 {i} 轮的回答内容，同样足够。"))),
                ]
            })
            .collect();
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);
        // turn 1（历史，20 ≤ 25）保留；turn 0（20 > 剩余 5）整轮裁掉；
        // turn 2 为当前轮完整保留 → 共 4 条
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].turn_index, 1);
        assert_eq!(result[2].turn_index, 2);
    }

    #[test]
    fn test_history_drops_tool_items_but_keeps_text() {
        let filter = PecoContextFilter::new(10_000);
        let msgs = [
            make_annotated(0, user("查天气")),
            make_annotated(0, assistant("让我看看")),
            make_annotated(0, function_call("c1", "fetch")),
            make_annotated(0, function_call_output("c1")),
            make_annotated(0, assistant("今天晴天")),
            make_annotated(1, user("谢谢")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);
        // 历史 turn 0 仅 User + 2 条 Assistant 文本，当前轮 1 条
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn test_calibrated_estimator_weights_cjk() {
        // 10 个汉字 ≈ 6 token（0.6/字），10 个 ASCII ≈ 3 token（0.3/字）
        assert_eq!(estimate_str_tokens("一二三四五六七八九十"), 6);
        assert_eq!(estimate_str_tokens("abcdefghij"), 3);
        // 混合：ceil(6.0) + ceil(0.9) = 6 + 1 = 7
        assert_eq!(estimate_str_tokens("一二三四五六七八九十abc"), 7);
    }

    fn user_with_image() -> InputItem {
        InputItem::Message {
            role: Role::User,
            content: Content::Parts(vec![
                ContentPart::Text {
                    text: "看看这张图".to_string(),
                },
                ContentPart::Image {
                    url: "data:image/png;base64,QUJD".to_string(),
                    detail: None,
                },
            ]),
        }
    }

    #[test]
    fn test_history_turn_with_image_keeps_parts_intact() {
        // 含图历史轮按整轮规则参与预算；轮内含图用户消息的部件随整轮保留。
        let filter = PecoContextFilter::new(10_000);
        let msgs = [
            make_annotated(0, user_with_image()),
            make_annotated(0, assistant("图中是一只猫")),
            make_annotated(1, user("谢谢")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);
        // 历史 turn 0（User + Assistant 文本）+ 当前轮 1 条
        assert_eq!(result.len(), 3);
        match result[0].message.as_ref() {
            InputItem::Message {
                role: Role::User,
                content: Content::Parts(parts),
            } => {
                assert_eq!(parts.len(), 2);
                assert!(parts.iter().any(
                    |p| matches!(p, ContentPart::Image { url, .. } if url.starts_with("data:image/png"))
                ));
            }
            other => panic!("含图用户消息应原样保留，得到 {other:?}"),
        }
    }

    #[test]
    fn test_image_turn_evicted_by_budget_like_any_turn() {
        // 图片不计入 token 估算，含图轮次按 text 口径与其他轮次同样
        // 参与预算 —— 超预算即整轮裁掉，无特殊豁免。
        // turn 0 文本 ≈ 3 + 4 = 7 token，预算 5 不足以容纳。
        let filter = PecoContextFilter::new(5);
        let msgs = [
            make_annotated(0, user_with_image()),
            make_annotated(0, assistant("图中是一只猫")),
            make_annotated(1, user("谢谢")),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);
        // turn 0（约 7 token）被预算 5 整轮裁掉，仅剩当前轮
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].turn_index, 1);
    }

    fn large_output(call_id: &str, chars: usize) -> InputItem {
        InputItem::FunctionCallOutput {
            call_id: call_id.into(),
            output: Content::Text("x".repeat(chars)),
        }
    }

    fn output_texts(result: &[AnnotatedMessage]) -> Vec<&str> {
        result
            .iter()
            .filter_map(|am| match am.message.as_ref() {
                InputItem::FunctionCallOutput {
                    output: Content::Text(text),
                    ..
                } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn estimated_total(result: &[AnnotatedMessage]) -> usize {
        result
            .iter()
            .map(|am| estimate_item_tokens(&am.message))
            .sum()
    }

    #[test]
    fn test_request_budget_truncates_oldest_current_turn_output() {
        // 3 条 30k ASCII 输出 ≈ 9000 token/条；预算 22000 只够削最旧那条
        // （最近两条保持原样）。削减后必须回到预算内，且 3 条结果配对都在。
        let filter = PecoContextFilter::new(10_000).with_request_budget(22_000);
        let msgs = [
            make_annotated(1, user("跑一下")),
            make_annotated(1, function_call("c1", "shell")),
            make_annotated(1, large_output("c1", 30_000)),
            make_annotated(1, function_call("c2", "shell")),
            make_annotated(1, large_output("c2", 30_000)),
            make_annotated(1, function_call("c3", "shell")),
            make_annotated(1, large_output("c3", 30_000)),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);

        assert!(
            estimated_total(&result) <= 22_000,
            "request must fit the budget, got {}",
            estimated_total(&result)
        );
        let outputs = output_texts(&result);
        assert_eq!(outputs.len(), 3, "tool-result pairing must be preserved");
        assert!(
            outputs[0].contains("[truncated to fit context]"),
            "oldest output should be truncated, got {} chars",
            outputs[0].chars().count()
        );
        assert_eq!(outputs[1].chars().count(), 30_000, "recent output intact");
        assert_eq!(outputs[2].chars().count(), 30_000, "newest output intact");
    }

    #[test]
    fn test_request_budget_omits_output_when_truncation_insufficient() {
        // 4 条 30k 输出（≈ 9000 token/条）；预算 20000 时先截断两条候选，
        // 仍不够再把最旧那条换成占位符。最近两条保持原样。
        let filter = PecoContextFilter::new(10_000).with_request_budget(20_000);
        let msgs = [
            make_annotated(1, user("跑一下")),
            make_annotated(1, large_output("c1", 30_000)),
            make_annotated(1, large_output("c2", 30_000)),
            make_annotated(1, large_output("c3", 30_000)),
            make_annotated(1, large_output("c4", 30_000)),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);

        assert!(
            estimated_total(&result) <= 20_000,
            "request must fit the budget, got {}",
            estimated_total(&result)
        );
        let outputs = output_texts(&result);
        assert_eq!(outputs.len(), 4, "tool-result pairing must be preserved");
        assert_eq!(
            outputs[0], OMITTED_TOOL_OUTPUT,
            "oldest output should be omitted"
        );
        assert!(
            outputs[1].contains("[truncated to fit context]"),
            "second oldest output should be truncated"
        );
        assert_eq!(outputs[2].chars().count(), 30_000, "recent output intact");
        assert_eq!(outputs[3].chars().count(), 30_000, "newest output intact");
    }

    #[test]
    fn test_request_budget_trims_old_tool_arguments() {
        // 输出不大时，护栏最后才动旧工具调用参数；最近两次调用保持原样。
        let filter = PecoContextFilter::new(10_000).with_request_budget(5_000);
        let big_args = "y".repeat(30_000);
        let call = |id: &str, args: &str| InputItem::FunctionCall {
            call_id: id.into(),
            name: "shell".into(),
            arguments: args.into(),
        };
        let msgs = [
            make_annotated(1, user("跑一下")),
            make_annotated(1, call("c1", &big_args)),
            make_annotated(1, call("c2", &big_args)),
            make_annotated(1, call("c3", &big_args)),
        ];
        let refs: Vec<&AnnotatedMessage> = msgs.iter().collect();
        let result = filter.filter(&refs);

        let args: Vec<&str> = result
            .iter()
            .filter_map(|am| match am.message.as_ref() {
                InputItem::FunctionCall { arguments, .. } => Some(arguments.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(args.len(), 3, "tool-call pairing structure preserved");
        assert!(
            args[0].contains("[truncated to fit context]"),
            "oldest call arguments should be truncated"
        );
        assert_eq!(args[1], big_args, "recent call arguments intact");
        assert_eq!(args[2], big_args, "newest call arguments intact");
    }
}
