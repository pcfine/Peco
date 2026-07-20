// ============================================================================
// Session — 会话全部状态的权威容器（零锁单线程版本）
// ============================================================================
//
// Session 由 AgentLooper 以 `Box<Session>` 独占所有权。
// 所有可变操作需要 `&mut self`，纯读操作为 `&self`。
// 编译期保证单线程安全，无需内部锁。

use std::collections::VecDeque;
use std::sync::Arc;

use model_provider::{Message, Usage};

use super::buffer::{CommittedBuffer, StagingBuffer};
use super::error::SessionError;
use super::snapshot::{SessionSnapshot, TurnBoundaryToken};
use super::types::{
    AnnotatedMessage, InputPriority, MessageId, MessageSource, PendingInput, SessionState,
    unix_timestamp_ms, unix_timestamp_secs,
};

/// 会话实体。
///
/// # 并发模型
///
/// Session 不包含内部锁。由持有者（AgentLooper）通过 `&mut self` 保证独占访问。
///
/// # 持久化
///
/// Session 不感知持久化状态。持久化由外部 `SessionPersister` 在 turn 边界触发。
pub struct Session {
    /// 会话唯一标识（不可变）
    id: String,
    /// 会话描述
    description: String,
    /// 会话创建时间（Unix 秒，构造时设定，不可变）
    created_at: u64,

    // ── 分层消息存储 ──
    /// 已确认的 turn 历史
    committed: CommittedBuffer,
    /// 当前 turn 进行中的消息
    staging: StagingBuffer,
    /// 排队中的用户输入
    pending: VecDeque<PendingInput>,

    // ── 运行时状态 ──
    /// 状态机
    state: SessionState,
    /// 当前 turn 编号（下一次 commit 后的值）
    turn_index: usize,
    /// 聚合 token 用量
    total_usage: Usage,
    /// 下一个消息 ID（单调递增）
    next_message_id: u64,

    // ── 时间戳 ──
    /// 最后一次变更时间（Unix 秒）
    updated_at: u64,
    /// 最后一次活跃时间（Unix 秒）
    last_active_at: u64,
}

impl Session {
    // ── 构造 ──────────────────────────────────────────────────────────

    /// 创建新的空会话。
    pub fn new(id: String, description: String) -> Self {
        let now = unix_timestamp_secs();
        Self {
            id,
            description,
            created_at: now,
            committed: CommittedBuffer::new(),
            staging: StagingBuffer::new(),
            pending: VecDeque::new(),
            state: SessionState::Idle,
            turn_index: 0,
            total_usage: Usage::default(),
            next_message_id: 0,
            updated_at: now,
            last_active_at: now,
        }
    }

    /// 从持久化快照重建。
    ///
    /// 防御性处理：state 统一规范化为 Idle，staging 恒为空。
    /// v3 格式保证这些不变量，此处作为磁盘损坏/手动编辑的防御。
    pub fn from_snapshot(
        id: String,
        description: String,
        created_at: u64,
        snapshot: SessionSnapshot,
    ) -> Self {
        // 计算合理的 next_message_id（取已有的最大 id + 1，防御性兜底）
        let max_id = snapshot
            .committed_turns
            .iter()
            .flat_map(|turn| turn.iter())
            .map(|am| am.id.0)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0);
        let next_id = snapshot.next_message_id.max(max_id);

