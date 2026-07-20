// ============================================================================
// 分层消息缓冲区
// ============================================================================
//
// NOTE: Some methods on CommittedBuffer and StagingBuffer are public API used
// by peco-server or planned for future use.

#![allow(dead_code)]
//
// - CommittedBuffer: 已确认的 turn 历史（不可变）
// - StagingBuffer: 当前 turn 的进行中消息（可 rollback）
//
// Session 直接组合这两个结构体，不再通过 trait 抽象。

use super::types::AnnotatedMessage;

// ============================================================================
// CommittedBuffer
// ============================================================================

/// 已确认的 turn 历史。
///
/// 每条 turn 是一个 `Vec<AnnotatedMessage>`，以 User 消息开头。
/// 一旦 commit 就不可变（除非 rollback_to_turn 截断）。
#[derive(Debug, Clone)]
pub(crate) struct CommittedBuffer {
    turns: Vec<Vec<AnnotatedMessage>>,
}

impl CommittedBuffer {
    /// 创建空的 committed buffer。
    pub fn new() -> Self {
        Self { turns: Vec::new() }
    }

    /// 从已有 turns 列表重建（用于 from_snapshot）。
    pub fn from_turns(turns: Vec<Vec<AnnotatedMessage>>) -> Self {
        Self { turns }
    }

    /// 追加一轮已完成的消息。
    pub fn push_turn(&mut self, messages: Vec<AnnotatedMessage>) {
        self.turns.push(messages);
    }

    /// 已完成 turn 的数量。
    pub fn len(&self) -> usize {
        self.turns.len()
    }

    /// 所有 completed turn 的消息总数。
    pub fn message_count(&self) -> usize {
        self.turns.iter().map(|t| t.len()).sum()
    }

    /// 最后一轮的索引。若无已完成 turn 则返回 0。
    pub fn last_turn_index(&self) -> usize {
        self.turns.len().saturating_sub(1)
    }

    /// 所有 turn 的切片引用。
    pub fn turns(&self) -> &[Vec<AnnotatedMessage>] {
        &self.turns
    }

    /// 遍历所有已完成 turn 的消息（按顺序）。
    pub fn iter_all(&self) -> impl Iterator<Item = &AnnotatedMessage> {
        self.turns.iter().flat_map(|turn| turn.iter())
    }

    /// 从指定 turn 开始遍历（含）。
    pub fn iter_from(&self, turn_index: usize) -> impl Iterator<Item = &AnnotatedMessage> {
        self.turns
            .iter()
            .skip(turn_index)
            .flat_map(|turn| turn.iter())
    }

    /// 回滚到指定 turn（保留 turn_index 之前的所有 turn）。
    ///
    /// 返回被删除的 turn 数量。
    pub fn truncate_to(&mut self, turn_index: usize) -> usize {
        if turn_index >= self.turns.len() {
            return 0;
        }
        let removed = self.turns.len() - turn_index;
        self.turns.truncate(turn_index);
        removed
    }

    /// 获取指定 turn 的引用。
    pub fn get_turn(&self, turn_index: usize) -> Option<&[AnnotatedMessage]> {
        self.turns.get(turn_index).map(|t| t.as_slice())
    }
}

// ============================================================================
// StagingBuffer
// ============================================================================

/// 当前 turn 的进行中消息。
///
/// 在 turn 完成前可随时丢弃（rollback）。commit 后消息移入 CommittedBuffer。
#[derive(Debug, Clone)]
pub(crate) struct StagingBuffer {
    /// 当前 turn 的用户输入消息
    user_input: Option<AnnotatedMessage>,
    /// 当前 turn 的后续消息（assistant、tool 结果等）
    messages: Vec<AnnotatedMessage>,
}

impl StagingBuffer {
    /// 创建空的 staging buffer。
    pub fn new() -> Self {
        Self {
            user_input: None,
            messages: Vec::new(),
        }
    }

    /// 设置本轮用户输入。
    pub fn set_user_input(&mut self, msg: AnnotatedMessage) {
        self.user_input = Some(msg);
    }

