// ============================================================================
// Session — 会话全部状态的权威容器（零锁单线程版本）
// ============================================================================
//
// Session 由 AgentLooper 以 `Box<Session>` 独占所有权。
// 所有可变操作需要 `&mut self`，纯读操作为 `&self`。
// 编译期保证单线程安全，无需内部锁。

use std::collections::VecDeque;
use std::sync::Arc;

use model_provider::{Content, InputItem, Role, Usage};

use super::buffer::{CommittedBuffer, StagingBuffer};
use super::error::SessionError;
use super::snapshot::{SessionSnapshot, TurnBoundaryToken};
use super::types::{
    AnnotatedMessage, MessageId, MessageSource, PendingInput, SessionState, merge_contents,
    unix_timestamp_ms, unix_timestamp_secs,
};

/// 悬空 tool_call 的合成结果文案。
///
/// **行为契约，不是 UI 文案**：该串会写进 committed 历史并发送给模型。
/// 错误语义只能由文案承载 —— [`InputItem::FunctionCallOutput`] 协议层
/// 没有 `is_error` 位（`ToolCallResult.is_error` 只喂事件与 hook，上线时被丢弃），
/// 所以必须带 `[interrupted]` 前缀；写成中性文案会被模型当作正常工具输出继续推理。
pub(crate) const INTERRUPTED_TOOL_OUTPUT: &str =
    "[interrupted] tool execution was interrupted before completion";

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
    /// 钉扎在上下文最前的历史摘要（compaction 产物）。
    ///
    /// 不属于任何 committed turn；`all_message_refs()` 将其排在最前，
    /// 使压缩摘要成为每次请求上下文的第一条历史消息。
    pinned_summary: Option<AnnotatedMessage>,

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
            pinned_summary: None,
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
            .chain(snapshot.pinned_summary.iter())
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
            pinned_summary: snapshot.pinned_summary,
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

    /// 返回全部 pinned 摘要 + committed + staging 消息的引用迭代器。
    ///
    /// 顺序：pinned 摘要（若有，恒在最前）→ committed（按 turn 顺序）
    /// → staging（user_input 在前，messages 在后）。
    pub fn all_message_refs(&self) -> impl Iterator<Item = &AnnotatedMessage> {
        self.pinned_summary
            .iter()
            .chain(self.committed.iter_all())
            .chain(self.staging.iter_all())
    }

    /// 返回钉扎的历史摘要（compaction 产物）。
    pub fn pinned_summary(&self) -> Option<&AnnotatedMessage> {
        self.pinned_summary.as_ref()
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

    /// staging 全部消息的克隆（user_input 在前）。
    ///
    /// [`StagingBuffer::take_all`](super::buffer::StagingBuffer) 的无副作用版本，
    /// 顺序与之一致 —— 检查点按此约定落盘，水化时按同一约定还原。
    pub fn staging_all(&self) -> Vec<AnnotatedMessage> {
        self.staging.iter_all().cloned().collect()
    }

    /// 展示用消息的引用迭代器（UI 渲染 — 仅 User query + Assistant 最终回复）。
    pub fn display_message_refs(&self) -> impl Iterator<Item = &AnnotatedMessage> {
        self.committed.iter_all().filter(|am| am.is_displayable())
    }

    // ── Turn 生命周期（&mut self，状态机守卫）─────────────────────────

    /// 开始新 turn（仅 Idle 状态），用户消息来源标记为 [`MessageSource::UserInput`]。
    pub fn start_turn(&mut self, user_text: Content) -> Result<(), SessionError> {
        self.start_turn_with_source(user_text, MessageSource::UserInput)
    }

    /// 开始新 turn，用户消息携带指定 [`MessageSource`]（仅 Idle 状态）。
    ///
    /// pending 批量合并续接走 [`MessageSource::MergedPending`]（展示层
    /// 据此剥离 `---` 标记）；直接用户输入走 [`MessageSource::UserInput`]。
    pub fn start_turn_with_source(
        &mut self,
        user_text: Content,
        source: MessageSource,
    ) -> Result<(), SessionError> {
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
            message: Arc::new(InputItem::Message {
                role: Role::User,
                content: user_text,
            }),
            timestamp_ms: unix_timestamp_ms(),
            estimated_tokens: None,
            source,
        };

        self.staging.set_user_input(am);
        self.state = SessionState::Active;
        self.touch();
        Ok(())
    }

    /// 向 staging 追加一个中立 item（仅 Active 状态）。
    pub fn stage_item(
        &mut self,
        source: MessageSource,
        item: InputItem,
    ) -> Result<MessageId, SessionError> {
        if !self.state.can_stage_message() {
            return Err(SessionError::InvalidStateTransition {
                current_state: self.state,
                action: "stage_item".to_string(),
            });
        }

        let id = self.push_staged(source, item);
        Ok(id)
    }

    /// staging 追加，不走 [`Self::stage_item`] 的 `Active` 守卫。
    ///
    /// 供 `interrupt_turn` 的补齐项使用：那些项不是模型产出，
    /// `can_stage_message()`（只认 `Active`）对它们没有语义，
    /// 而补齐必须能在 `Cancelling` / `Interrupted` 下发生。
    fn push_staged(&mut self, source: MessageSource, item: InputItem) -> MessageId {
        let id = self.allocate_message_id();
        let am = self.make_annotated(id, source, item);
        self.staging.push(am);
        self.touch();
        id
    }

    /// 构造一条归属当前 turn 的 [`AnnotatedMessage`]。
    ///
    /// `stage_item` 与 `push_staged` 共用，避免 `timestamp_ms` /
    /// `estimated_tokens` 在两处漂移。
    fn make_annotated(
        &self,
        id: MessageId,
        source: MessageSource,
        item: InputItem,
    ) -> AnnotatedMessage {
        AnnotatedMessage {
            id,
            turn_index: self.turn_index,
            message: Arc::new(item),
            timestamp_ms: unix_timestamp_ms(),
            estimated_tokens: None,
            source,
        }
    }

    /// 为 staging 中悬空的 `FunctionCall` 合成配对 `FunctionCallOutput`，
    /// 返回合成条数。按 `called` 原序追加。
    ///
    /// 补齐是强制的：provider 要求每条 `tool_calls` 都有配对结果，
    /// 否则下一次请求直接 400 —— 保留半轮而不补齐等于把会话弄坏。
    ///
    /// `reason` 是中断原因（人类可读），挂进 `MessageSource`。合成项的
    /// **输出文案**恒定是 [`INTERRUPTED_TOOL_OUTPUT`]（模型面向契约），
    /// 但来源标记带真实原因 —— 这样消费方不必分辨「取哪一条 `InterruptedTurn`」。
    fn patch_dangling_tool_calls(&mut self, reason: &str) -> usize {
        let mut called: Vec<String> = Vec::new();
        let mut answered: Vec<String> = Vec::new();
        for am in self.staging.messages_ref() {
            match am.message.as_ref() {
                InputItem::FunctionCall { call_id, .. } => called.push(call_id.clone()),
                InputItem::FunctionCallOutput { call_id, .. } => answered.push(call_id.clone()),
                _ => {}
            }
        }

        let dangling: Vec<String> = called
            .into_iter()
            .filter(|id| !answered.contains(id))
            .collect();

        for call_id in &dangling {
            self.push_staged(
                MessageSource::InterruptedTurn {
                    reason: reason.to_string(),
                },
                InputItem::FunctionCallOutput {
                    call_id: call_id.clone(),
                    output: INTERRUPTED_TOOL_OUTPUT.into(),
                },
            );
        }
        dangling.len()
    }

    /// 追加一条中断说明，让这一轮对模型自解释，而不是以悬空工具结果收尾。
    ///
    /// **必须先于调用方产出任何 `FunctionCallOutput` 之外的 assistant 文本**
    /// —— 补齐与说明顺序颠倒会产出「`tool_calls` 后紧跟 assistant」的非法历史。
    fn push_interrupt_notice(&mut self, reason: &str) {
        self.push_staged(
            MessageSource::InterruptedTurn {
                reason: reason.to_string(),
            },
            InputItem::Message {
                role: Role::Assistant,
                content: format!(
                    "[interrupted] this turn was interrupted before completion: {reason}"
                )
                .into(),
            },
        );
    }

    /// 追加合成收尾报告，**取代** [`Self::push_interrupt_notice`]。
    ///
    /// 与中断说明同构（同样走 `push_staged`，收尾态可写），但来源标
    /// [`MessageSource::TurnEpilogue`] —— 消费方据此识别「这条是撞上轮数上限后
    /// 合成的收尾」，不必去猜文案特征串。报告正文已由调用方备好，此处不做加工。
    fn push_closing_notice(&mut self, text: &str) {
        self.push_staged(
            MessageSource::TurnEpilogue,
            InputItem::Message {
                role: Role::Assistant,
                content: text.to_string().into(),
            },
        );
    }

    /// 把收尾时的部分文本补进 staging，供 [`Self::interrupt_turn`] 冻结。
    ///
    /// 截断重试会先回退掉上一次尝试的产物；若此后收尾失败，staging 只剩
    /// `user_input`，[`Self::interrupt_turn`] 便退化成 `rollback_turn` ——
    /// 整轮（含用户提问）从历史里消失，反而比不重试更差。补一条 assistant
    /// 消息把那一轮救回来，使其与「没有重试、直接截断收尾」的历史形态一致。
    ///
    /// **只在 staging 无可保留产物时调用**：否则文本已以原始形态在 staging 里，
    /// 再补一条就是重复。
    ///
    /// 不走 [`Self::stage_item`] 的 `Active` 守卫 —— 收尾态（`Cancelling` /
    /// `Interrupted`）也要能补，与 `push_interrupt_notice` 同理。
    pub fn stage_salvage(&mut self, text: String) {
        self.push_staged(
            MessageSource::ModelGeneration,
            InputItem::Message {
                role: Role::Assistant,
                content: text.into(),
            },
        );
    }

    /// 中断在途轮：有可保留产物则冻结进 committed，否则等同 rollback。
    ///
    /// 与 [`Self::commit_turn`] / [`Self::rollback_turn`] 并列的第三个 turn 边界
    /// 出口。冻结路径做两件补齐（补齐悬空 tool_call → 追加中断说明），
    /// 保证产出的历史对 provider 合法。两种情况下 state 均回到 `Idle`。
    ///
    /// `Idle` 上调用返回 `Err`（staging 必空，调用方应显式 `rollback_turn`）。
    pub fn interrupt_turn(&mut self, reason: &str) -> Result<TurnBoundaryToken, SessionError> {
        self.interrupt_turn_with_closing(reason, None)
    }

    /// 同 [`Self::interrupt_turn`]，但可用 `closing` 取代通常的中断说明。
    ///
    /// `closing` 是撞上 `max_iterations` 上限时合成的收尾报告（已由调用方异步备好）。
    /// 它**取代**而非追加 [`Self::push_interrupt_notice`]：两者同时存在会让
    /// 历史以两条连续 assistant 消息收尾 —— 补齐的工具结果之后只该有一条。
    /// 悬空 `FunctionCall` 的补齐与收尾的相对顺序因此是强制的（补齐在前）。
    ///
    /// `None` 时与 [`Self::interrupt_turn`] 逐字节一致。
    pub fn interrupt_turn_with_closing(
        &mut self,
        reason: &str,
        closing: Option<&str>,
    ) -> Result<TurnBoundaryToken, SessionError> {
        // 与 commit/rollback 不同，这里认 `Cancelling` / `Interrupted`：
        // 补齐项要能在收尾态写入（见 `push_staged`）。
        if !matches!(
            self.state,
            SessionState::Active | SessionState::Cancelling | SessionState::Interrupted
        ) {
            return Err(SessionError::InvalidStateTransition {
                current_state: self.state,
                action: "interrupt_turn".to_string(),
            });
        }

        if self.staging.messages_ref().is_empty() {
            // 退化路径：只有 user_input，没有可保留的模型产物
            return self.rollback_turn(false);
        }

        // ★ 补齐必须先于说明。顺序颠倒会产出「assistant(tool_calls) → assistant(text)
        //   → tool」，即 tool_calls 后紧跟 assistant 而非 tool → 400。
        self.patch_dangling_tool_calls(reason);
        match closing {
            Some(text) => self.push_closing_notice(text),
            None => self.push_interrupt_notice(reason),
        }

        let turn_messages = self.staging.take_all();
        self.committed.push_turn(turn_messages);
        self.turn_index += 1;
        self.state = SessionState::Idle;
        self.touch();
        Ok(TurnBoundaryToken(()))
    }

    /// 把在途轮检查点的消息灌回 staging 并置为 `Active`，返回灌入条数。
    ///
    /// 冷启动恢复路径：[`Self::from_snapshot`] 恒产出 `Idle` + 空 staging，
    /// 检查点里那些已落地但未 commit 的消息因此无法经常规 staging 接口复原
    /// （`stage_item` 只认 `Active`）。调用方灌入后应立即 [`Self::interrupt_turn`]
    /// 完成补齐与冻结 —— 这是让「崩溃前已完成的工具结果」进入历史的唯一路径。
    ///
    /// `staged` 按 [`Self::staging_all`] 的约定排布（首条是 user_input）。
    /// 空切片不做任何变更（返回 `Ok(0)`），非 `Idle` 状态返回 `Err`。
    pub fn hydrate_inflight(
        &mut self,
        staged: Vec<AnnotatedMessage>,
    ) -> Result<usize, SessionError> {
        if self.state != SessionState::Idle {
            return Err(SessionError::InvalidStateTransition {
                current_state: self.state,
                action: "hydrate_inflight".to_string(),
            });
        }
        if staged.is_empty() {
            return Ok(0);
        }

        let count = staged.len();
        let mut iter = staged.into_iter();
        // 首条按约定是 user_input，但类型上无法保证 —— 不是就整条按普通消息
        // 入 staging，不必为一个损坏的检查点 panic。
        let first = iter.next().expect("count > 0");
        if matches!(
            first.message.as_ref(),
            InputItem::Message {
                role: Role::User,
                ..
            }
        ) {
            self.staging.set_user_input(first);
        } else {
            self.staging.push(first);
        }
        for am in iter {
            self.staging.push(am);
        }

        // 恢复的消息带着崩溃进程的消息 ID，必须让计数器越过它们，
        // 否则后续 `stage_item` 会分配出重复 ID。
        let max_id = self.staging.iter_all().map(|am| am.id.0).max();
        if let Some(max_id) = max_id {
            self.next_message_id = self.next_message_id.max(max_id + 1);
        }

        self.state = SessionState::Active;
        self.touch();
        Ok(count)
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
            let content = match ui.message.as_ref() {
                InputItem::Message {
                    role: Role::User,
                    content,
                } => content.clone(),
                _ => Content::Text(String::new()),
            };
            // 空文本且无图片的输入不回队；其余（含纯图片输入）整体保留
            if !content.text_view().is_empty() || content.image_count() > 0 {
                self.pending.push_front(PendingInput::new(content));
            }
        }

        self.staging.clear();
        self.state = SessionState::Idle;
        self.touch();
        Ok(TurnBoundaryToken(()))
    }

    /// 当前 staging 的回退锚点：已暂存消息条数（**不含** `user_input`）。
    ///
    /// 与 [`Self::truncate_staging`] 成对使用：在**发起模型请求之前**取锚点，
    /// 请求产出被回填后若判定需要丢弃（截断重试），用该锚点回退。
    ///
    /// 返回下标而非令牌：无状态，锚点由调用方自己保管 —— 这样 `Session`
    /// 不必为 commit / rollback / interrupt / compact / hydrate 逐个维护失效逻辑。
    pub fn staging_checkpoint(&self) -> usize {
        self.staging.messages_ref().len()
    }

    /// 把 staging 回退到 `checkpoint`：丢弃下标 ≥ `checkpoint` 的已暂存消息。
    ///
    /// 返回实际丢弃的条数。
    ///
    /// # 守卫
    /// - 非 `Active` 状态返回 [`SessionError::InvalidStateTransition`]。只有 `Active`
    ///   下 staging 才在增长（见 [`Self::stage_item`] 的 `can_stage_message()`）；
    ///   在收尾态回退会改掉已经发给用户的 `TurnComplete` 内容。
    /// - `checkpoint` 大于当前条数返回 [`SessionError::StagingCheckpointOutOfBounds`]。
    ///   越界说明锚点与当前 staging 不是同一生命周期，必须显式失败 ——
    ///   静默饱和会掩盖状态错乱。
    ///
    /// # `next_message_id` 不回退
    /// 消息 ID 只需唯一，单调性是白拿的更强保证。回退会让新消息复用已经流出
    /// （`LooperEvent`、`InflightCheckpoint`）的 ID。`hydrate_inflight` 同样
    /// 只前进不回退，本条与它一致。
    pub fn truncate_staging(&mut self, checkpoint: usize) -> Result<usize, SessionError> {
        if !self.state.can_stage_message() {
            return Err(SessionError::InvalidStateTransition {
                current_state: self.state,
                action: "truncate_staging".to_string(),
            });
        }

        let current = self.staging.messages_ref().len();
        if checkpoint > current {
            return Err(SessionError::StagingCheckpointOutOfBounds {
                requested: checkpoint,
                current,
            });
        }

        let dropped = self.staging.truncate_messages(checkpoint);
        if dropped > 0 {
            self.touch();
        }
        Ok(dropped)
    }

    /// 回滚到指定 turn，丢弃该 turn 之后的所有已 committed turn。
    ///
    /// 仅在 Idle 状态可调用。返回被删除的 turn 数量。
    pub fn rollback_to_turn(&mut self, turn: usize) -> Result<usize, SessionError> {
        // 截断按 committed 位置进行（`truncate_to`），守卫上界也必须是位置 —
        // compaction 后 turn_index 计数器（历史累计轮数）大于 committed 轮数，
        // 不能作为上界，否则会放行越界请求并制造新的计数器/位置错位。
        let max = self.committed.len();
        if turn > max {
            return Err(SessionError::TurnOutOfBounds {
                requested: turn,
                max,
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

    // ── 上下文压缩（compaction，仅 turn 边界可调用）──────────────────

    /// 物理修剪最旧的 `evict_count` 轮，并以 `summary` 钉扎替代。
    ///
    /// 仅在 Idle 状态（turn 边界）可调用。被驱逐轮次的内容由调用方
    /// （compaction 模块）先行组装为摘要并交给持久层之外的归档方；
    /// 本方法只负责：
    /// 1. 重编号剩余消息的 `turn_index`，闭合驱逐产生的空洞，使 committed 内
    ///    `turn_index` 单调连续且从 0 起；
    /// 2. 从 committed 最旧端驱逐；
    /// 3. 将摘要写入 pinned_summary（Role::System + SystemInjection 来源）。
    ///
    /// **注意**：`turn_index` 字段（`Self::turn_index`）是"历史累计轮数"计数器，
    /// 本方法**不回退**它 — 压缩后新增轮次的消息 `turn_index` 取计数器值，
    /// 会大于其 committed 位置（差值 = 历史驱逐总数）。因此
    /// `turn_index == committed 位置` 仅对压缩时的存量消息成立，
    /// 消费方不得以消息 `turn_index` 索引 committed 位置（用 `get_turn` 时须以位置为准）。
    ///
    /// 始终保留至少最后一轮 verbatim — 单轮即超预算时不驱逐（返回 0）。
    /// 返回实际驱逐的轮数。
    pub fn compact(&mut self, evict_count: usize, summary: String) -> Result<usize, SessionError> {
        if self.state != SessionState::Idle {
            return Err(SessionError::InvalidStateTransition {
                current_state: self.state,
                action: "compact".to_string(),
            });
        }

        // 至少保留最后一轮
        let evict_count = evict_count.min(self.committed.len().saturating_sub(1));
        if evict_count == 0 {
            return Ok(0);
        }

        // 1. 物理驱逐
        self.committed.evict_front(evict_count);

        // 2. 重编号剩余消息，闭合驱逐产生的 turn_index 空洞
        for turn in self.committed.turns_mut() {
            for am in turn {
                am.turn_index -= evict_count;
            }
        }

        // 3. 钉扎摘要（替换旧摘要 — 递归摘要由调用方合并文本后传入）
        let id = self.allocate_message_id();
        self.pinned_summary = Some(AnnotatedMessage {
            id,
            turn_index: 0,
            message: Arc::new(InputItem::Message {
                role: Role::System,
                content: summary.into(),
            }),
            timestamp_ms: unix_timestamp_ms(),
            estimated_tokens: None,
            source: MessageSource::SystemInjection {
                reason: "compaction".to_string(),
            },
        });

        self.touch();
        Ok(evict_count)
    }

    // ── Pending 队列（&mut self）──────────────────────────────────────

    /// 将用户输入加入 pending 队列。
    pub fn enqueue_pending(&mut self, content: Content) {
        self.pending.push_back(PendingInput::new(content));
    }

    /// 排空整个 pending 队列，合并为一条消息，启动新 turn。
    ///
    /// 一次消化全部排队输入（而非逐条），合并语义见 [`merge_contents`]。
    /// **来源按批大小定**：单条走 [`MessageSource::UserInput`]（逐字节未改的原始
    /// 输入），多条才走 [`MessageSource::MergedPending`]。
    ///
    /// 展示层只对 `MergedPending` 调 [`strip_merge_markers`]，而它会剥掉内容里
    /// **任何**恰好等于 `---` 的行 —— 靠来源标记把直接输入挡在门外。若单条也打
    /// `MergedPending`，用户自己写的一行 `---` 会在渲染时凭空消失。
    ///
    /// 返回 `Ok(true)` = 成功启动新 turn，`Ok(false)` = 队列为空。
    pub fn dequeue_and_start_turn(&mut self) -> Result<bool, SessionError> {
        if self.pending.is_empty() {
            return Ok(false);
        }

        // start_turn 唯一失败点是状态守卫 —— 前置检查，失败时队列原封
        // 不动（旧「整批回队」语义由前置检查天然满足，且省掉整批 clone：
        // 排队图片是 base64 data URI，深克隆代价随队列线性增长）。
        if !self.state.can_start_turn() {
            return Err(SessionError::InvalidStateTransition {
                current_state: self.state,
                action: "start_turn".to_string(),
            });
        }

        let batch: Vec<PendingInput> = self.pending.drain(..).collect();
        let source = if batch.len() == 1 {
            MessageSource::UserInput
        } else {
            MessageSource::MergedPending
        };
        let merged = merge_contents(batch.into_iter().map(|i| i.content).collect());

        // 前置检查后不可达（其间无状态变更）；万一未来 start_turn 增加
        // 新失败点，这里只报错不回队 —— 合并态已无法还原为原序批次，
        // 新增失败点时应同步在此补回队策略。
        self.start_turn_with_source(merged, source).map(|()| true)
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
    ///
    /// 收尾由 [`Self::interrupt_turn`] 或 [`Self::rollback_turn`] 完成 ——
    /// 二者都接受 `Cancelling` 作为入口状态。
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
            pinned_summary: self.pinned_summary.clone(),
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
    fn tool(call_id: impl Into<String>, content: impl Into<Content>) -> InputItem {
        InputItem::FunctionCallOutput {
            call_id: call_id.into(),
            output: content.into(),
        }
    }
    fn function_call(call_id: impl Into<String>, name: impl Into<String>) -> InputItem {
        InputItem::FunctionCall {
            call_id: call_id.into(),
            name: name.into(),
            arguments: "{}".to_string(),
        }
    }

    fn make_session() -> Session {
        Session::new("test-id".to_string(), "test session".to_string())
    }

    /// 模拟 chat 协议的 tool_call ↔ tool_result 配对校验。
    ///
    /// - `FunctionCall` 入栈
    /// - `FunctionCallOutput` 必须匹配栈内项（否则错序 / 无主结果）
    /// - 非空栈期间不得出现 assistant `Message`（`tool_calls` 后必须紧跟 tool）
    /// - 末尾栈必须为空（无悬空调用）
    ///
    /// 把「对 provider 合法」从「我按文档写了」变成机器可验。
    #[allow(dead_code)]
    fn assert_wire_valid(items: &[InputItem]) {
        let mut stack: Vec<String> = Vec::new();
        for item in items {
            match item {
                InputItem::FunctionCall { call_id, .. } => stack.push(call_id.clone()),
                InputItem::FunctionCallOutput { call_id, .. } => {
                    let pos = stack.iter().rposition(|c| c == call_id);
                    assert!(
                        pos.is_some(),
                        "FunctionCallOutput for {call_id} without a matching FunctionCall"
                    );
                    stack.remove(pos.unwrap());
                }
                InputItem::Message {
                    role: Role::Assistant,
                    ..
                } => assert!(
                    stack.is_empty(),
                    "assistant Message while tool results still pending: {stack:?}"
                ),
                _ => {}
            }
        }
        assert!(stack.is_empty(), "dangling tool calls at end: {stack:?}");
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
        s.start_turn("hello".into()).unwrap();
        assert_eq!(s.state(), SessionState::Active);
        assert_eq!(s.message_count(), 1); // user input in staging
    }

    #[test]
    fn test_start_turn_when_active_fails() {
        let mut s = make_session();
        s.start_turn("first".into()).unwrap();
        let result = s.start_turn("second".into());
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
        let result = s.stage_item(MessageSource::ModelGeneration, assistant("hi"));
        assert!(result.is_err());
    }

    #[test]
    fn test_stage_and_commit_turn() {
        let mut s = make_session();
        s.start_turn("hello".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, assistant("hi there"))
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
        s.start_turn("q1".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, assistant("a1"))
            .unwrap();
        let _token = s.commit_turn().unwrap();

        // Turn 1 (staging, not committed)
        s.start_turn("q2".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, assistant("a2"))
            .unwrap();

        let refs: Vec<&AnnotatedMessage> = s.all_message_refs().collect();
        assert_eq!(refs.len(), 4); // User(q1), Assistant(a1), User(q2), Assistant(a2)
    }

    #[test]
    fn test_rollback_turn() {
        let mut s = make_session();
        s.start_turn("hello".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, assistant("partial"))
            .unwrap();

        let _token = s.rollback_turn(false).unwrap();
        assert_eq!(s.state(), SessionState::Idle);
        assert_eq!(s.message_count(), 0); // staging cleared
    }

    #[test]
    fn test_rollback_turn_requeue() {
        let mut s = make_session();
        s.start_turn("hello".into()).unwrap();

        let _token = s.rollback_turn(true).unwrap();
        assert!(s.has_pending());
        // Dequeue should restart the turn
        let result = s.dequeue_and_start_turn().unwrap();
        assert!(result);
        assert_eq!(s.state(), SessionState::Active);
    }

    #[test]
    fn test_rollback_turn_requeue_preserves_parts() {
        // 带图片部件的回放输入在 rollback 后重新入队，部件必须原样保留。
        let parts = Content::Parts(vec![
            ContentPart::Text {
                text: "看这张图".to_string(),
            },
            ContentPart::Image {
                url: "https://example.com/cat.png".to_string(),
                detail: None,
            },
        ]);
        let mut s = make_session();
        s.start_turn(parts.clone()).unwrap();

        let _token = s.rollback_turn(true).unwrap();
        assert!(s.has_pending());
        s.dequeue_and_start_turn().unwrap();

        let staged = s.staging_user_input().unwrap();
        match staged.message.as_ref() {
            InputItem::Message { role, content } => {
                assert_eq!(*role, Role::User);
                assert_eq!(content, &parts);
            }
            _ => panic!("expected user message"),
        }
    }

    #[test]
    fn test_pending_enqueue_preserves_parts() {
        // Active 期间排队的带图输入在下一轮启动时，部件必须与 turn 启动、
        // rollback 重排队同样原样保留（三条入队路径对称）。
        let parts = Content::Parts(vec![
            ContentPart::Text {
                text: "排队带图输入".to_string(),
            },
            ContentPart::Image {
                url: "data:image/png;base64,aGVsbG8=".to_string(),
                detail: None,
            },
        ]);
        let mut s = make_session();
        s.start_turn("q1".into()).unwrap();
        s.enqueue_pending(parts.clone());
        assert!(s.has_pending());

        s.stage_item(MessageSource::ModelGeneration, assistant("a1"))
            .unwrap();
        let _token = s.commit_turn().unwrap();
        s.dequeue_and_start_turn().unwrap();

        let staged = s.staging_user_input().unwrap();
        match staged.message.as_ref() {
            InputItem::Message { role, content } => {
                assert_eq!(*role, Role::User);
                assert_eq!(content, &parts);
            }
            _ => panic!("expected user message"),
        }
    }

    #[test]
    fn test_pending_queue_flow() {
        let mut s = make_session();

        // Start a turn to make it active
        s.start_turn("q1".into()).unwrap();

        // Enqueue pending while active
        s.enqueue_pending("q2".into());
        s.enqueue_pending("q3".into());
        assert!(s.has_pending());

        // Complete current turn
        s.stage_item(MessageSource::ModelGeneration, assistant("a1"))
            .unwrap();
        let _token = s.commit_turn().unwrap();

        // 一次排空全部排队输入，启动合并后的新轮
        let result = s.dequeue_and_start_turn().unwrap();
        assert!(result);
        assert_eq!(s.state(), SessionState::Active);
        assert!(!s.has_pending());
        let ui = s.staging_user_input().unwrap();
        assert_eq!(ui.message.as_ref(), &user("---\nq2\n---\nq3"));
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
            s.start_turn(format!("q{i}").into()).unwrap();
            s.stage_item(MessageSource::ModelGeneration, assistant(format!("a{i}")))
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
    fn test_compact_evicts_front_and_pins_summary() {
        let mut s = make_session();

        for i in 0..4 {
            s.start_turn(format!("q{i}").into()).unwrap();
            s.stage_item(MessageSource::ModelGeneration, assistant(format!("a{i}")))
                .unwrap();
            let _ = s.commit_turn().unwrap();
        }

        let evicted = s.compact(2, "summary of turns 0-1".to_string()).unwrap();
        assert_eq!(evicted, 2);
        assert_eq!(s.committed_turns().len(), 2);
        assert_eq!(s.turn_index(), 4); // 计数不变（历史语义）

        // 剩余消息 turn_index 重编号闭合空洞
        let refs: Vec<_> = s.all_message_refs().collect();
        assert_eq!(refs.len(), 5); // 1 pinned + 2 turns × 2 messages
        assert!(matches!(
            refs[0].message.as_ref(),
            InputItem::Message {
                role: Role::System,
                content
            } if *content == Content::Text("summary of turns 0-1".to_string())
        ));
        assert_eq!(
            refs[0].source,
            MessageSource::SystemInjection {
                reason: "compaction".to_string()
            }
        );
        assert_eq!(refs[1].turn_index, 0);
        assert_eq!(refs[4].turn_index, 1);
    }

    #[test]
    fn test_compact_never_evicts_last_turn() {
        let mut s = make_session();
        s.start_turn("only".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, assistant("a"))
            .unwrap();
        let _ = s.commit_turn().unwrap();

        // 单轮：无可驱逐
        assert_eq!(s.compact(1, "s".to_string()).unwrap(), 0);
        assert!(s.pinned_summary().is_none());

        // 驱逐数超出：clamp 到 len-1 = 0
        s.start_turn("second".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, assistant("b"))
            .unwrap();
        let _ = s.commit_turn().unwrap();
        assert_eq!(s.compact(5, "s".to_string()).unwrap(), 1);
        assert_eq!(s.committed_turns().len(), 1);
    }

    #[test]
    fn test_compact_requires_idle() {
        let mut s = make_session();
        s.start_turn("q".into()).unwrap();
        assert!(s.compact(1, "s".to_string()).is_err());
    }

    #[test]
    fn test_pinned_summary_survives_snapshot_roundtrip() {
        let mut s = make_session();
        for i in 0..3 {
            s.start_turn(format!("q{i}").into()).unwrap();
            s.stage_item(MessageSource::ModelGeneration, assistant(format!("a{i}")))
                .unwrap();
            let _ = s.commit_turn().unwrap();
        }
        let _ = s.compact(1, "summary v1".to_string()).unwrap();

        let token = {
            // snapshot 需要令牌：Idle 状态下通过一次空 commit 不可行，
            // 直接用 rollback_turn 产生令牌（不改变已提交状态）
            s.rollback_turn(false).unwrap()
        };
        let snap = s.snapshot(&token);
        assert!(snap.pinned_summary.is_some());

        let mut restored = Session::from_snapshot(s.id().to_string(), "d".to_string(), 0, snap);
        assert_eq!(
            restored.pinned_summary().unwrap().message.as_ref(),
            &InputItem::Message {
                role: Role::System,
                content: "summary v1".into()
            }
        );
        assert_eq!(restored.committed_turns().len(), 2);
        // next_message_id 防御性兜底：恢复后新消息 id 不与 pinned 冲突
        restored.start_turn("new".into()).unwrap();
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

        // Turn with tool calls. In the neutral model the assistant's text preamble,
        // function call, and final answer are all separate items.
        s.start_turn("weather?".into()).unwrap();
        // Assistant preamble text — a real text item, IS displayable
        s.stage_item(MessageSource::ModelGeneration, assistant("Let me check..."))
            .unwrap();
        // Function call — NOT displayable
        s.stage_item(
            MessageSource::ModelGeneration,
            function_call("call_1", "get_weather"),
        )
        .unwrap();
        // Tool result — NOT displayable
        s.stage_item(
            MessageSource::ToolExecution {
                tool_name: "get_weather".to_string(),
            },
            tool("call_1", "Sunny, 25C"),
        )
        .unwrap();
        // Final assistant — IS displayable
        s.stage_item(
            MessageSource::ModelGeneration,
            assistant("Beijing is sunny, 25C"),
        )
        .unwrap();
        let _token = s.commit_turn().unwrap();

        let display: Vec<_> = s.display_message_refs().collect();
        // 3 displayable: user query + assistant preamble + final assistant reply.
        // FunctionCall / FunctionCallOutput / Reasoning are filtered out.
        assert_eq!(display.len(), 3);
        assert!(matches!(
            display[0].message.as_ref(),
            InputItem::Message {
                role: Role::User,
                ..
            }
        ));
        assert!(matches!(
            display[1].message.as_ref(),
            InputItem::Message {
                role: Role::Assistant,
                ..
            }
        ));
        assert!(matches!(
            display[2].message.as_ref(),
            InputItem::Message {
                role: Role::Assistant,
                ..
            }
        ));
    }

    #[test]
    fn test_cancel_active_turn() {
        let mut s = make_session();
        s.start_turn("hello".into()).unwrap();
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
        s.start_turn("q".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, assistant("a"))
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

    // ── interrupt_turn ─────────────────────────────────────────────────

    /// 从一条 `InterruptedTurn` 消息取出中断原因。
    fn interrupted_reason(am: &AnnotatedMessage) -> Option<&str> {
        match &am.source {
            MessageSource::InterruptedTurn { reason } => Some(reason.as_str()),
            _ => None,
        }
    }

    /// 抽出一轮的协议层条目，供 [`assert_wire_valid`] 校验。
    fn items(turn: &[AnnotatedMessage]) -> Vec<InputItem> {
        turn.iter().map(|am| am.message.as_ref().clone()).collect()
    }

    /// 取一条消息的纯文本；非 `Message` 条目（工具调用/结果）返回空串。
    fn text_of(am: &AnnotatedMessage) -> String {
        match am.message.as_ref() {
            InputItem::Message { content, .. } => content.text_view().into_owned(),
            _ => String::new(),
        }
    }

    #[test]
    fn test_interrupt_turn_patches_dangling_tool_calls() {
        // ① 悬空 c2：补齐必须按 called 原序，且插在中断说明之前
        let mut s = make_session();
        s.start_turn("do two things".into()).unwrap();
        // 轮首 preamble —— 顺序即线上顺序，故必须排在 FunctionCall 之前
        let partial = assistant("working on it");
        s.stage_item(MessageSource::ModelGeneration, partial.clone())
            .unwrap();
        s.stage_item(MessageSource::ModelGeneration, function_call("c1", "t1"))
            .unwrap();
        s.stage_item(MessageSource::ModelGeneration, function_call("c2", "t2"))
            .unwrap();
        s.stage_item(
            MessageSource::ToolExecution {
                tool_name: "t1".to_string(),
            },
            tool("c1", "done 1"),
        )
        .unwrap();

        s.interrupt_turn("cancelled").unwrap();

        assert_eq!(s.state(), SessionState::Idle);
        assert_eq!(s.turn_index(), 1);
        assert_eq!(s.committed_turns().len(), 1);

        let turn = &s.committed_turns()[0];
        let items: Vec<&InputItem> = turn.iter().map(|am| am.message.as_ref()).collect();

        // 顺序锚：user, partial, FC c1, FC c2, Output c1, Output c2(合成), notice
        assert_eq!(items.len(), 7);
        assert_eq!(items[0], &user("do two things"));
        assert_eq!(items[1], &partial);
        assert_eq!(items[2], &function_call("c1", "t1"));
        assert_eq!(items[3], &function_call("c2", "t2"));
        assert_eq!(items[4], &tool("c1", "done 1"));
        assert_eq!(items[5], &tool("c2", INTERRUPTED_TOOL_OUTPUT));
        assert!(matches!(
            items[6],
            InputItem::Message {
                role: Role::Assistant,
                ..
            }
        ));
        assert_eq!(
            interrupted_reason(&turn[6]),
            Some("cancelled"),
            "中断说明必须带 InterruptedTurn 标记"
        );
        // 合成项也带**真实原因**（不是工具输出文案）—— 消费方取任意一条
        // InterruptedTurn 都能拿到人话原因
        assert_eq!(interrupted_reason(&turn[5]), Some("cancelled"));

        // 每个 call_id 恰好应答一次
        for call_id in ["c1", "c2"] {
            let answers = turn
                .iter()
                .filter(|am| {
                    matches!(
                        am.message.as_ref(),
                        InputItem::FunctionCallOutput { call_id: c, .. } if c == call_id
                    )
                })
                .count();
            assert_eq!(answers, 1, "{call_id} must be answered exactly once");
        }

        // 末条是 assistant 的 notice（不是悬空工具结果）
        assert!(matches!(
            turn.last().unwrap().message.as_ref(),
            InputItem::Message {
                role: Role::Assistant,
                ..
            }
        ));

        assert_wire_valid(&items.into_iter().cloned().collect::<Vec<_>>());
    }

    #[test]
    fn test_interrupt_turn_degenerates_to_rollback() {
        // ② staging 只有 user_input → 无可保留产物，等同 rollback（不自增 turn_index）
        let mut s = make_session();
        s.start_turn("just a question".into()).unwrap();

        s.interrupt_turn("cancelled").unwrap();

        assert_eq!(s.state(), SessionState::Idle);
        assert!(s.committed_turns().is_empty());
        assert_eq!(s.turn_index(), 0);
        assert!(s.staging_user_input().is_none());
    }

    #[test]
    fn test_interrupt_turn_complete_tool_roundtrip_adds_no_output() {
        // ③ 完整往返：不新增 Output，只追加 InterruptedTurn 标记
        let mut s = make_session();
        s.start_turn("weather?".into()).unwrap();
        s.stage_item(
            MessageSource::ModelGeneration,
            function_call("c1", "get_weather"),
        )
        .unwrap();
        s.stage_item(
            MessageSource::ToolExecution {
                tool_name: "get_weather".to_string(),
            },
            tool("c1", "sunny"),
        )
        .unwrap();

        s.interrupt_turn("cancelled").unwrap();

        let turn = &s.committed_turns()[0];
        let outputs = turn
            .iter()
            .filter(|am| matches!(am.message.as_ref(), InputItem::FunctionCallOutput { .. }))
            .count();
        assert_eq!(outputs, 1, "完整往返不应新增合成 Output");
        assert!(matches!(
            turn[2].message.as_ref(),
            InputItem::FunctionCallOutput { call_id, .. } if call_id == "c1"
        ));
        assert!(turn.iter().any(|am| interrupted_reason(am).is_some()));
    }

    /// 收尾报告取代固定中断说明，且成为本轮唯一的一条收尾 assistant 消息。
    #[test]
    fn test_interrupt_turn_with_closing_replaces_notice() {
        let mut s = make_session();
        s.start_turn("long task".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, assistant("working"))
            .unwrap();
        s.stage_item(MessageSource::ModelGeneration, function_call("c1", "t1"))
            .unwrap();
        s.stage_item(
            MessageSource::ToolExecution {
                tool_name: "t1".to_string(),
            },
            tool("c1", "done"),
        )
        .unwrap();

        s.interrupt_turn_with_closing("max turns exceeded", Some("收尾报告正文"))
            .unwrap();

        let turn = &s.committed_turns()[0];
        // 末条是收尾报告，来源是 TurnEpilogue
        let last = turn.last().unwrap();
        assert!(matches!(
            last.message.as_ref(),
            InputItem::Message { role: Role::Assistant, content } if content.text_view() == "收尾报告正文"
        ));
        assert_eq!(last.source, MessageSource::TurnEpilogue);
        // 固定中断说明不得出现 —— 两条连续 assistant 会破坏历史合法性
        assert!(
            !turn.iter().any(|am| text_of(am).contains("[interrupted]")),
            "收尾存在时不得再追加固定中断说明"
        );
        assert_wire_valid(&items(turn));
    }

    /// `closing` 为 `None` 时与 `interrupt_turn` 逐字段一致。
    #[test]
    fn test_interrupt_turn_with_closing_none_matches_plain() {
        let build = || {
            let mut s = make_session();
            s.start_turn("q".into()).unwrap();
            s.stage_item(MessageSource::ModelGeneration, function_call("c1", "t1"))
                .unwrap();
            s.stage_item(
                MessageSource::ToolExecution {
                    tool_name: "t1".to_string(),
                },
                tool("c1", "r"),
            )
            .unwrap();
            s
        };

        let mut plain = build();
        plain.interrupt_turn("cancelled").unwrap();
        let mut with_none = build();
        with_none
            .interrupt_turn_with_closing("cancelled", None)
            .unwrap();

        assert_eq!(
            items(&plain.committed_turns()[0]),
            items(&with_none.committed_turns()[0])
        );
        assert_eq!(plain.turn_index(), with_none.turn_index());
    }

    /// 悬空 tool_call 的补齐必须先于收尾报告 —— 顺序颠倒会产出
    /// 「assistant(tool_calls) → assistant(text) → tool」的非法历史。
    #[test]
    fn test_interrupt_turn_with_closing_patches_before_closing() {
        let mut s = make_session();
        s.start_turn("do two things".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, function_call("c1", "t1"))
            .unwrap();
        s.stage_item(MessageSource::ModelGeneration, function_call("c2", "t2"))
            .unwrap();
        s.stage_item(
            MessageSource::ToolExecution {
                tool_name: "t1".to_string(),
            },
            tool("c1", "done 1"),
        )
        .unwrap();

        s.interrupt_turn_with_closing("max turns exceeded", Some("收尾"))
            .unwrap();

        let turn = &s.committed_turns()[0];
        // c2 的合成补齐夹在 tool 结果与收尾之间
        assert!(matches!(
            turn[turn.len() - 2].message.as_ref(),
            InputItem::FunctionCallOutput { call_id, .. } if call_id == "c2"
        ));
        assert!(matches!(
            turn.last().unwrap().message.as_ref(),
            InputItem::Message { content, .. } if content.text_view() == "收尾"
        ));
        // 补齐项仍带中断标记（消费方据此识别「这轮没跑完」）
        assert!(interrupted_reason(&turn[turn.len() - 2]).is_some());
        assert_wire_valid(&items(turn));
    }

    #[test]
    fn test_interrupt_turn_twice_on_idle_is_err() {
        // ④ Idle 上再次调用 → Err，且不产生第二个 turn
        let mut s = make_session();
        s.start_turn("q".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, function_call("c1", "t1"))
            .unwrap();
        s.stage_item(
            MessageSource::ToolExecution {
                tool_name: "t1".to_string(),
            },
            tool("c1", "r"),
        )
        .unwrap();
        s.interrupt_turn("cancelled").unwrap();

        let turns_before = s.committed_turns().len();
        let index_before = s.turn_index();

        let err = s.interrupt_turn("again").unwrap_err();
        assert!(matches!(
            err,
            SessionError::InvalidStateTransition { action, .. } if action == "interrupt_turn"
        ));
        assert_eq!(s.committed_turns().len(), turns_before);
        assert_eq!(s.turn_index(), index_before);
    }

    // ── staging 回退锚点（截断重试用）──────────────────────────────────

    #[test]
    fn test_staging_checkpoint_roundtrip() {
        let mut s = make_session();
        s.start_turn("q".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, assistant("a1"))
            .unwrap();
        s.stage_item(MessageSource::ModelGeneration, assistant("a2"))
            .unwrap();

        // 锚点在发请求前取：此刻 staging 里是「上一批落地结果」
        let cp = s.staging_checkpoint();
        assert_eq!(cp, 2);

        // 本次模型调用的产出追加在后面
        s.stage_item(MessageSource::ModelGeneration, assistant("truncated-1"))
            .unwrap();
        s.stage_item(MessageSource::ModelGeneration, assistant("truncated-2"))
            .unwrap();
        assert_eq!(s.staging_checkpoint(), 4);

        // 回退到锚点：丢弃的正是本次调用的两条
        assert_eq!(s.truncate_staging(cp).unwrap(), 2);
        assert_eq!(s.staging_checkpoint(), cp);
        assert_eq!(s.state(), SessionState::Active);
        // user_input 不受影响，本轮仍在进行
        assert!(s.staging_user_input().is_some());
    }

    #[test]
    fn test_truncate_staging_when_idle_fails() {
        let mut s = make_session();
        let err = s.truncate_staging(0).unwrap_err();
        assert!(matches!(
            err,
            SessionError::InvalidStateTransition { action, .. } if action == "truncate_staging"
        ));
    }

    #[test]
    fn test_truncate_staging_out_of_bounds_errors() {
        let mut s = make_session();
        s.start_turn("q".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, assistant("a"))
            .unwrap();

        // 锚点大于当前条数 → 显式失败，不静默饱和
        let err = s.truncate_staging(99).unwrap_err();
        assert!(matches!(
            err,
            SessionError::StagingCheckpointOutOfBounds {
                requested: 99,
                current: 1,
            }
        ));
        // 失败不动 staging
        assert_eq!(s.staging_checkpoint(), 1);
    }

    #[test]
    fn test_truncate_staging_does_not_rewind_next_message_id() {
        // 显式契约：回退只丢消息，不回退 ID 计数器 ——
        // 复用已流出（LooperEvent / InflightCheckpoint）的 ID 会让它们指向两条消息。
        let mut s = make_session();
        s.start_turn("q".into()).unwrap();
        let first = s
            .stage_item(MessageSource::ModelGeneration, assistant("a1"))
            .unwrap();

        let cp = s.staging_checkpoint();
        let discarded = s
            .stage_item(MessageSource::ModelGeneration, assistant("discarded"))
            .unwrap();
        s.truncate_staging(cp).unwrap();

        let after = s
            .stage_item(MessageSource::ModelGeneration, assistant("a2"))
            .unwrap();
        assert!(after.0 > discarded.0, "回退后新 ID 必须仍单调递增");
        assert!(discarded.0 > first.0);
    }

    #[test]
    fn test_truncate_staging_excluded_from_snapshot() {
        // 被回退的消息不进历史：commit 后的 committed 与快照往返都不含它们。
        let mut s = make_session();
        s.start_turn("q".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, assistant("kept"))
            .unwrap();

        let cp = s.staging_checkpoint();
        s.stage_item(MessageSource::ModelGeneration, assistant("dropped"))
            .unwrap();
        s.truncate_staging(cp).unwrap();

        let token = s.commit_turn().unwrap();
        let snap = s.snapshot(&token);
        let json = serde_json::to_string(&snap).unwrap();
        let back: SessionSnapshot = serde_json::from_str(&json).unwrap();

        assert!(!json.contains("dropped"), "被回退的消息不应出现在快照中");
        let restored =
            Session::from_snapshot(s.id().to_string(), "d".to_string(), s.created_at(), back);
        assert_eq!(restored.committed_turns().len(), 1);
        assert!(
            restored
                .committed_turns()
                .iter()
                .flatten()
                .any(|am| matches!(am.message.as_ref(), InputItem::Message { content, .. } if content.text_view() == "kept"))
        );
    }

    #[test]
    fn test_interrupt_turn_survives_snapshot_roundtrip() {
        // ⑤ 快照往返：serde 新变体 + from_snapshot 规范化一次性覆盖
        let mut s = make_session();
        s.start_turn("q".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, function_call("c1", "t1"))
            .unwrap();
        s.stage_item(
            MessageSource::ToolExecution {
                tool_name: "t1".to_string(),
            },
            tool("c1", "r"),
        )
        .unwrap();
        let token = s.interrupt_turn("hook abort").unwrap();
        let snap = s.snapshot(&token);

        let json = serde_json::to_string(&snap).unwrap();
        let back: SessionSnapshot = serde_json::from_str(&json).unwrap();
        let restored =
            Session::from_snapshot(s.id().to_string(), "d".to_string(), s.created_at(), back);

        assert_eq!(restored.turn_index(), 1);
        let turn = &restored.committed_turns()[0];
        assert_eq!(
            interrupted_reason(turn.last().unwrap()),
            Some("hook abort"),
            "快照往返后 InterruptedTurn 标记必须原样重建"
        );
        assert_eq!(
            &turn[2].message.as_ref().clone(),
            &tool("c1", "r"),
            "非合成项内容不变"
        );
    }

    // ── hydrate_inflight ───────────────────────────────────────────────

    /// 造一份检查点内容：`[user, FC c1, Output c1]`，与 `staging_all()` 同序。
    fn checkpoint_messages() -> Vec<AnnotatedMessage> {
        let mut s = make_session();
        s.start_turn("跑个长任务".into()).unwrap();
        s.stage_item(MessageSource::ModelGeneration, function_call("c1", "t1"))
            .unwrap();
        s.stage_item(
            MessageSource::ToolExecution {
                tool_name: "t1".to_string(),
            },
            tool("c1", "done 1"),
        )
        .unwrap();
        s.staging_all()
    }

    #[test]
    fn test_hydrate_inflight_then_freeze() {
        // 冷启动路径：from_snapshot 恒 Idle + 空 staging，检查点靠 hydrate 复原
        let mut s = Session::from_snapshot(
            "test-id".to_string(),
            "d".to_string(),
            1,
            SessionSnapshot {
                committed_turns: Vec::new(),
                turn_index: 0,
                total_usage: Usage::default(),
                next_message_id: 0,
                pending_inputs: Vec::new(),
                pinned_summary: None,
            },
        );
        assert!(s.is_idle());

        let staged = checkpoint_messages();
        assert_eq!(s.hydrate_inflight(staged).unwrap(), 3);
        assert_eq!(s.state(), SessionState::Active);
        assert_eq!(
            s.staging_user_input().map(|am| am.message.as_ref()),
            Some(&user("跑个长任务"))
        );
        assert_eq!(s.staging_messages().len(), 2);

        let _token = s.interrupt_turn("crashed").unwrap();
        assert_eq!(s.state(), SessionState::Idle);
        assert_eq!(s.turn_index(), 1);

        let turn = &s.committed_turns()[0];
        let items: Vec<&InputItem> = turn.iter().map(|am| am.message.as_ref()).collect();
        // user, FC c1, Output c1, notice
        assert_eq!(items.len(), 4);
        assert_eq!(items[1], &function_call("c1", "t1"));
        assert_eq!(items[2], &tool("c1", "done 1"));
        assert_eq!(interrupted_reason(turn.last().unwrap()), Some("crashed"));
        assert_wire_valid(&items.into_iter().cloned().collect::<Vec<_>>());
    }

    #[test]
    fn test_hydrate_inflight_patches_dangling_call() {
        // 检查点停在「工具还没回来」的时刻：水化后冻结必须补齐悬空调用
        let mut s = make_session();
        let staged = {
            let mut src = make_session();
            src.start_turn("q".into()).unwrap();
            src.stage_item(MessageSource::ModelGeneration, function_call("c1", "t1"))
                .unwrap();
            src.staging_all()
        };

        s.hydrate_inflight(staged).unwrap();
        s.interrupt_turn("crashed").unwrap();

        let turn = &s.committed_turns()[0];
        let items: Vec<InputItem> = turn.iter().map(|am| am.message.as_ref().clone()).collect();
        assert_wire_valid(&items);
        assert_eq!(items[2], tool("c1", INTERRUPTED_TOOL_OUTPUT));
    }

    #[test]
    fn test_hydrate_inflight_advances_message_id() {
        // 恢复的消息带着崩溃进程的消息 ID — 计数器必须越过它们，
        // 否则后续 stage_item 会分配出重复 ID。
        let mut s = make_session();
        let staged = checkpoint_messages(); // id 0..2
        s.hydrate_inflight(staged).unwrap();

        let id = s
            .stage_item(MessageSource::ModelGeneration, assistant("继续"))
            .unwrap();
        let ids: Vec<u64> = s.staging_all().iter().map(|am| am.id.0).collect();
        assert!(
            !ids[..ids.len() - 1].contains(&id.0),
            "新分配的消息 ID 与恢复的 ID 冲突: {ids:?}"
        );
    }

    #[test]
    fn test_hydrate_inflight_empty_and_non_idle() {
        let mut s = make_session();
        assert_eq!(s.hydrate_inflight(Vec::new()).unwrap(), 0);
        assert!(s.is_idle(), "空检查点不得改变状态");

        // 非 Idle 拒绝：与 from_snapshot 的「恒 Idle」前提绑定
        s.start_turn("q".into()).unwrap();
        assert!(s.hydrate_inflight(checkpoint_messages()).is_err());
    }

    #[test]
    fn test_hydrate_inflight_tolerates_non_user_first() {
        // 首条不是 user 消息（检查点损坏）——按普通消息入 staging，不 panic
        let mut s = make_session();
        let staged = vec![AnnotatedMessage::new(
            MessageId(7),
            0,
            assistant("孤儿回复"),
            MessageSource::ModelGeneration,
        )];
        assert_eq!(s.hydrate_inflight(staged).unwrap(), 1);
        assert!(s.staging_user_input().is_none());
        assert_eq!(s.staging_messages().len(), 1);
    }

    #[test]
    fn test_dequeue_drains_all_pending() {
        let mut s = make_session();

        s.enqueue_pending("normal".into());
        s.enqueue_pending("interrupt".into());
        s.enqueue_pending("later".into());

        // 一次排空全部排队输入，合并为一条 user 消息
        let result = s.dequeue_and_start_turn().unwrap();
        assert!(result);
        assert!(!s.has_pending());

        let ui = s.staging_user_input().unwrap();
        assert_eq!(
            ui.message.as_ref(),
            &user("---\nnormal\n---\ninterrupt\n---\nlater")
        );
    }

    #[test]
    fn test_dequeue_single_pending_is_identity() {
        // 单条排队输入原样出队，不加任何分隔标记
        let mut s = make_session();
        s.enqueue_pending("only one".into());

        s.dequeue_and_start_turn().unwrap();
        let ui = s.staging_user_input().unwrap();
        assert_eq!(ui.message.as_ref(), &user("only one"));
    }

    #[test]
    fn test_dequeue_merge_preserves_images() {
        // 多条合并时图片部件不丢，分隔标记以 Text 部件插入，
        // 且相邻文本部件裸拼接（wire 形态）与纯文本分支逐字一致
        let with_image = Content::Parts(vec![
            ContentPart::Text {
                text: "看这张图".to_string(),
            },
            ContentPart::Image {
                url: "https://example.com/cat.png".to_string(),
                detail: None,
            },
        ]);
        let mut s = make_session();
        s.start_turn("q1".into()).unwrap();
        s.enqueue_pending(with_image);
        s.enqueue_pending("q2".into());
        s.commit_turn().unwrap();

        s.dequeue_and_start_turn().unwrap();
        let ui = s.staging_user_input().unwrap();
        match ui.message.as_ref() {
            InputItem::Message { role, content } => {
                assert_eq!(*role, Role::User);
                match content {
                    Content::Parts(parts) => {
                        assert!(
                            parts.iter().any(|p| matches!(p, ContentPart::Image { .. })),
                            "图片部件必须保留"
                        );
                        let texts: Vec<&str> = parts
                            .iter()
                            .filter_map(|p| match p {
                                ContentPart::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect();
                        assert_eq!(
                            texts,
                            vec!["---\n", "看这张图", "\n", "---\n", "q2"],
                            "标记部件自带换行、条目间补 \n 部件"
                        );
                        // wire 形态：相邻 Text 部件直传拼接（chat_common 无分隔）
                        let wire: String = parts
                            .iter()
                            .filter_map(|p| match p {
                                ContentPart::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect();
                        assert_eq!(
                            wire, "---\n看这张图\n---\nq2",
                            "含图分支 wire 文本必须与纯文本分支一致"
                        );
                    }
                    _ => panic!("含图合并必须是 Parts 形态"),
                }
            }
            _ => panic!("expected user message"),
        }
    }

    #[test]
    fn test_dequeue_marks_merged_source() {
        // 批量合并出队的 user 消息标记 MergedPending（展示层据此剥离标记）；
        // 直发 start_turn 保持 UserInput
        let mut s = make_session();
        s.enqueue_pending("q1".into());
        s.enqueue_pending("q2".into());
        s.dequeue_and_start_turn().unwrap();
        let ui = s.staging_user_input().unwrap();
        assert_eq!(ui.source, MessageSource::MergedPending);

        let mut s2 = make_session();
        s2.start_turn("direct".into()).unwrap();
        let ui2 = s2.staging_user_input().unwrap();
        assert_eq!(ui2.source, MessageSource::UserInput);
    }

    #[test]
    fn test_strip_merge_markers() {
        use crate::session::types::strip_merge_markers;

        // 纯文本合并形态：标记行删除、其余原样
        assert_eq!(
            strip_merge_markers(&Content::Text("---\nA\n---\nB".into())),
            "A\nB"
        );
        // 含图 text_view 形态（标记间多空行）：折叠 + trim 归一
        assert_eq!(
            strip_merge_markers(&Content::Text("---\n\n看这张图\n\n\n---\n\nq2".into())),
            "看这张图\nq2"
        );
        // 消息内部的空行段落保留（最多一个连续空行）
        assert_eq!(
            strip_merge_markers(&Content::Text("---\np1\n\np2\n---\nB".into())),
            "p1\n\np2\nB"
        );
    }

    #[test]
    fn test_dequeue_start_turn_failure_requeues_batch_in_order() {
        // start_turn 失败（非 Idle）时整批按原序回队
        let mut s = make_session();
        s.start_turn("active".into()).unwrap(); // 使 session 进入 Active
        s.enqueue_pending("q1".into());
        s.enqueue_pending("q2".into());

        assert!(s.dequeue_and_start_turn().is_err());
        assert!(s.has_pending());

        // 回到 Idle 后出队，顺序仍是 q1 → q2
        let _ = s.rollback_turn(false).unwrap();
        s.dequeue_and_start_turn().unwrap();
        let ui = s.staging_user_input().unwrap();
        assert_eq!(ui.message.as_ref(), &user("---\nq1\n---\nq2"));
    }

    /// 来源按批大小定：单条 `UserInput`、多条 `MergedPending`（理由见
    /// [`Session::dequeue_and_start_turn`]）。
    #[test]
    fn test_dequeue_source_depends_on_batch_size() {
        let mut s = make_session();
        s.enqueue_pending("only".into());
        assert!(s.dequeue_and_start_turn().unwrap());
        assert_eq!(
            s.staging_user_input().unwrap().source,
            MessageSource::UserInput,
            "单条出队必须是 UserInput，否则展示层会剥用户自己的 --- 行"
        );

        let mut s = make_session();
        let _ = s.rollback_turn(false).unwrap();
        s.enqueue_pending("a".into());
        s.enqueue_pending("b".into());
        assert!(s.dequeue_and_start_turn().unwrap());
        assert_eq!(
            s.staging_user_input().unwrap().source,
            MessageSource::MergedPending,
            "多条合并才是 MergedPending"
        );
    }
}