        let now = unix_timestamp_secs();
        Self {
            id,
            description,
            created_at,
            committed: CommittedBuffer::from_turns(snapshot.committed_turns),
            staging: StagingBuffer::new(), // ← 恒为空
            pending: snapshot.pending_inputs.into(),
            state: SessionState::Idle, // ← 恒为 Idle
            turn_index: snapshot.turn_index,
            total_usage: snapshot.total_usage,
            next_message_id: next_id,
            updated_at: now,
            last_active_at: now,
        }
    }

    // ── 元数据（&self，无副作用）──────────────────────────────────────

    /// 会话唯一标识符。
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 会话描述。
    pub fn description(&self) -> &str {
        &self.description
    }

    /// 设置会话描述。
    pub fn set_description(&mut self, desc: String) {
        self.description = desc;
        self.touch();
    }

    /// 会话创建时间（Unix 秒）。
    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    // ── 状态查询（&self）──────────────────────────────────────────────

    /// 获取当前会话运行状态。
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// 获取当前 turn 编号。
    pub fn turn_index(&self) -> usize {
        self.turn_index
    }

    /// 获取聚合 token 用量。
    pub fn total_usage(&self) -> Usage {
        self.total_usage.clone()
    }

    /// 是否处于 Idle 状态。
    pub fn is_idle(&self) -> bool {
        self.state == SessionState::Idle
    }

    /// 是否处于 Active 状态。
    pub fn is_active(&self) -> bool {
        self.state == SessionState::Active
    }

    /// 消息总数（committed + staging）。
    pub fn message_count(&self) -> usize {
        self.committed.message_count() + self.staging.len()
    }

    // ── 消息访问（&self，零拷贝引用）──────────────────────────────────

    /// 返回全部 committed + staging 消息的引用迭代器。
    ///
    /// 顺序：committed（按 turn 顺序）→ staging（user_input 在前，messages 在后）。
    pub fn all_message_refs(&self) -> impl Iterator<Item = &AnnotatedMessage> {
        self.committed.iter_all().chain(self.staging.iter_all())
    }

    /// 返回 committed turns 的切片（不可变引用）。
    pub fn committed_turns(&self) -> &[Vec<AnnotatedMessage>] {
        self.committed.turns()
    }

    /// 返回 staging 消息的切片（不含 user_input）。
    pub fn staging_messages(&self) -> &[AnnotatedMessage] {
        self.staging.messages_ref()
    }

    /// 返回 staging user_input 的引用。
    pub fn staging_user_input(&self) -> Option<&AnnotatedMessage> {
        self.staging.user_input_ref()
    }

    /// 展示用消息的引用迭代器（UI 渲染 — 仅 User query + Assistant 最终回复）。
    pub fn display_message_refs(&self) -> impl Iterator<Item = &AnnotatedMessage> {
        self.committed.iter_all().filter(|am| am.is_displayable())
    }

    // ── Turn 生命周期（&mut self，状态机守卫）─────────────────────────

    /// 开始新 turn（仅 Idle 状态）。
    pub fn start_turn(&mut self, user_text: String) -> Result<(), SessionError> {
        if !self.state.can_start_turn() {
            return Err(SessionError::InvalidStateTransition {
                current_state: self.state,
                action: "start_turn".to_string(),
            });
        }

        let id = self.allocate_message_id();
        let am = AnnotatedMessage {
            id,
            turn_index: self.turn_index,
            message: Arc::new(Message::user(user_text)),
            timestamp_ms: unix_timestamp_ms(),
            estimated_tokens: None,
            source: MessageSource::UserInput,
        };

        self.staging.set_user_input(am);
        self.state = SessionState::Active;
        self.touch();
        Ok(())
    }

    /// 向 staging 追加消息（仅 Active 状态）。
    pub fn stage_message(
        &mut self,
        source: MessageSource,
        msg: Message,
    ) -> Result<MessageId, SessionError> {
        if !self.state.can_stage_message() {
            return Err(SessionError::InvalidStateTransition {
                current_state: self.state,
                action: "stage_message".to_string(),
            });
        }

        let id = self.allocate_message_id();
        let am = AnnotatedMessage {
            id,
            turn_index: self.turn_index,
            message: Arc::new(msg),
            timestamp_ms: unix_timestamp_ms(),
            estimated_tokens: None,
            source,
        };

        self.staging.push(am);
        self.touch();
        Ok(id)
    }

    /// 提交当前 turn（Active → Idle）。
    ///
    /// 返回 `TurnBoundaryToken`，用于后续调用 `snapshot()`。
    pub fn commit_turn(&mut self) -> Result<TurnBoundaryToken, SessionError> {
        if self.state != SessionState::Active {
            return Err(SessionError::InvalidStateTransition {
                current_state: self.state,
                action: "commit_turn".to_string(),
            });
        }

        if self.staging.is_empty() {
            // 空 turn — 直接跳过，不 commit
            self.state = SessionState::Idle;
            self.touch();
            return Ok(TurnBoundaryToken(()));
        }

        let turn_messages = self.staging.take_all();
        self.committed.push_turn(turn_messages);
        self.turn_index += 1;
        self.state = SessionState::Idle;
        self.touch();
        Ok(TurnBoundaryToken(()))
    }

    /// 回滚当前 turn（Active/Cancelling → Idle，可选 requeue）。
    ///
    /// 返回 `TurnBoundaryToken`，用于后续调用 `snapshot()`。
    pub fn rollback_turn(&mut self, requeue: bool) -> Result<TurnBoundaryToken, SessionError> {
        if requeue && let Some(ui) = self.staging.take_user_input() {
            let text = match ui.message.as_ref() {
                Message::User { content } => content.clone(),
                _ => String::new(),
            };
            if !text.is_empty() {
                self.pending.push_front(PendingInput::new(text));
            }
        }

        self.staging.clear();
        self.state = SessionState::Idle;
        self.touch();
        Ok(TurnBoundaryToken(()))
    }

    /// 回滚到指定 turn，丢弃该 turn 之后的所有已 committed turn。
    ///
    /// 仅在 Idle 状态可调用。返回被删除的 turn 数量。
    pub fn rollback_to_turn(&mut self, turn: usize) -> Result<usize, SessionError> {
        if turn > self.turn_index {
            return Err(SessionError::TurnOutOfBounds {
                requested: turn,
                max: self.turn_index,
            });
        }

        // 先清空 staging（防御性）
        self.staging.clear();

        // 截断 committed
        let removed = self.committed.truncate_to(turn);
        self.turn_index = turn;
        self.state = SessionState::Idle;
        self.touch();
        Ok(removed)
    }

    // ── Pending 队列（&mut self）──────────────────────────────────────

    /// 将用户输入加入 pending 队列（默认 Normal 优先级）。
    pub fn enqueue_pending(&mut self, text: String) {
        self.pending.push_back(PendingInput::new(text));
    }

    /// 将指定优先级的用户输入加入 pending 队列。
    pub fn enqueue_pending_with_priority(&mut self, text: String, priority: InputPriority) {
        self.pending
            .push_back(PendingInput::with_priority(text, priority));
    }

    /// 从 pending 队列取出下一个输入并启动新 turn。
    ///
    /// 优先处理 `Interrupt` 优先级输入（从前往后扫描，取第一个 Interrupt）。
    /// 若 `start_turn` 失败，输入重新放入队列头部（保证不丢失）。
    /// 返回 `Ok(true)` 表示成功启动新 turn，
    /// `Ok(false)` 表示队列为空。
    pub fn dequeue_and_start_turn(&mut self) -> Result<bool, SessionError> {
        // 优先处理 Interrupt：从队列中找到第一个 Interrupt 并移除
        let interrupt_idx = self
            .pending
            .iter()
            .position(|input| input.priority == InputPriority::Interrupt);

        let input = if let Some(idx) = interrupt_idx {
            // Safety: idx is guaranteed valid by position(); remove(idx) returns Option<T>
            // for OOB safety
            self.pending.remove(idx)
        } else {
            self.pending.pop_front()
        };

        let input = match input {
            Some(i) => i,
            None => return Ok(false),
        };

        match self.start_turn(input.text.clone()) {
            Ok(()) => Ok(true),
            Err(e) => {
                // 失败时将输入重新放入队列头部
                self.pending.push_front(input);
                Err(e)
            }
        }
    }

    /// 是否有排队中的输入。
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    // ── Token 用量（&mut self）────────────────────────────────────────

    /// 累加 token 用量。
    pub fn add_usage(&mut self, usage: Usage) {
        self.total_usage.input_tokens += usage.input_tokens;
        self.total_usage.output_tokens += usage.output_tokens;
        self.total_usage.total_tokens += usage.total_tokens;
    }

    // ── 取消（&mut self）──────────────────────────────────────────────

    /// 取消当前 turn（Active → Cancelling）。
    ///
    /// 若已在 Cancelling 状态，无操作并返回 Ok。
    /// Cancelling 状态下由 AgentLooper 调用 `rollback_turn()` 完成清理。
    pub fn cancel(&mut self) -> Result<(), SessionError> {
        match self.state {
            SessionState::Active => {
                self.state = SessionState::Cancelling;
                self.touch();
                Ok(())
            }
            SessionState::Cancelling => Ok(()),
            _ => Err(SessionError::InvalidStateTransition {
                current_state: self.state,
                action: "cancel".to_string(),
            }),
        }
    }

    // ── 快照（&self，受 Token 保护）───────────────────────────────────

    /// 生成持久化快照。
    ///
    /// 需要 `TurnBoundaryToken`（仅 `commit_turn()` / `rollback_turn()` 可产生），
    /// 编译期保证快照只在 turn 边界生成。
    pub fn snapshot(&self, _token: &TurnBoundaryToken) -> SessionSnapshot {
        SessionSnapshot {
            committed_turns: self.committed.turns().to_vec(),
            turn_index: self.turn_index,
            total_usage: self.total_usage.clone(),
            next_message_id: self.next_message_id,
            pending_inputs: self.pending.iter().cloned().collect(),
        }
    }

    // ── 内部辅助 ──────────────────────────────────────────────────────

    /// 分配下一个 MessageId。
    fn allocate_message_id(&mut self) -> MessageId {
        let id = MessageId(self.next_message_id);
        self.next_message_id += 1;
        id
    }

    /// 更新时间戳。
    fn touch(&mut self) {
        let now = unix_timestamp_secs();
        self.updated_at = now;
        self.last_active_at = now;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use model_provider::Message;

    fn make_session() -> Session {
        Session::new("test-id".to_string(), "test session".to_string())
    }

    #[test]
    fn test_new_session_is_idle() {
        let s = make_session();
        assert_eq!(s.state(), SessionState::Idle);
        assert_eq!(s.turn_index(), 0);
        assert_eq!(s.message_count(), 0);
        assert!(s.is_idle());
        assert!(!s.is_active());
    }

    #[test]
    fn test_start_turn_transitions_to_active() {
        let mut s = make_session();
        s.start_turn("hello".to_string()).unwrap();
        assert_eq!(s.state(), SessionState::Active);
        assert_eq!(s.message_count(), 1); // user input in staging
    }

    #[test]
    fn test_start_turn_when_active_fails() {
        let mut s = make_session();
        s.start_turn("first".to_string()).unwrap();
        let result = s.start_turn("second".to_string());
        assert!(result.is_err());
        match result.unwrap_err() {
            SessionError::InvalidStateTransition { current_state, .. } => {
                assert_eq!(current_state, SessionState::Active);
            }
            _ => panic!("expected InvalidStateTransition"),
        }
    }

    #[test]
    fn test_stage_message_when_idle_fails() {
        let mut s = make_session();
        let result = s.stage_message(MessageSource::ModelGeneration, Message::assistant("hi"));
        assert!(result.is_err());
    }

    #[test]
    fn test_stage_and_commit_turn() {
        let mut s = make_session();
        s.start_turn("hello".to_string()).unwrap();
        s.stage_message(
            MessageSource::ModelGeneration,
            Message::assistant("hi there"),
        )
        .unwrap();

        let token = s.commit_turn().unwrap();
        assert_eq!(s.state(), SessionState::Idle);
        assert_eq!(s.turn_index(), 1);
        // After commit, 2 messages are in committed (user + assistant)
        assert_eq!(s.message_count(), 2);

        // token should allow snapshot
        let snap = s.snapshot(&token);
        assert_eq!(snap.committed_turns.len(), 1);
        assert_eq!(snap.turn_index, 1);
    }

    #[test]
    fn test_commit_turn_when_idle_fails() {
        let mut s = make_session();
        let result = s.commit_turn();
        assert!(result.is_err());
        match result {
            Err(SessionError::InvalidStateTransition {
                current_state,
                action,
            }) => {
                assert_eq!(current_state, SessionState::Idle);
                assert_eq!(action, "commit_turn");
            }
            _ => panic!("expected InvalidStateTransition"),
        }
    }

    #[test]
    fn test_all_message_refs_includes_committed_and_staging() {
        let mut s = make_session();

        // Turn 0
        s.start_turn("q1".to_string()).unwrap();
        s.stage_message(MessageSource::ModelGeneration, Message::assistant("a1"))
            .unwrap();
        let _token = s.commit_turn().unwrap();

        // Turn 1 (staging, not committed)
        s.start_turn("q2".to_string()).unwrap();
        s.stage_message(MessageSource::ModelGeneration, Message::assistant("a2"))
            .unwrap();

        let refs: Vec<&AnnotatedMessage> = s.all_message_refs().collect();
        assert_eq!(refs.len(), 4); // User(q1), Assistant(a1), User(q2), Assistant(a2)
    }

    #[test]
    fn test_rollback_turn() {
        let mut s = make_session();
        s.start_turn("hello".to_string()).unwrap();
        s.stage_message(
            MessageSource::ModelGeneration,
            Message::assistant("partial"),
        )
        .unwrap();

        let _token = s.rollback_turn(false).unwrap();
        assert_eq!(s.state(), SessionState::Idle);
        assert_eq!(s.message_count(), 0); // staging cleared
    }

    #[test]
    fn test_rollback_turn_requeue() {
        let mut s = make_session();
        s.start_turn("hello".to_string()).unwrap();

        let _token = s.rollback_turn(true).unwrap();
        assert!(s.has_pending());
        // Dequeue should restart the turn
        let result = s.dequeue_and_start_turn().unwrap();
        assert!(result);
        assert_eq!(s.state(), SessionState::Active);
    }

    #[test]
    fn test_pending_queue_flow() {
        let mut s = make_session();

        // Start a turn to make it active
        s.start_turn("q1".to_string()).unwrap();

        // Enqueue pending while active
        s.enqueue_pending("q2".to_string());
        s.enqueue_pending("q3".to_string());
        assert!(s.has_pending());

        // Complete current turn
        s.stage_message(MessageSource::ModelGeneration, Message::assistant("a1"))
            .unwrap();
        let _token = s.commit_turn().unwrap();

        // Dequeue should start new turn
        let result = s.dequeue_and_start_turn().unwrap();
        assert!(result);
        assert_eq!(s.state(), SessionState::Active);
        assert!(s.has_pending()); // one more
    }

    #[test]
    fn test_dequeue_and_start_turn_empty() {
        let mut s = make_session();
        let result = s.dequeue_and_start_turn().unwrap();
        assert!(!result); // queue was empty
    }

    #[test]
    fn test_rollback_to_turn() {
        let mut s = make_session();

        // Create 3 turns
        for i in 0..3 {
            s.start_turn(format!("q{i}")).unwrap();
            s.stage_message(
                MessageSource::ModelGeneration,
                Message::assistant(format!("a{i}")),
            )
            .unwrap();
            let _ = s.commit_turn().unwrap();
        }
        assert_eq!(s.turn_index(), 3);

        // Rollback to turn 1
        let removed = s.rollback_to_turn(1).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(s.committed_turns().len(), 1);
        assert_eq!(s.turn_index(), 1);
    }

    #[test]
    fn test_add_usage() {
        let mut s = make_session();
        s.add_usage(Usage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
        });
        s.add_usage(Usage {
            input_tokens: 200,
            output_tokens: 100,
            total_tokens: 300,
        });

        let total = s.total_usage();
        assert_eq!(total.input_tokens, 300);
        assert_eq!(total.output_tokens, 150);
        assert_eq!(total.total_tokens, 450);
    }

    #[test]
    fn test_display_message_refs_filters_correctly() {
        let mut s = make_session();

        // Turn with tool calls
        s.start_turn("weather?".to_string()).unwrap();
        // Assistant with tool call — should NOT be displayable
        use model_provider::ToolCall;
        let tc = ToolCall::new("call_1", "get_weather", r#"{"city":"BJ"}"#);
        let assistant_tool = Message::Assistant {
            content: Some("Let me check...".to_string()),
            tool_calls: Some(vec![tc]),
            reasoning_content: None,
        };
        s.stage_message(MessageSource::ModelGeneration, assistant_tool)
            .unwrap();
        // Tool result — should NOT be displayable
        s.stage_message(
            MessageSource::ToolExecution {
                tool_name: "get_weather".to_string(),
            },
            Message::tool("call_1", "Sunny, 25C"),
        )
        .unwrap();
        // Final assistant — should BE displayable
        s.stage_message(
            MessageSource::ModelGeneration,
            Message::assistant("Beijing is sunny, 25C"),
        )
        .unwrap();
        let _token = s.commit_turn().unwrap();

        let display: Vec<_> = s.display_message_refs().collect();
        // Only 2: user query + final assistant reply
        assert_eq!(display.len(), 2);
        assert!(display[0].is_displayable()); // User
        assert!(display[1].is_displayable()); // Final assistant
    }

    #[test]
    fn test_cancel_active_turn() {
        let mut s = make_session();
        s.start_turn("hello".to_string()).unwrap();
        assert_eq!(s.state(), SessionState::Active);

        s.cancel().unwrap();
        assert_eq!(s.state(), SessionState::Cancelling);

        // Double cancel is ok
        s.cancel().unwrap();
        assert_eq!(s.state(), SessionState::Cancelling);
    }

    #[test]
    fn test_cancel_when_idle_fails() {
        let mut s = make_session();
        let result = s.cancel();
        assert!(result.is_err());
    }

    #[test]
    fn test_from_snapshot_normalizes_state() {
        // Create a snapshot with some data
        let mut s = make_session();
        s.start_turn("q".to_string()).unwrap();
        s.stage_message(MessageSource::ModelGeneration, Message::assistant("a"))
            .unwrap();
        let token = s.commit_turn().unwrap();
        let snap = s.snapshot(&token);

        // Even if snapshot somehow had non-Idle data, from_snapshot normalizes
        let restored =
            Session::from_snapshot("new-id".to_string(), "restored".to_string(), 1000, snap);
        assert_eq!(restored.state(), SessionState::Idle);
        assert!(restored.staging_user_input().is_none());
        assert_eq!(restored.committed_turns().len(), 1);
        assert_eq!(restored.turn_index(), 1);
    }

    #[test]
    fn test_metadata_accessors() {
        let s = Session::new("my-id".to_string(), "my desc".to_string());
        assert_eq!(s.id(), "my-id");
        assert_eq!(s.description(), "my desc");
        assert!(s.created_at() > 0);
    }

    #[test]
    fn test_set_description() {
        let mut s = make_session();
        s.set_description("new desc".to_string());
        assert_eq!(s.description(), "new desc");
    }

    #[test]
    fn test_pending_interrupt_priority() {
        let mut s = make_session();

        // Enqueue normal input, then interrupt input
        s.enqueue_pending("normal".to_string());
        s.enqueue_pending_with_priority("interrupt".to_string(), InputPriority::Interrupt);
        s.enqueue_pending("later".to_string());

        // Dequeue — should get interrupt first
        let result = s.dequeue_and_start_turn().unwrap();
        assert!(result);
        // The user input in staging should be the interrupt
        let ui = s.staging_user_input().unwrap();
        assert_eq!(ui.message.as_ref(), &Message::user("interrupt"));

        // Commit
        let _ = s.commit_turn().unwrap();

        // Next should be "normal" (enqueued first, before "later")
        let result = s.dequeue_and_start_turn().unwrap();
        assert!(result);
        let ui = s.staging_user_input().unwrap();
        assert_eq!(ui.message.as_ref(), &Message::user("normal"));
    }
}