    /// 追加一条消息到 staging。
    pub fn push(&mut self, msg: AnnotatedMessage) {
        self.messages.push(msg);
    }

    /// staging 中是否有消息。
    pub fn is_empty(&self) -> bool {
        self.user_input.is_none() && self.messages.is_empty()
    }

    /// staging 中的消息总数（含 user_input）。
    pub fn len(&self) -> usize {
        self.messages.len() + self.user_input.iter().count()
    }

    /// 本轮是否已有完整回复（存在无 tool_calls 的 assistant 消息）。
    pub fn is_complete(&self) -> bool {
        self.messages.iter().any(|am| am.is_final_response())
    }

    /// 获取所有 staging 消息的引用（不含 user_input）。
    pub fn messages_ref(&self) -> &[AnnotatedMessage] {
        &self.messages
    }

    /// 获取 user_input 的引用。
    pub fn user_input_ref(&self) -> Option<&AnnotatedMessage> {
        self.user_input.as_ref()
    }

    /// 按 user_input → messages 顺序遍历所有 staging 消息的引用。
    pub fn iter_all(&self) -> impl Iterator<Item = &AnnotatedMessage> {
        self.user_input.iter().chain(self.messages.iter())
    }

    /// 取出所有 staging 消息（含 user_input），清空 staging。
    ///
    /// user_input 排在 messages 之前。
    pub fn take_all(&mut self) -> Vec<AnnotatedMessage> {
        let mut all = Vec::with_capacity(self.len());
        if let Some(ui) = self.user_input.take() {
            all.push(ui);
        }
        all.append(&mut self.messages);
        all
    }

    /// 取出 user_input（用于 requeue 到 pending）。
    pub fn take_user_input(&mut self) -> Option<AnnotatedMessage> {
        self.user_input.take()
    }

    /// 清空 staging（丢弃所有消息）。
    pub fn clear(&mut self) {
        self.user_input = None;
        self.messages.clear();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::types::{MessageId, MessageSource};
    use model_provider::Message;

    // ── CommittedBuffer tests ──────────────────────────────────────────

    #[test]
    fn test_committed_new_is_empty() {
        let cb = CommittedBuffer::new();
        assert_eq!(cb.len(), 0);
        assert_eq!(cb.message_count(), 0);
        assert!(cb.turns().is_empty());
    }

    #[test]
    fn test_committed_push_and_iter() {
        let mut cb = CommittedBuffer::new();
        let am = AnnotatedMessage::new(
            MessageId(0),
            0,
            Message::user("hello"),
            MessageSource::UserInput,
        );
        cb.push_turn(vec![am.clone()]);
        assert_eq!(cb.len(), 1);
        assert_eq!(cb.message_count(), 1);
        let all: Vec<_> = cb.iter_all().collect();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, MessageId(0));
    }

    #[test]
    fn test_committed_iter_from() {
        let mut cb = CommittedBuffer::new();
        let am0 = AnnotatedMessage::new(
            MessageId(0),
            0,
            Message::user("q0"),
            MessageSource::UserInput,
        );
        let am1 = AnnotatedMessage::new(
            MessageId(1),
            1,
            Message::user("q1"),
            MessageSource::UserInput,
        );
        cb.push_turn(vec![am0]);
        cb.push_turn(vec![am1]);
        assert_eq!(cb.iter_from(0).count(), 2);
        assert_eq!(cb.iter_from(1).count(), 1);
        assert_eq!(cb.iter_from(2).count(), 0);
    }

    #[test]
    fn test_committed_truncate_to() {
        let mut cb = CommittedBuffer::new();
        for i in 0..3 {
            let am = AnnotatedMessage::new(
                MessageId(i),
                i as usize,
                Message::user(&format!("q{i}")),
                MessageSource::UserInput,
            );
            cb.push_turn(vec![am]);
        }
        assert_eq!(cb.len(), 3);
        let removed = cb.truncate_to(1);
        assert_eq!(removed, 2);
        assert_eq!(cb.len(), 1);
    }

    #[test]
    fn test_committed_get_turn() {
        let mut cb = CommittedBuffer::new();
        let am = AnnotatedMessage::new(
            MessageId(0),
            0,
            Message::user("q0"),
            MessageSource::UserInput,
        );
        cb.push_turn(vec![am]);
        assert!(cb.get_turn(0).is_some());
        assert!(cb.get_turn(1).is_none());
    }

    #[test]
    fn test_committed_from_turns() {
        let turns = vec![vec![AnnotatedMessage::new(
            MessageId(0),
            0,
            Message::user("q0"),
            MessageSource::UserInput,
        )]];
        let cb = CommittedBuffer::from_turns(turns);
        assert_eq!(cb.len(), 1);
    }

    // ── StagingBuffer tests ───────────────────────────────────────────

    #[test]
    fn test_staging_new_is_empty() {
        let sb = StagingBuffer::new();
        assert!(sb.is_empty());
        assert_eq!(sb.len(), 0);
        assert!(sb.user_input_ref().is_none());
        assert!(sb.messages_ref().is_empty());
    }

    #[test]
    fn test_staging_set_and_iter() {
        let mut sb = StagingBuffer::new();
        let ui = AnnotatedMessage::new(
            MessageId(0),
            0,
            Message::user("hi"),
            MessageSource::UserInput,
        );
        sb.set_user_input(ui);
        let msg = AnnotatedMessage::new(
            MessageId(1),
            0,
            Message::assistant("hello"),
            MessageSource::ModelGeneration,
        );
        sb.push(msg);

        assert!(!sb.is_empty());
        assert_eq!(sb.len(), 2);

        let all: Vec<_> = sb.iter_all().collect();
        assert_eq!(all.len(), 2);
        // user_input comes first
        assert!(matches!(all[0].message.as_ref(), Message::User { .. }));
        assert!(matches!(all[1].message.as_ref(), Message::Assistant { .. }));
    }

    #[test]
    fn test_staging_take_all_clears() {
        let mut sb = StagingBuffer::new();
        sb.set_user_input(AnnotatedMessage::new(
            MessageId(0),
            0,
            Message::user("q"),
            MessageSource::UserInput,
        ));
        sb.push(AnnotatedMessage::new(
            MessageId(1),
            0,
            Message::assistant("a"),
            MessageSource::ModelGeneration,
        ));

        let taken = sb.take_all();
        assert_eq!(taken.len(), 2);
        assert!(sb.is_empty());
    }

    #[test]
    fn test_staging_take_user_input() {
        let mut sb = StagingBuffer::new();
        sb.set_user_input(AnnotatedMessage::new(
            MessageId(0),
            0,
            Message::user("q"),
            MessageSource::UserInput,
        ));
        let ui = sb.take_user_input();
        assert!(ui.is_some());
        assert!(sb.user_input_ref().is_none());
    }

    #[test]
    fn test_staging_clear() {
        let mut sb = StagingBuffer::new();
        sb.set_user_input(AnnotatedMessage::new(
            MessageId(0),
            0,
            Message::user("q"),
            MessageSource::UserInput,
        ));
        sb.push(AnnotatedMessage::new(
            MessageId(1),
            0,
            Message::assistant("a"),
            MessageSource::ModelGeneration,
        ));
        sb.clear();
        assert!(sb.is_empty());
    }

    #[test]
    fn test_staging_is_complete() {
        let mut sb = StagingBuffer::new();
        // Not complete initially
        assert!(!sb.is_complete());

        // Assistant with tool_calls — not a final response
        use model_provider::ToolCall;
        let tc = ToolCall::new("c1", "tool", "{}");
        let assist_with_tool = AnnotatedMessage::new(
            MessageId(1),
            0,
            Message::Assistant {
                content: Some("calling...".to_string()),
                tool_calls: Some(vec![tc]),
                reasoning_content: None,
            },
            MessageSource::ModelGeneration,
        );
        sb.push(assist_with_tool);
        assert!(!sb.is_complete());

        // Assistant without tool_calls — final response
        let final_assist = AnnotatedMessage::new(
            MessageId(2),
            0,
            Message::assistant("done"),
            MessageSource::ModelGeneration,
        );
        sb.push(final_assist);
        assert!(sb.is_complete());
    }
}
