use super::*;
use std::io::Write;
use std::sync::atomic::AtomicUsize;
use tracing_subscriber::fmt::MakeWriter;

// ── LooperConfig tests ────────────────────────────────────────────

#[test]
fn test_looper_config_default() {
    let config = LooperConfig::default();
    assert_eq!(config.event_buffer, 256);
    assert!(config.hooks.is_empty());
    assert!(config.environment.is_none());
    assert_eq!(config.retry_limit, 3);
    assert_eq!(config.retry_output_budget, 32_768);
    assert_eq!(config.retry_base_delay_ms, 500);
    assert_eq!(config.retry_max_delay_ms, 5_000);
}

// ── prompt 组装纯函数 tests ────────────────────────────────────────

#[test]
fn test_compose_stable_prefix_none() {
    assert_eq!(compose_stable_prefix("SYS", None), "SYS");
}

#[test]
fn test_compose_stable_prefix_some() {
    assert_eq!(
        compose_stable_prefix("SYS", Some("<environment>x</environment>")),
        "SYS\n\n<environment>x</environment>"
    );
}

#[test]
fn test_compose_stable_prefix_empty_env() {
    // 空串必须等同 None——否则追加尾随 "\n\n" 破坏字节一致
    assert_eq!(compose_stable_prefix("SYS", Some("")), "SYS");
}

#[test]
fn test_compose_effective_prompt_none() {
    assert_eq!(compose_effective_prompt("PREFIX", None), "PREFIX");
}

#[test]
fn test_compose_effective_prompt_some() {
    assert_eq!(
        compose_effective_prompt("PREFIX", Some("ctx")),
        "PREFIX\n\n[Dynamic Context]\nctx"
    );
}

#[test]
fn test_compose_roundtrip_compat() {
    // 与旧行为字节等价的回归锚：无 environment、无 dynamic context 时
    // effective prompt == agent.system_prompt()
    let stable = compose_stable_prefix("SYS", None);
    assert_eq!(compose_effective_prompt(&stable, None), "SYS");
}

// ── drain_finished_join_results tests ──────────────────────────────

/// 构造 `pending` 槽位（`call` 与 `result` 均占位，索引与 spawn 时的 idx 对齐）。
fn pending_slots(n: usize) -> Vec<PendingToolCall> {
    (0..n)
        .map(|i| PendingToolCall {
            call: Arc::new(ToolCall::new(format!("c{i}"), "t", "{}")),
            result: None,
        })
        .collect()
}

/// 产物声明必须在**构造**时就被剥离 —— 这是「工具输出」与
/// 「会话历史」的分界。若退化到调用点各自剥离，漏一处就会让
/// `@@PECO_ARTIFACT` 行进模型上下文。
#[test]
fn tool_call_result_strips_artifact_declarations() {
    let call = Arc::new(ToolCall::new("c1", "shell", "{}"));
    let r = ToolCallResult::new(
        call,
        Content::Text(
            "生成完成\n@@PECO_ARTIFACT {\"path\":\"/a/b.jpg\"}\n共 1 个文件\n".to_string(),
        ),
        false,
    );
    let text = r.result.text_view().into_owned();
    assert!(!text.contains("@@PECO_ARTIFACT"), "leaked: {text}");
    assert_eq!(text, "生成完成\n共 1 个文件\n");
    assert_eq!(r.artifacts.len(), 1);
    assert_eq!(r.artifacts[0].path, "/a/b.jpg");
}

/// 错误结果同样走剥离（错误文本也可能被工具拼上声明）。
#[test]
fn tool_call_result_strips_on_error_path() {
    let call = Arc::new(ToolCall::new("c1", "shell", "{}"));
    let r = ToolCallResult::new(
        call,
        Content::Text("boom\n@@PECO_ARTIFACT {\"path\":\"/a/c.mp4\"}\n".to_string()),
        true,
    );
    assert!(r.is_error);
    assert_eq!(r.result.text_view().into_owned(), "boom\n");
    assert_eq!(r.artifacts.len(), 1);
}

fn tool_result(idx: usize) -> (usize, ToolCallResult) {
    let call = Arc::new(ToolCall::new(format!("c{idx}"), "t", "{}"));
    (
        idx,
        ToolCallResult::new(call, Content::Text(format!("r{idx}")), false),
    )
}

#[tokio::test]
async fn test_drain_collects_all_finished_tasks() {
    let mut joinset: tokio::task::JoinSet<(usize, ToolCallResult)> = tokio::task::JoinSet::new();
    joinset.spawn(async { tool_result(0) });
    joinset.spawn(async { tool_result(1) });
    // 永不完成 —— 必须不被伪造结果
    joinset.spawn(async {
        std::future::pending::<()>().await;
        tool_result(2)
    });

    tokio::time::sleep(Duration::from_millis(30)).await;

    let mut pending = pending_slots(3);
    let n = AgentLooper::drain_finished_join_results(&mut joinset, &mut pending);

    assert_eq!(n, 2);
    assert!(pending[0].result.is_some());
    assert!(pending[1].result.is_some());
    assert!(pending[2].result.is_none(), "在途 task 不得被伪造结果");
}

#[tokio::test]
async fn test_drain_survives_panicking_sibling() {
    // 回归锚：`while let Some(Ok(..))` 写法会让 panic 的 task 提前终止排空，
    // 排在它后面的正常 task 成果被吞掉。
    let mut joinset: tokio::task::JoinSet<(usize, ToolCallResult)> = tokio::task::JoinSet::new();
    joinset.spawn(async { panic!("boom") });
    joinset.spawn(async { tool_result(1) });

    tokio::time::sleep(Duration::from_millis(30)).await;

    let mut pending = pending_slots(2);
    let n = AgentLooper::drain_finished_join_results(&mut joinset, &mut pending);

    assert_eq!(n, 1, "panic 的兄弟 task 不得吞掉正常 task 的成果");
    assert!(pending[0].result.is_none(), "panic 的 task 不应有结果");
    assert!(pending[1].result.is_some(), "正常 task 的成果必须被捞回");
}

// ── plan_failure tests（零脚手架：真实 Session，无 Agent / provider）──

fn plan_session() -> Session {
    Session::new("plan-test".to_string(), "d".to_string())
}

fn plan_ctx_with_text(text: &str) -> ReActContext {
    ReActContext {
        assistant_text: text.to_string(),
        ..Default::default()
    }
}

#[test]
fn test_plan_failure_freezes_dangling_tool_call() {
    let mut session = plan_session();
    session.start_turn("do it".into()).unwrap();
    session
        .stage_item(
            MessageSource::ModelGeneration,
            InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "t1".to_string(),
                arguments: "{}".to_string(),
            },
        )
        .unwrap();

    let mut ctx = plan_ctx_with_text("partial");
    let turn_before = session.turn_index();

    let plan = plan_failure(
        &mut session,
        &mut ctx,
        true,
        TurnFailureReason::Cancelled,
        false,
        None,
    )
    .expect("in-flight turn must produce a plan");

    // turn 必须等于冻结**前**的 index（interrupt_turn 后会 +1）
    assert_eq!(plan.turn_index, turn_before);
    assert_eq!(session.turn_index(), turn_before + 1);
    assert_eq!(session.committed_turns().len(), 1);
    assert!(plan.snapshot.is_some(), "persist=true 必须产出快照");
    assert_eq!(plan.partial_text_len, "partial".len());
    // 冻结的轮里必须有合成的 Output，否则历史对 provider 非法
    let turn = &session.committed_turns()[0];
    assert!(turn.iter().any(|am| matches!(
        am.message.as_ref(),
        InputItem::FunctionCallOutput { call_id, .. } if call_id == "c1"
    )));
    // partial_text 被取走，looper 状态被清
    assert!(ctx.assistant_text.is_empty());
    assert!(ctx.pending_tool_calls.is_empty());
}

/// C2 的 drain 捞回的结果必须由 `plan_failure` 写进 staging，随本轮一起冻结 ——
/// 这是「取消时已完成的工具成果不丢」的最后一环。
#[test]
fn test_plan_failure_stages_salvaged_tool_results() {
    let mut session = plan_session();
    session.start_turn("do it".into()).unwrap();
    session
        .stage_item(
            MessageSource::ModelGeneration,
            InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "t1".to_string(),
                arguments: "{}".to_string(),
            },
        )
        .unwrap();

    let call = Arc::new(ToolCall::new("c1", "t1", "{}"));
    let mut ctx = ReActContext {
        pending_tool_calls: vec![PendingToolCall {
            call: Arc::clone(&call),
            result: Some(ToolCallResult::new(
                call,
                Content::Text("salvaged".to_string()),
                false,
            )),
        }],
        ..Default::default()
    };

    let plan = plan_failure(
        &mut session,
        &mut ctx,
        true,
        TurnFailureReason::Cancelled,
        false,
        None,
    )
    .expect("有在途轮");

    // 计数取的是冲刷**前**的 staging 规模（悬空的 FunctionCall 计 1 条）
    assert_eq!(plan.frozen_staging_messages, 1);

    let turn = &session.committed_turns()[0];
    assert!(
        turn.iter().any(|am| matches!(
            am.message.as_ref(),
            InputItem::FunctionCallOutput { call_id, output }
                if call_id == "c1" && matches!(output, Content::Text(t) if t == "salvaged")
        )),
        "捞回的工具结果必须随本轮冻进 committed"
    );
    assert!(ctx.pending_tool_calls.is_empty(), "ctx 必须清空，防重放");
}

/// 取消时模型还没吐字：必须补桩冻结，**不能**退化成 rollback —— 退化路径会
/// 把这一轮（含用户提问）整轮丢掉，用户按停后重载页面会发现自己那条消息凭空
/// 消失。取消是主停止手段，这条必须锁住。
#[test]
fn test_plan_failure_cancel_before_output_freezes_with_stub() {
    let mut session = plan_session();
    session.start_turn("hi".into()).unwrap();
    let mut ctx = ReActContext::default();

    let plan = plan_failure(
        &mut session,
        &mut ctx,
        true,
        TurnFailureReason::Cancelled,
        false,
        None,
    )
    .expect("Active with only user_input still has a turn to close");

    assert_eq!(plan.turn_index, 0);
    assert_eq!(session.committed_turns().len(), 1, "取消必须冻结而非丢弃");
    assert_eq!(session.turn_index(), 1);
    assert_eq!(session.state(), SessionState::Idle);
    let turn = &session.committed_turns()[0];
    assert!(
        turn.iter().any(|am| matches!(
            am.message.as_ref(),
            InputItem::Message { role: Role::Assistant, content }
                if matches!(content, Content::Text(t) if t == "[cancelled before response]")
        )),
        "必须补存活桩，否则 interrupt_turn 退化成 rollback，用户提问丢失"
    );
    assert!(
        turn.iter().any(|am| matches!(
            am.message.as_ref(),
            InputItem::Message { role: Role::User, content }
                if matches!(content, Content::Text(t) if t == "hi")
        )),
        "用户提问必须留在历史里"
    );
}

/// 非取消、零产出、无重发 → 仍走退化 rollback（守住该路径不被误删）。
#[test]
fn test_plan_failure_non_cancel_only_user_input_degenerates() {
    let mut session = plan_session();
    session.start_turn("hi".into()).unwrap();
    let mut ctx = ReActContext::default();

    let plan = plan_failure(
        &mut session,
        &mut ctx,
        true,
        TurnFailureReason::MaxIterationsExceeded,
        false,
        None,
    )
    .expect("Active with only user_input still has a turn to close");

    assert_eq!(plan.turn_index, 0);
    assert!(session.committed_turns().is_empty(), "退化路径=rollback");
    assert_eq!(session.turn_index(), 0, "退化路径不自增");
    assert_eq!(session.state(), SessionState::Idle);
}

#[test]
fn test_plan_failure_returns_none_on_idle() {
    // 无在途轮 → 不发语义为空的 TurnComplete
    let mut session = plan_session();
    let mut ctx = ReActContext::default();

    assert!(
        plan_failure(
            &mut session,
            &mut ctx,
            true,
            TurnFailureReason::Cancelled,
            false,
            None,
        )
        .is_none()
    );
}

#[test]
fn test_plan_failure_persist_false_skips_snapshot_only() {
    let mut session = plan_session();
    session.start_turn("q".into()).unwrap();
    session
        .stage_item(
            MessageSource::ModelGeneration,
            InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "t1".to_string(),
                arguments: "{}".to_string(),
            },
        )
        .unwrap();
    let mut ctx = ReActContext::default();

    let plan = plan_failure(
        &mut session,
        &mut ctx,
        false,
        TurnFailureReason::MaxIterationsExceeded,
        false,
        None,
    )
    .unwrap();

    // CLI 路径：冻结照常（内存），只是不落盘 —— committed 有这一轮、快照为空
    assert!(plan.snapshot.is_none());
    assert_eq!(session.committed_turns().len(), 1);
}

#[test]
fn test_failure_label_covers_all_variants_without_debug_leak() {
    let variants = [
        TurnFailureReason::Cancelled,
        TurnFailureReason::MaxIterationsExceeded,
        TurnFailureReason::HookAbort("hook says no".to_string()),
        TurnFailureReason::Other("boom".to_string()),
    ];
    for reason in &variants {
        let label = failure_label(reason);
        assert!(!label.is_empty(), "{reason:?} produced an empty label");
        // Debug 泄漏会把 `HookAbort("x")` 原样送进模型上下文
        assert!(
            !label.contains("Some("),
            "{reason:?} leaked Debug formatting into {label:?}"
        );
    }
}

// ── looper 级收尾测试 ──────────────────────────────────────────────

/// 计数 persister —— `NullSessionPersister` 是 no-op，看不出有没有被调用。
#[derive(Default)]
struct CountingPersister {
    saves: AtomicUsize,
}

#[async_trait::async_trait]
impl crate::persistence::SessionPersister for CountingPersister {
    async fn save(
        &self,
        _snapshot: &SessionSnapshot,
        _session_id: &str,
        _description: &str,
        _created_at: u64,
    ) -> Result<crate::persistence::PersistResult, crate::persistence::PersistError> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        Ok(crate::persistence::PersistResult {
            bytes_written: 0,
            path: std::path::PathBuf::new(),
        })
    }
    async fn load(
        &self,
        _session_id: &str,
    ) -> Result<Option<(SessionSnapshot, crate::SessionMeta)>, crate::persistence::PersistError>
    {
        Ok(None)
    }
    async fn delete(&self, _session_id: &str) -> Result<(), crate::persistence::PersistError> {
        Ok(())
    }
    async fn list(&self) -> Result<Vec<crate::SessionMeta>, crate::persistence::PersistError> {
        Ok(Vec::new())
    }
}

/// 失败路径永不触达模型 —— 所有方法 `unimplemented!()`。
struct PanicProvider;

#[async_trait::async_trait]
impl model_provider::ModelProvider for PanicProvider {
    fn name(&self) -> &str {
        "panic-provider"
    }
    async fn generate_full(
        &self,
        _request: &model_provider::GenerateRequest,
    ) -> Result<model_provider::GenerateResult, model_provider::ProviderError> {
        unimplemented!("failure path must not reach the model")
    }
    async fn generate_stream(
        &self,
        _request: &model_provider::GenerateRequest,
    ) -> Result<model_provider::GenerateStream, model_provider::ProviderError> {
        unimplemented!("failure path must not reach the model")
    }
}

/// 失败收尾测试的 looper 及其通道端点。
///
/// 端点都是字段而非返回值的一部分，因为**必须保活**：
/// `user_speaker` 一 drop，`run()` 就认为输入 channel 已关闭而提前退出；
/// `events` 一 drop，`emit_event_guaranteed` 送不出去。
struct FailureHarness {
    looper: AgentLooper,
    /// 只作为「输入 channel 未关闭」的保活句柄存在，本身从不被读。
    _user_speaker: Speaker<UserMsg>,
    /// `run()` 取所有权，故用 `Option` 让调用点能把它移出去。
    user_listener: Option<Listener<UserMsg>>,
    events: Listener<LooperEvent>,
}

/// 构造失败收尾测试用的 looper：`PanicProvider` + 计数 persister
/// （失败路径永不触达模型），会话预置一个悬空 `FunctionCall`，
/// 使收尾走冻结路径而非退化 rollback。
fn failure_looper_harness(
    cancel: bool,
    persist_on_failure: bool,
    pending_input: bool,
) -> FailureHarness {
    failure_looper_harness_with_epilogue(cancel, persist_on_failure, pending_input, None)
}

/// 取一条消息的纯文本；非 `Message` 条目（工具调用/结果）返回空串。
fn text_of(am: &crate::session::AnnotatedMessage) -> String {
    match am.message.as_ref() {
        InputItem::Message { content, .. } => content.text_view().into_owned(),
        _ => String::new(),
    }
}

/// 固定输出的假收尾器 —— 直接实现元任务接口，不走 Agent 的 provider。
struct MockEpiloguer(&'static str);

#[async_trait::async_trait]
impl super::super::compaction::TurnSummarizer for MockEpiloguer {
    async fn summarize(
        &self,
        _previous: Option<&str>,
        _evicted: &str,
    ) -> Result<String, AgentError> {
        unimplemented!("failure harness never compacts")
    }

    async fn summarize_inflight(&self, _transcript: &str) -> Result<String, AgentError> {
        Ok(self.0.to_string())
    }
}

/// 必然失败的收尾器 —— 验证回退到固定中断说明。
struct FailingEpiloguer;

#[async_trait::async_trait]
impl super::super::compaction::TurnSummarizer for FailingEpiloguer {
    async fn summarize(
        &self,
        _previous: Option<&str>,
        _evicted: &str,
    ) -> Result<String, AgentError> {
        unimplemented!("failure harness never compacts")
    }

    async fn summarize_inflight(&self, _transcript: &str) -> Result<String, AgentError> {
        Err(AgentError::Compaction("boom".into()))
    }
}

fn failure_looper_harness_with_epilogue(
    cancel: bool,
    persist_on_failure: bool,
    pending_input: bool,
    epilogue: Option<Arc<dyn super::super::compaction::TurnSummarizer>>,
) -> FailureHarness {
    let profile: crate::agent::AgentProfile = serde_yaml::from_str(
        "agent:\n  name: t\n  description: d\nllm:\n  provider: p\n  model: m\n",
    )
    .unwrap();
    let executor: Arc<dyn crate::tools::ToolExecutor> =
        Arc::new(crate::tools::DefaultToolsExecutor::new(Vec::new()));
    let agent = Arc::new(Agent::from_parts(
        std::path::PathBuf::from("/tmp/agent.md"),
        profile,
        "sys".to_string(),
        Arc::new(PanicProvider),
        crate::agent::ModelConfigBuilder::new().build(),
        Arc::clone(&executor),
        Arc::new(crate::mcp::McpManager::empty(executor)),
        None,
    ));

    let mut session = Session::new("s1".to_string(), "d".to_string());
    session.start_turn("q".into()).unwrap();
    session
        .stage_item(
            MessageSource::ModelGeneration,
            InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "t1".to_string(),
                arguments: "{}".to_string(),
            },
        )
        .unwrap();
    if pending_input {
        session.enqueue_pending("queued".into());
    }

    let (looper_side, caller_side) = make_async_intercom_pair::<LooperEvent, UserMsg>(64);
    let (event_speaker, user_listener) = looper_side.split();
    let (user_speaker, event_listener) = caller_side.split();

    let persister = Arc::new(CountingPersister::default());
    let looper = AgentLooper::new(
        agent,
        Box::new(session),
        event_speaker,
        LooperConfig {
            persist_on_failure,
            epilogue,
            ..Default::default()
        },
        Arc::clone(&persister) as Arc<dyn crate::persistence::SessionPersister>,
    );
    // 直写内部闩模拟「run() 启动前已请求取消」（测试同模块访问私有字段）
    looper.cancel_request.store(cancel, Ordering::Release);

    FailureHarness {
        looper,
        _user_speaker: user_speaker,
        user_listener: Some(user_listener),
        events: event_listener,
    }
}

impl FailureHarness {
    /// 数一数收到过几次 `TurnComplete`。
    fn turn_completes(&mut self) -> usize {
        let mut n = 0;
        while let Ok(ev) = self.events.try_recv() {
            if matches!(ev, LooperEvent::TurnComplete { .. }) {
                n += 1;
            }
        }
        n
    }
}

/// 撞上 `max_iterations` 时合成收尾报告：成为本轮最后一条 assistant 消息，
/// 且补发 `TextDelta` 让实时视图（不刷新页面）也能看到。
#[tokio::test]
async fn test_max_iterations_composes_epilogue_as_last_message() {
    let mut h = failure_looper_harness_with_epilogue(
        false,
        true,
        false,
        Some(Arc::new(MockEpiloguer("## 本轮已完成\n- 查了三处"))),
    );
    h.looper.failure_reason = Some(TurnFailureReason::MaxIterationsExceeded);
    h.looper.set_quiet(LooperState::Failed);
    h.looper.finalize_failure().await;

    let turn = &h.looper.session().committed_turns()[0];
    let last = turn.last().unwrap();
    assert!(matches!(
        last.message.as_ref(),
        InputItem::Message { content, .. } if content.text_view().contains("本轮已完成")
    ));
    assert_eq!(
        last.source,
        MessageSource::TurnEpilogue,
        "收尾必须按来源可识别，不靠猜文案"
    );
    // 固定中断说明被取代 —— 两条连续 assistant 会破坏历史合法性
    assert!(
        !turn.iter().any(|am| text_of(am).contains("[interrupted]")),
        "收尾存在时不得再追加固定中断说明"
    );
    // 失败语义不因收尾而变
    assert!(matches!(
        h.looper.failure_reason,
        Some(TurnFailureReason::MaxIterationsExceeded)
    ));

    let mut deltas = Vec::new();
    while let Ok(ev) = h.events.try_recv() {
        if let LooperEvent::TextDelta { delta } = ev {
            deltas.push(delta);
        }
    }
    assert!(
        deltas.iter().any(|d| d.contains("本轮已完成")),
        "收尾是 generate_full 产物，不补发 TextDelta 用户就看不到；现状={deltas:?}"
    );
}

/// 收尾合成失败 → 回退到固定中断说明，失败原因与事件面都不变。
#[tokio::test]
async fn test_epilogue_failure_falls_back() {
    let mut h =
        failure_looper_harness_with_epilogue(false, true, false, Some(Arc::new(FailingEpiloguer)));
    h.looper.failure_reason = Some(TurnFailureReason::MaxIterationsExceeded);
    h.looper.set_quiet(LooperState::Failed);
    h.looper.finalize_failure().await;

    let turn = &h.looper.session().committed_turns()[0];
    let last = turn.last().unwrap();
    assert!(
        text_of(last).contains("[interrupted]"),
        "合成失败必须回退到固定中断说明"
    );
    assert!(matches!(last.source, MessageSource::InterruptedTurn { .. }));
    assert_eq!(h.turn_completes(), 1, "失败轮照发一次 TurnComplete");
    assert!(matches!(
        h.looper.failure_reason,
        Some(TurnFailureReason::MaxIterationsExceeded)
    ));
}

/// 只有 `MaxIterationsExceeded` 才合成收尾 —— 取消 / Hook 中止没有「还没做完」可言。
#[tokio::test]
async fn test_epilogue_only_for_max_iterations() {
    let mut h = failure_looper_harness_with_epilogue(
        false,
        true,
        false,
        Some(Arc::new(MockEpiloguer("不该出现的收尾"))),
    );
    h.looper.failure_reason = Some(TurnFailureReason::HookAbort("hooked".into()));
    h.looper.set_quiet(LooperState::Failed);
    h.looper.finalize_failure().await;

    let turn = &h.looper.session().committed_turns()[0];
    assert!(
        !turn
            .iter()
            .any(|am| matches!(am.source, MessageSource::TurnEpilogue)),
        "非 max_iterations 失败不得合成收尾"
    );
    assert!(text_of(turn.last().unwrap()).contains("[interrupted]"));
}

/// 失败后自动续接排队输入时，上一轮的失败原因不得跨轮存活 —— 续接那轮若正常
/// 跑到 `Done`，会撞上 `debug_assert!(failure_reason.is_none())`。同时验证多条
/// 排队输入被一次排空、合并进续接轮。
///
/// 收尾与续接是两步（`finalize_failure` → `start_turn_from_pending`），对应
/// `run()` 的 ② 与 ③；这里按同样顺序手动驱动。
#[tokio::test]
async fn test_auto_continue_clears_failure_reason() {
    let mut h = failure_looper_harness(false, true, true);
    // harness 已入队 "queued"，再补一条 —— 续接时必须一次消化两条
    h.looper.session.enqueue_pending("second".into());

    h.looper.failure_reason = Some(TurnFailureReason::HookAbort("hooked".into()));
    h.looper.set_quiet(LooperState::Failed);
    h.looper.finalize_failure().await;
    assert!(
        h.looper.session().has_pending(),
        "失败收尾不得吞掉排队输入 —— 那是 ③ 的事"
    );
    assert!(
        matches!(
            h.looper.failure_reason,
            Some(TurnFailureReason::HookAbort(_))
        ),
        "收尾后原因留在 self 上等 Shutdown 读取；现状={:?}",
        h.looper.failure_reason
    );

    h.looper.pending_armed = true;
    h.looper.start_turn_from_pending();
    assert!(
        h.looper.failure_reason.is_none(),
        "续接后必须清空 failure_reason；现状={:?}",
        h.looper.failure_reason
    );
    assert!(
        !h.looper.session().has_pending(),
        "续接轮必须一次排空全部排队输入"
    );

    // 续接的那一轮正常跑完 —— Done 分支的 debug_assert 不得触发
    h.looper.set_quiet(LooperState::Done);
    h.looper.react_step().await;
    assert_eq!(h.looper.session().committed_turns().len(), 2);

    // 续接轮的 user 消息是两条排队输入的合并形态
    let continued_turn = &h.looper.session().committed_turns()[1];
    assert!(
        matches!(
            continued_turn[0].message.as_ref(),
            InputItem::Message { content, .. }
                if content.text_view() == "---\nqueued\n---\nsecond"
        ),
        "续接轮 user 消息应为合并文本；实际={:?}",
        continued_turn[0].message
    );
}

/// 非取消类失败收尾后应停靠回 `Idle` 等下一句输入，而不是终止整个 run()。
#[tokio::test]
async fn test_non_cancel_failure_docks_at_idle() {
    let mut h = failure_looper_harness(false, true, false);
    // 模拟 react_step 已挂起的失败（如 hook abort / max turns）
    h.looper.set_quiet(LooperState::Failed);
    h.looper.failure_reason = Some(TurnFailureReason::HookAbort("boom".into()));

    let listener = h.user_listener.take().expect("harness 持有 listener");
    let res = tokio::time::timeout(Duration::from_millis(300), h.looper.run(listener)).await;
    assert!(
        res.is_err(),
        "失败收尾后应停靠等输入，而不是 break 退出 run()"
    );

    assert_eq!(
        h.looper.session().committed_turns().len(),
        1,
        "非取消失败同样要冻结在途轮"
    );
    assert!(matches!(h.looper.state(), LooperState::Idle));
    assert_eq!(h.turn_completes(), 1, "冻结必须恰好发一次 TurnComplete");
}

// ── State enum tests ───────────────────────────────────────────────

#[test]
fn test_looper_state_serde_roundtrip() {
    for state in [LooperState::Idle, LooperState::PausedDuringTools] {
        let json = serde_json::to_string(&state).unwrap();
        let deserialized: LooperState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, state);
    }
}

// ── 统一重试 ────────────────────────────────────────────────────────

/// 一次模型调用的脚本：吐一串 chunk、直接给一个非流式响应，或者直接失败。
enum Script {
    Chunks(Vec<StreamChunk>),
    /// 流中途 yield `Err` —— 模拟 established-stream 中断 / 流内错误 payload。
    ChunksWithErr(Vec<Result<StreamChunk, model_provider::ProviderError>>),
    Batch(Box<model_provider::GenerateResult>),
    Fail(model_provider::ProviderError),
}

/// 按调用顺序弹出脚本的 provider，并记录每次请求的输出预算。
///
/// 脚本与调用方式必须配对：流式路径只能拿到 `Chunks`、批量路径只能拿到
/// `Batch`，配错即测试自身写错，故宁可直接 panic 也不静默降级。
struct ScriptedStreamProvider {
    scripts: std::sync::Mutex<std::collections::VecDeque<Script>>,
    budgets: std::sync::Mutex<Vec<Option<u32>>>,
}

impl ScriptedStreamProvider {
    fn new(scripts: Vec<Script>) -> Self {
        Self {
            scripts: std::sync::Mutex::new(scripts.into()),
            budgets: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// 每次请求实际收到的输出预算（`None` = 未指定，走服务端默认）。
    fn budgets(&self) -> Vec<Option<u32>> {
        self.budgets.lock().unwrap().clone()
    }

    fn next_script(&self) -> Script {
        self.scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted provider ran out of scripts")
    }
}

#[async_trait::async_trait]
impl model_provider::ModelProvider for ScriptedStreamProvider {
    fn name(&self) -> &str {
        "scripted-stream"
    }

    async fn generate_full(
        &self,
        request: &model_provider::GenerateRequest,
    ) -> Result<model_provider::GenerateResult, model_provider::ProviderError> {
        self.budgets.lock().unwrap().push(request.max_output_tokens);
        match self.next_script() {
            Script::Batch(r) => Ok(*r),
            Script::Fail(e) => Err(e),
            Script::Chunks(_) | Script::ChunksWithErr(_) => {
                panic!("batch path received a streaming script; test harness misconfigured")
            }
        }
    }

    async fn generate_stream(
        &self,
        request: &model_provider::GenerateRequest,
    ) -> Result<GenerateStream, model_provider::ProviderError> {
        self.budgets.lock().unwrap().push(request.max_output_tokens);
        match self.next_script() {
            Script::Fail(e) => Err(e),
            Script::Chunks(chunks) => Ok(GenerateStream::new(Box::pin(futures::stream::iter(
                chunks.into_iter().map(Ok),
            )))),
            Script::ChunksWithErr(chunks) => {
                Ok(GenerateStream::new(Box::pin(futures::stream::iter(chunks))))
            }
            Script::Batch(_) => {
                panic!("streaming path received a batch script; test harness misconfigured")
            }
        }
    }
}

/// 非流式响应：`status` / `finish_reason` 由调用方指定，用来钉住批量路径的判据。
fn batch_response(
    text: &str,
    output_tokens: u32,
    status: model_provider::ResponseStatus,
    finish_reason: Option<FinishReason>,
) -> Script {
    Script::Batch(Box::new(model_provider::GenerateResult {
        id: "r".to_string(),
        output: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        usage: Usage {
            input_tokens: 10,
            output_tokens,
            total_tokens: 10 + output_tokens,
        },
        status,
        finish_reason,
        error: None,
    }))
}

/// 一段可见文本 + 收敛原因。`output_tokens` 用来模拟顶到预算。
fn text_chunks(text: &str, output_tokens: u32, reason: FinishReason) -> Script {
    Script::Chunks(vec![
        StreamChunk::BlockStart {
            index: 0,
            block_type: model_provider::BlockType::Text,
        },
        StreamChunk::TextDelta {
            index: 0,
            delta: text.to_string(),
        },
        StreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::Text {
                text: text.to_string(),
            },
        },
        StreamChunk::Usage {
            usage: Usage {
                input_tokens: 10,
                output_tokens,
                total_tokens: 10 + output_tokens,
            },
        },
        StreamChunk::Finish { reason },
    ])
}

/// 被截断的那次：文本落了块，但没有 `Stop`。
fn truncated(text: &str) -> Script {
    text_chunks(text, 4096, FinishReason::MaxTokens)
}

/// 正常收尾的那次。
fn completed(text: &str) -> Script {
    text_chunks(text, 20, FinishReason::Stop)
}

/// 截断重试测试的 looper 及其通道端点（保活语义同 [`FailureHarness`]）。
struct RetryHarness {
    looper: AgentLooper,
    _user_speaker: Speaker<UserMsg>,
    user_listener: Option<Listener<UserMsg>>,
    provider: Arc<ScriptedStreamProvider>,
    events: Listener<LooperEvent>,
}

impl RetryHarness {
    fn committed_turns(&self) -> usize {
        self.looper.session().committed_turns().len()
    }

    /// 收齐本轮事件里的 `TurnComplete` 结果。
    fn turn_outcomes(&mut self) -> Vec<TurnOutcome> {
        let mut out = Vec::new();
        while let Ok(ev) = self.events.try_recv() {
            if let LooperEvent::TurnComplete { outcome, .. } = ev {
                out.push(outcome);
            }
        }
        out
    }

    /// 原样排空事件通道 —— 需要断言事件**顺序**（如通知早于重试的增量）时用。
    ///
    /// 与 [`Self::turn_outcomes`] 互斥：两者都在消费同一个 listener，
    /// 先 drain 的拿走全部。
    fn drain_events(&mut self) -> Vec<LooperEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.events.try_recv() {
            out.push(ev);
        }
        out
    }
}

/// 取出 `events` 里全部 `TruncationRetry` 通知，连同它们在序列中的下标。
fn truncation_notices(events: &[LooperEvent]) -> Vec<(usize, u32, u32, u32, u32, String)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(i, ev)| match ev {
            LooperEvent::TruncationRetry {
                attempt,
                limit,
                output_tokens,
                retry_budget,
                discarded_text,
                ..
            } => Some((
                i,
                *attempt,
                *limit,
                *output_tokens,
                *retry_budget,
                discarded_text.clone(),
            )),
            _ => None,
        })
        .collect()
}

/// `events` 里全部 `TextDelta` 原文首尾相接 —— 即客户端此刻累积到的内容。
fn streamed_text(events: &[LooperEvent]) -> String {
    events
        .iter()
        .filter_map(|ev| match ev {
            LooperEvent::TextDelta { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

/// 构造截断重试测试用的 looper：流式 provider + 指定的重试配置。
fn retry_harness(scripts: Vec<Script>, config: LooperConfig) -> RetryHarness {
    retry_harness_with_budget(scripts, config, None)
}

/// 同上，但可指定 `ModelConfig::max_tokens`（用于「预算已高于下限」的守卫用例）。
fn retry_harness_with_budget(
    scripts: Vec<Script>,
    config: LooperConfig,
    max_tokens: Option<u32>,
) -> RetryHarness {
    retry_harness_inner(scripts, config, max_tokens, true)
}

/// 走**批量**路径的 harness（`ModelConfig::stream = false`）。
///
/// 批量路径的重试判据与流式不同（流式有 `FinishReason`，批量要靠
/// `GenerateResult::finish_reason`），需要各自的用例钉住。
fn batch_retry_harness(scripts: Vec<Script>, config: LooperConfig) -> RetryHarness {
    retry_harness_inner(scripts, config, None, false)
}

fn retry_harness_inner(
    scripts: Vec<Script>,
    config: LooperConfig,
    max_tokens: Option<u32>,
    stream: bool,
) -> RetryHarness {
    let profile: crate::agent::AgentProfile = serde_yaml::from_str(
        "agent:\n  name: t\n  description: d\nllm:\n  provider: p\n  model: m\n",
    )
    .unwrap();
    let executor: Arc<dyn crate::tools::ToolExecutor> =
        Arc::new(crate::tools::DefaultToolsExecutor::new(Vec::new()));
    let provider = Arc::new(ScriptedStreamProvider::new(scripts));
    let mut builder = crate::agent::ModelConfigBuilder::new().stream(stream);
    if let Some(m) = max_tokens {
        builder = builder.max_tokens(m);
    }
    let agent = Arc::new(Agent::from_parts(
        std::path::PathBuf::from("/tmp/agent.md"),
        profile,
        "sys".to_string(),
        Arc::clone(&provider) as Arc<dyn model_provider::ModelProvider>,
        builder.build(),
        Arc::clone(&executor),
        Arc::new(crate::mcp::McpManager::empty(executor)),
        None,
    ));

    let session = Session::new("s1".to_string(), "d".to_string());

    let (looper_side, caller_side) = make_async_intercom_pair::<LooperEvent, UserMsg>(64);
    let (event_speaker, user_listener) = looper_side.split();
    let (user_speaker, event_listener) = caller_side.split();

    let looper = AgentLooper::new(
        agent,
        Box::new(session),
        event_speaker,
        config,
        Arc::new(crate::persistence::NullSessionPersister),
    );

    RetryHarness {
        looper,
        _user_speaker: user_speaker,
        user_listener: Some(user_listener),
        provider,
        events: event_listener,
    }
}

/// 送一句 query 并驱动到本轮结束，然后停靠回 Idle。
///
/// `run()` 正常完成会停靠等输入而不退出，故以超时结束 —— 与
/// [`test_non_cancel_failure_docks_at_idle`] 同法，超时即「已收敛」。
async fn drive_query(h: &mut RetryHarness) {
    let listener = h.user_listener.take().expect("harness 持有 listener");
    h._user_speaker
        .send(UserMsg::Query("hi".into()))
        .await
        .expect("looper 侧仍在监听");
    let _ = tokio::time::timeout(Duration::from_secs(5), h.looper.run(listener)).await;
}

// ── 服务端工具块（web_search）tests ───────────────────────────────────

/// 服务端 web_search 一次完整流：发起 → 结果 → 可见文本。
fn server_web_search_chunks() -> Script {
    Script::Chunks(vec![
        StreamChunk::BlockStart {
            index: 0,
            block_type: model_provider::BlockType::ServerToolUse,
        },
        StreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::ServerToolUse {
                call_id: "srv_1".to_string(),
                name: "web_search".to_string(),
                input: r#"{"query":"rust async"}"#.to_string(),
            },
        },
        StreamChunk::BlockStart {
            index: 1,
            block_type: model_provider::BlockType::ServerToolResult,
        },
        StreamChunk::BlockEnd {
            index: 1,
            block: ContentBlock::ServerToolResult {
                call_id: "srv_1".to_string(),
                name: "web_search".to_string(),
                content: r#"{"results":["a","b"]}"#.to_string(),
            },
        },
        StreamChunk::BlockStart {
            index: 2,
            block_type: model_provider::BlockType::Text,
        },
        StreamChunk::TextDelta {
            index: 2,
            delta: "答案".to_string(),
        },
        StreamChunk::BlockEnd {
            index: 2,
            block: ContentBlock::Text {
                text: "答案".to_string(),
            },
        },
        StreamChunk::Usage {
            usage: Usage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
            },
        },
        StreamChunk::Finish {
            reason: FinishReason::Stop,
        },
    ])
}

#[tokio::test]
async fn test_server_tool_emits_events_and_skips_client_execution() {
    let mut h = retry_harness(vec![server_web_search_chunks()], LooperConfig::default());

    drive_query(&mut h).await;

    let events = h.drain_events();

    let uses: Vec<_> = events
        .iter()
        .filter_map(|ev| match ev {
            LooperEvent::ServerToolUse { id, name, input } => {
                Some((id.as_str(), name.as_str(), input.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        uses,
        vec![("srv_1", "web_search", r#"{"query":"rust async"}"#)],
        "必须恰好发出一次 ServerToolUse"
    );

    let results: Vec<_> = events
        .iter()
        .filter_map(|ev| match ev {
            LooperEvent::ServerToolResult { id, name, content } => {
                Some((id.as_str(), name.as_str(), content.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        vec![("srv_1", "web_search", r#"{"results":["a","b"]}"#)],
        "必须恰好发出一次 ServerToolResult"
    );

    assert!(
        !events.iter().any(|ev| matches!(
            ev,
            LooperEvent::ToolCallStart { id, .. } | LooperEvent::ToolCallDelta { id, .. }
                if id == "srv_1"
        )),
        "服务端工具不得触发客户端工具执行事件"
    );

    let outcomes: Vec<_> = events
        .iter()
        .filter_map(|ev| match ev {
            LooperEvent::TurnComplete { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(outcomes.len(), 1, "必须恰好收尾一次");
    match &outcomes[0] {
        TurnOutcome::Success { text, .. } => assert_eq!(text, "答案"),
        other => panic!("预期 Success，实际 {other:?}"),
    }

    // 服务端工具块不入 session 历史 —— 提交后的轮次里不得有对应 FunctionCall。
    let committed = h.looper.session().committed_turns();
    assert_eq!(committed.len(), 1);
    let has_function_call = committed[0].iter().any(|am| {
        matches!(
            am.message.as_ref(),
            InputItem::FunctionCall { call_id, .. } if call_id == "srv_1"
        )
    });
    assert!(!has_function_call, "服务端工具块不得写入 session 历史");
    let text = committed[0]
        .iter()
        .find_map(|am| match am.message.as_ref() {
            InputItem::Message {
                role: Role::Assistant,
                content,
            } => Some(content.text_view().to_string()),
            _ => None,
        })
        .unwrap_or_default();
    assert_eq!(text, "答案");
}

#[tokio::test]
async fn test_batch_server_tool_emits_events() {
    let mut h = batch_retry_harness(
        vec![Script::Batch(Box::new(model_provider::GenerateResult {
            id: "r".to_string(),
            output: vec![
                ContentBlock::ServerToolUse {
                    call_id: "srv_1".to_string(),
                    name: "web_search".to_string(),
                    input: r#"{"query":"rust"}"#.to_string(),
                },
                ContentBlock::ServerToolResult {
                    call_id: "srv_1".to_string(),
                    name: "web_search".to_string(),
                    content: r#"{"results":[]}"#.to_string(),
                },
                ContentBlock::Text {
                    text: "答案".to_string(),
                },
            ],
            usage: Usage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
            },
            status: ResponseStatus::Completed,
            finish_reason: Some(FinishReason::Stop),
            error: None,
        }))],
        LooperConfig::default(),
    );

    drive_query(&mut h).await;

    let events = h.drain_events();
    assert_eq!(
        events
            .iter()
            .filter(|ev| matches!(ev, LooperEvent::ServerToolUse { .. }))
            .count(),
        1,
        "批量路径同样发出 ServerToolUse"
    );
    assert_eq!(
        events
            .iter()
            .filter(|ev| matches!(ev, LooperEvent::ServerToolResult { .. }))
            .count(),
        1,
        "批量路径同样发出 ServerToolResult"
    );
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, LooperEvent::ToolCallStart { id, .. } if id == "srv_1")),
        "批量路径的服务端工具也不得触发客户端工具事件"
    );
    let committed = h.looper.session().committed_turns();
    assert_eq!(committed.len(), 1);
    assert!(
        !committed[0].iter().any(|am| matches!(
            am.message.as_ref(),
            InputItem::FunctionCall { call_id, .. } if call_id == "srv_1"
        )),
        "服务端工具块不得写入 session 历史"
    );
}

// ── 暂停/恢复（UserMsg 带内控制）tests ────────────────────────────────

/// spawn 一个脚本化 provider 的 looper 及其句柄 —— 暂停/恢复走
/// [`LooperHandle`]，与生产路径同构。
fn spawn_scripted_looper(
    scripts: Vec<Script>,
    config: LooperConfig,
) -> (LooperHandle, Arc<ScriptedStreamProvider>) {
    spawn_scripted_looper_with_persister(
        scripts,
        config,
        Arc::new(crate::persistence::NullSessionPersister),
    )
}

/// 同上，但落盘走注入的 persister —— 落盘时机类测试要数 save 次数。
fn spawn_scripted_looper_with_persister(
    scripts: Vec<Script>,
    config: LooperConfig,
    persister: Arc<dyn crate::persistence::SessionPersister>,
) -> (LooperHandle, Arc<ScriptedStreamProvider>) {
    let profile: crate::agent::AgentProfile = serde_yaml::from_str(
        "agent:\n  name: t\n  description: d\nllm:\n  provider: p\n  model: m\n",
    )
    .unwrap();
    let executor: Arc<dyn crate::tools::ToolExecutor> =
        Arc::new(crate::tools::DefaultToolsExecutor::new(Vec::new()));
    let provider = Arc::new(ScriptedStreamProvider::new(scripts));
    let agent = Arc::new(Agent::from_parts(
        std::path::PathBuf::from("/tmp/agent.md"),
        profile,
        "sys".to_string(),
        Arc::clone(&provider) as Arc<dyn model_provider::ModelProvider>,
        crate::agent::ModelConfigBuilder::new().stream(true).build(),
        Arc::clone(&executor),
        Arc::new(crate::mcp::McpManager::empty(executor)),
        None,
    ));
    let session = Session::new("s1".to_string(), "d".to_string());
    let handle = AgentLooper::spawn(agent, Box::new(session), config, persister);
    (handle, provider)
}

/// 收事件直到状态到达 `target`（5s 超时即失败）。
async fn recv_until_state(h: &LooperHandle, target: LooperState) {
    let res = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match h.recv_event().await {
                Some(LooperEvent::StateChange { to, .. }) if to == target => break,
                Some(_) => continue,
                None => panic!("event channel closed before reaching {target:?}"),
            }
        }
    })
    .await;
    res.expect("timed out waiting for state");
}

/// 收事件直到拿到一个成功的 `TurnComplete`。
async fn recv_until_turn_complete(h: &LooperHandle) -> TurnOutcome {
    let res = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match h.recv_event().await {
                Some(LooperEvent::TurnComplete { outcome, .. }) => break outcome,
                Some(_) => continue,
                None => panic!("event channel closed before TurnComplete"),
            }
        }
    })
    .await;
    res.expect("timed out waiting for TurnComplete")
}

/// 空闲态 Pause 是无操作：没有在途轮可挂起 —— 状态不动、镜像不置、不发暂停
/// 事件；此后 Query 正常起轮（空闲态的 Resume 同样无操作）。
#[tokio::test]
async fn test_idle_pause_is_noop() {
    let (h, _provider) = spawn_scripted_looper(vec![completed("ok")], LooperConfig::default());

    // 空闲时 Pause —— 无在途可挂起，什么也不发生
    h.pause().await.expect("looper alive");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let stray = h.drain_events();
    assert!(
        !stray.iter().any(is_paused_event),
        "空闲态 Pause 不得产生暂停事件，实际收到 {stray:?}"
    );
    assert!(!h.is_paused(), "空闲态 Pause 不得置暂停镜像");

    // 空闲态的 Resume 同样无操作（无挂起意图可撤回，不产生任何事件）
    h.resume().await.expect("looper alive");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let stray = h.drain_events();
    assert!(
        stray.is_empty(),
        "空闲态 Resume 不得产生事件，实际收到 {stray:?}"
    );

    // 后续 Query 正常起轮 —— 空闲暂停不再拦路
    h.send_query("hi".into()).await.expect("looper alive");
    match recv_until_turn_complete(&h).await {
        TurnOutcome::Success { text } => assert_eq!(text, "ok"),
        other => panic!("Query 应正常完成，实际 {other:?}"),
    }
    recv_until_state(&h, LooperState::Idle).await;
    assert!(!h.is_paused(), "完成后镜像为 false");

    h.shutdown().await.expect("clean shutdown");
}

/// 取消语义是「中止当前 ReAct 轮，looper 不退出」—— 在途轮暂停中取消同样如此：
/// 收尾把状态带回 Idle、复位暂停镜像与一次性的 `cancel_request`，looper 停在
/// Idle 继续等输入。
#[tokio::test]
async fn test_cancel_from_paused_returns_to_idle_and_looper_survives() {
    let (h, mut senders) = spawn_controllable_looper();
    reach_paused_during_tools(&h, &mut senders).await;
    assert!(h.is_paused(), "进入暂停态后镜像必须为 true");

    // 暂停中不发 Resume、不发 query，仅靠 Cancel 收尾
    h.cancel().await.expect("looper alive");

    // 取消不退出 looper —— 收尾后回到 Idle
    recv_until_state(&h, LooperState::Idle).await;
    assert!(!h.is_paused(), "取消收尾后暂停镜像必须复位");
    assert!(
        !h.is_cancelled(),
        "cancel_request 是一次性的，收尾后必须复位"
    );

    // looper 仍存活：下一句 Query 能正常跑完一轮（喂第二条流）
    h.send_query("again".into())
        .await
        .expect("取消后 looper 仍须接受输入");
    feed_text_stream(&mut senders, "ok").await;
    match recv_until_turn_complete(&h).await {
        TurnOutcome::Success { text } => assert_eq!(text, "ok"),
        other => panic!("取消后续接轮应正常完成，实际 {other:?}"),
    }

    h.shutdown().await.expect("clean shutdown");
}

/// 取消收尾必须先把在途工具**已完成**的结果捞回来，再 abort 其余任务。工具
/// 轮询是唯一持有在途 tokio 任务、也是唯一「成果连内存都进不去」的状态 ——
/// 最后一个 200ms 窗口内完成的结果只存在于 JoinSet 里，漏掉就永久丢失。
/// 顺序颠倒（先 abort 后冻结）本测试必须红。
#[tokio::test]
async fn test_cancel_salvages_finished_tool_results() {
    let mut h = failure_looper_harness(false, false, false);

    // 两个在途任务：一个立刻完成（必须被捞回），一个永不完成（必须被 abort）
    let mut joinset: tokio::task::JoinSet<(usize, ToolCallResult)> = tokio::task::JoinSet::new();
    joinset.spawn(async { tool_result(0) });
    joinset.spawn(async {
        std::future::pending::<()>().await;
        tool_result(1)
    });
    // 模拟「最后一次轮询之后、取消之前」完成的那个
    tokio::time::sleep(Duration::from_millis(30)).await;

    h.looper.react_ctx.pending_tool_calls = pending_slots(2);
    h.looper.active_tool_tasks = Some(joinset);
    h.looper.set_quiet(LooperState::ExecutingTools);

    h.looper.finalize_cancel().await;

    assert!(
        h.looper.active_tool_tasks.is_none(),
        "取消必须回收在途任务集"
    );
    assert!(
        matches!(h.looper.failure_reason, Some(TurnFailureReason::Cancelled)),
        "取消收尾必须留下 Cancelled 原因，实际={:?}",
        h.looper.failure_reason
    );
    // 已完成的 c0 结果随本轮一起冻结（悬空的 c1 由 plan_failure 补占位）
    let turn = &h.looper.session().committed_turns()[0];
    assert!(
        turn.iter().any(|am| matches!(
            am.message.as_ref(),
            InputItem::FunctionCallOutput { call_id, output }
                if call_id == "c0" && matches!(output, Content::Text(t) if t == "r0")
        )),
        "取消前已完成的工具结果必须随本轮冻结，实际={:?}",
        turn.iter().map(|am| &am.message).collect::<Vec<_>>()
    );
}

/// finalize 在「无在途轮」（plan=None）时也必须把状态带回停靠位 —— Failed 留在
/// 循环顶会反复命中形成热自旋；暂停态不满足 `is_active()`，不得被推平。
#[tokio::test]
async fn test_finalize_none_repairs_state() {
    let mut h = failure_looper_harness(false, true, false);
    // 造「无在途轮」：回滚到 Idle、staging 清空
    h.looper.session.rollback_turn(false).unwrap();

    h.looper.set_quiet(LooperState::Failed);
    h.looper
        .apply_failure(TurnFailureReason::Other("x".into()), true, None)
        .await;
    assert_eq!(
        h.looper.state(),
        LooperState::Idle,
        "plan=None 后必须离开 Failed 回到停靠位"
    );

    // 暂停态不得被 None 分支推平（恢复落点由变体自身编码）
    h.looper.set_quiet(LooperState::PausedDuringTools);
    h.looper
        .apply_failure(TurnFailureReason::Other("x".into()), false, None)
        .await;
    assert_eq!(
        h.looper.state(),
        LooperState::PausedDuringTools,
        "plan=None 不得推平暂停态"
    );
    assert!(
        h.looper.pause_mirror.load(Ordering::Acquire),
        "暂停态保留时镜像必须仍为 true"
    );
}

/// 暂停态取消收尾：落地点 B 只在 spawn 前生效，暂停态不握 JoinSet、不握
/// 活连接 —— 收尾无在途资源可回收，但状态/镜像/取消闩/挂起意图必须全部
/// 复位到干净 Idle，在途轮照常冻结。
#[tokio::test]
async fn test_finalize_cancel_from_paused_resets_cleanly() {
    let mut h = failure_looper_harness(false, false, false);
    h.looper.set_quiet(LooperState::PausedDuringTools);
    h.looper.pause_pending = true;
    // 消息路径置闩（finalize_cancel 收尾后必须复位，否则回 Idle 反复命中）
    let _ = h.looper.handle_user_msg(UserMsg::Cancel);

    h.looper.finalize_cancel().await;

    assert_eq!(
        h.looper.state(),
        LooperState::Idle,
        "取消收尾必须归位到干净 Idle"
    );
    assert!(
        !h.looper.pause_mirror.load(Ordering::Acquire),
        "归位后暂停镜像必须复位"
    );
    assert!(
        !h.looper.cancel_request.load(Ordering::Acquire),
        "取消闩是一次性的，收尾后必须复位"
    );
    assert!(!h.looper.pause_pending, "取消优先：暂停意图必须作废");
    assert!(
        h.looper.active_tool_tasks.is_none(),
        "暂停态无在途任务集，收尾后仍为 None"
    );
    assert!(
        matches!(h.looper.failure_reason, Some(TurnFailureReason::Cancelled)),
        "取消收尾必须留下 Cancelled 原因，实际={:?}",
        h.looper.failure_reason
    );
    assert_eq!(
        h.looper.session().committed_turns().len(),
        1,
        "在途轮必须照常冻结"
    );
}

/// Resume 带排队输入时：从暂停态**单迁移**直达运行位（不途经 Idle ——
/// 途经 Idle 会让 runner 见 to=Idle 且无 SSE 订阅者时回收 handle）。排队
/// 输入由 pending 队列持有，本轮收尾回 Idle 之后才由 ③ 启动续接轮。
#[tokio::test]
async fn test_resume_with_pending_skips_idle_emit() {
    let (h, mut senders) = spawn_controllable_looper();
    reach_paused_during_tools(&h, &mut senders).await;

    // 暂停期间排队一条消息，再恢复 —— 恢复必须单迁移到运行位
    h.send_query("queued".into()).await.expect("looper alive");
    h.resume().await.expect("looper alive");

    let resumed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match h.recv_event().await {
                Some(LooperEvent::StateChange {
                    from: LooperState::PausedDuringTools,
                    to,
                    ..
                }) => {
                    break (LooperState::PausedDuringTools, to);
                }
                Some(_) => continue,
                None => panic!("event channel closed before resume transition"),
            }
        }
    })
    .await
    .expect("timed out waiting for resume transition");
    assert_eq!(
        resumed,
        (LooperState::PausedDuringTools, LooperState::ExecutingTools),
        "Resume 必须从暂停态单迁移到运行位"
    );

    // 工具执行完 → 本轮第二次模型调用
    feed_text_stream(&mut senders, "hello").await;

    // 从 Resume 到本轮收尾：不得出现「暂停态 → Idle」的迁移
    let first = tokio::time::timeout(Duration::from_secs(5), async {
        let mut saw_idle_from_paused = false;
        loop {
            match h.recv_event().await {
                Some(LooperEvent::StateChange { from, to, .. })
                    if from.is_paused() && to == LooperState::Idle =>
                {
                    saw_idle_from_paused = true;
                }
                Some(LooperEvent::TurnComplete { outcome, .. }) => {
                    break (saw_idle_from_paused, outcome);
                }
                Some(_) => continue,
                None => panic!("event channel closed before TurnComplete"),
            }
        }
    })
    .await
    .expect("timed out waiting for the paused turn to finish");
    assert!(
        !first.0,
        "Resume 到本轮收尾不得发出暂停态 → Idle（会触发 runner 回收）"
    );
    match first.1 {
        TurnOutcome::Success { text } => assert_eq!(text, "hello"),
        other => panic!("恢复后的在途轮应成功，实际 {other:?}"),
    }

    // 续接轮：排队输入在本轮收尾后由 ③ 启动
    feed_text_stream(&mut senders, "ok").await;
    expect_queued_continuation(&h).await;

    h.shutdown().await.expect("clean shutdown");
}

/// 暂停中排队的 query 尚未跑（在途轮停在暂停态）时通道关闭：按取消语义收尾
/// 在途轮，排队的 query 不补跑 —— 恰好一条 TurnStart / 一条 TurnComplete，
/// 无第二条 TurnStart，Shutdown 原因为 Cancelled。
#[tokio::test]
async fn test_paused_queued_query_input_closed_cancels_without_turn() {
    // 手工构造（非 spawn）：需要单独 drop user_speaker；
    // LooperHandle 整体 drop 会 abort 后台任务。可控 provider 把 looper
    // 确定地停在「暂停已落地、轮未收尾」的中间态。
    let profile: crate::agent::AgentProfile = serde_yaml::from_str(
        "agent:\n  name: t\n  description: d\nllm:\n  provider: p\n  model: m\n",
    )
    .unwrap();
    let executor: Arc<dyn crate::tools::ToolExecutor> =
        Arc::new(crate::tools::DefaultToolsExecutor::new(Vec::new()));
    let (tx_inbox, mut senders) = tokio::sync::mpsc::unbounded_channel::<ChunkSender>();
    let provider = Arc::new(ControllableStreamProvider { senders: tx_inbox });
    let agent = Arc::new(Agent::from_parts(
        std::path::PathBuf::from("/tmp/agent.md"),
        profile,
        "sys".to_string(),
        Arc::clone(&provider) as Arc<dyn model_provider::ModelProvider>,
        crate::agent::ModelConfigBuilder::new().stream(true).build(),
        Arc::clone(&executor),
        Arc::new(crate::mcp::McpManager::empty(executor)),
        None,
    ));
    let session = Session::new("s1".to_string(), "d".to_string());
    let (looper_side, caller_side) = make_async_intercom_pair::<LooperEvent, UserMsg>(64);
    let (event_speaker, user_listener) = looper_side.split();
    let (user_speaker, mut event_listener) = caller_side.split();
    let mut looper = AgentLooper::new(
        agent,
        Box::new(session),
        event_speaker,
        LooperConfig::default(),
        Arc::new(crate::persistence::NullSessionPersister),
    );
    let run_task = tokio::spawn(async move { looper.run(user_listener).await });

    // 在途轮推进到流式在途，打 Pause 后以 tool_calls 收尾 —— 落地
    // PausedDuringTools（spawn 前：无在途任务）。全程事件统一累计。
    let mut events: Vec<LooperEvent> = Vec::new();
    user_speaker.send(UserMsg::Query("q".into())).await.unwrap();
    let tx = senders
        .recv()
        .await
        .expect("provider should start a stream");
    tx.send(Ok(text_start())).unwrap();
    tx.send(Ok(text_delta("hel"))).unwrap();
    drain_until_text_delta_raw(&mut event_listener, &mut events, "hel").await;
    user_speaker.send(UserMsg::Pause).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    tx.send(Ok(text_delta("lo"))).unwrap();
    tx.send(Ok(text_end("hello"))).unwrap();
    tx.send(Ok(tool_call_end("call-1", "unknown_tool", "{}")))
        .unwrap();
    tx.send(Ok(stream_usage())).unwrap();
    tx.send(Ok(stream_finish())).unwrap();
    drain_until_paused_raw(&mut event_listener, &mut events).await;

    // 暂停中排队一条 query（只入队），随后关闭通道
    user_speaker
        .send(UserMsg::Query("queued".into()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    drop(user_speaker);

    let res = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match event_listener.recv().await {
                Some(LooperEvent::Shutdown { reason, .. }) => break reason,
                Some(e) => events.push(e),
                None => panic!("event channel closed before Shutdown"),
            }
        }
    })
    .await;
    let reason = res.expect("暂停中关闭通道应干净退出");

    let turn_starts = events
        .iter()
        .filter(|e| matches!(e, LooperEvent::TurnStart { .. }))
        .count();
    let outcomes: Vec<&TurnOutcome> = events
        .iter()
        .filter_map(|e| match e {
            LooperEvent::TurnComplete { outcome, .. } => Some(outcome),
            _ => None,
        })
        .collect();
    assert_eq!(turn_starts, 1, "只有在途轮那一次 TurnStart，排队的不起轮");
    assert_eq!(outcomes.len(), 1, "在途轮恰好收尾一次，排队的不补跑");
    assert!(
        matches!(
            outcomes[0],
            TurnOutcome::Failed {
                reason: TurnFailureReason::Cancelled,
                ..
            }
        ),
        "在途轮收尾应为 Failed{{Cancelled}}，实际 {:?}",
        outcomes[0]
    );
    assert_eq!(reason, "Cancelled", "通道关闭按取消语义收尾，实际 {reason}");

    run_task.await.unwrap().unwrap();
}

/// 暂停中**在途**轮 + 通道关闭：收尾必须把在途轮判为 Cancelled，发出
/// 恰好一条 `TurnComplete{Failed}`，Shutdown 原因同为 Cancelled ——
/// 不得静默丢轮。
#[tokio::test]
async fn test_paused_inflight_turn_closed_cancels_with_turn_complete() {
    // 手工构造（非 spawn）：需要单独 drop user_speaker，且用可控 provider
    // 把 looper 确定地停在「暂停已落地、轮未收尾」的中间态。
    let profile: crate::agent::AgentProfile = serde_yaml::from_str(
        "agent:\n  name: t\n  description: d\nllm:\n  provider: p\n  model: m\n",
    )
    .unwrap();
    let executor: Arc<dyn crate::tools::ToolExecutor> =
        Arc::new(crate::tools::DefaultToolsExecutor::new(Vec::new()));
    let (tx_inbox, mut senders) = tokio::sync::mpsc::unbounded_channel::<ChunkSender>();
    let provider = Arc::new(ControllableStreamProvider { senders: tx_inbox });
    let agent = Arc::new(Agent::from_parts(
        std::path::PathBuf::from("/tmp/agent.md"),
        profile,
        "sys".to_string(),
        Arc::clone(&provider) as Arc<dyn model_provider::ModelProvider>,
        crate::agent::ModelConfigBuilder::new().stream(true).build(),
        Arc::clone(&executor),
        Arc::new(crate::mcp::McpManager::empty(executor)),
        None,
    ));
    let session = Session::new("s1".to_string(), "d".to_string());
    let (looper_side, caller_side) = make_async_intercom_pair::<LooperEvent, UserMsg>(64);
    let (event_speaker, user_listener) = looper_side.split();
    let (user_speaker, mut event_listener) = caller_side.split();
    let mut looper = AgentLooper::new(
        agent,
        Box::new(session),
        event_speaker,
        LooperConfig::default(),
        Arc::new(crate::persistence::NullSessionPersister),
    );
    let run_task = tokio::spawn(async move { looper.run(user_listener).await });

    user_speaker.send(UserMsg::Query("q".into())).await.unwrap();
    let tx = senders
        .recv()
        .await
        .expect("provider should start a stream");

    tx.send(Ok(text_start())).unwrap();
    tx.send(Ok(text_delta("hel"))).unwrap();

    let mut events = Vec::new();
    drain_until_text_delta_raw(&mut event_listener, &mut events, "hel").await;

    // 流式在途的 Pause 只记待办；等它被消费后再收尾本次调用
    user_speaker.send(UserMsg::Pause).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    // 收尾本次调用（含一个工具调用块）—— 落地暂停时轮仍在途、工具未启动
    tx.send(Ok(text_delta("lo"))).unwrap();
    tx.send(Ok(text_end("hello"))).unwrap();
    tx.send(Ok(tool_call_end("call-1", "unknown_tool", "{}")))
        .unwrap();
    tx.send(Ok(stream_usage())).unwrap();
    tx.send(Ok(stream_finish())).unwrap();

    let paused_idx = drain_until_paused_raw(&mut event_listener, &mut events).await;
    assert!(
        events[..=paused_idx]
            .iter()
            .all(|e| !matches!(e, LooperEvent::TurnComplete { .. })),
        "暂停落地时尚在途，不得已有 TurnComplete"
    );

    // 此刻外层已 Paused、轮未收尾 —— 关闭用户通道，按取消语义收尾
    drop(user_speaker);

    let res = tokio::time::timeout(Duration::from_secs(5), async {
        let mut turn_completes = 0usize;
        let mut failed_outcome: Option<TurnOutcome> = None;
        loop {
            match event_listener.recv().await {
                Some(LooperEvent::TurnComplete { outcome, .. }) => {
                    turn_completes += 1;
                    failed_outcome = Some(outcome);
                }
                Some(LooperEvent::Shutdown { reason, .. }) => {
                    break (turn_completes, failed_outcome, reason);
                }
                Some(_) => continue,
                None => panic!("event channel closed before Shutdown"),
            }
        }
    })
    .await;
    let (turn_completes, failed_outcome, reason) = res.expect("在途轮的收尾应在超时内完成并退出");
    assert_eq!(turn_completes, 1, "在途轮必须收尾恰好一次");
    match failed_outcome {
        Some(TurnOutcome::Failed {
            reason: TurnFailureReason::Cancelled,
            ..
        }) => {}
        other => panic!("收尾应为 Failed{{Cancelled}}，实际 {other:?}"),
    }
    assert_eq!(
        reason, "Cancelled",
        "Shutdown 原因应为 Cancelled，实际 {reason}"
    );

    run_task.await.unwrap().unwrap();
}

// 手工构造的用例直接持有 `event_listener`（无 LooperHandle），等待语义
// 与 handle 版助手逐字节一致，只是接收端换成了裸 `Listener`。
async fn drain_until_text_delta_raw(
    event_listener: &mut Listener<LooperEvent>,
    events: &mut Vec<LooperEvent>,
    want: &str,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match event_listener.recv().await {
                Some(e) => {
                    let hit = matches!(&e, LooperEvent::TextDelta { delta } if delta == want);
                    events.push(e);
                    if hit {
                        break;
                    }
                }
                None => panic!("event channel closed before TextDelta {want:?}"),
            }
        }
    })
    .await
    .expect("timed out waiting for TextDelta");
}

async fn drain_until_paused_raw(
    event_listener: &mut Listener<LooperEvent>,
    events: &mut Vec<LooperEvent>,
) -> usize {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match event_listener.recv().await {
                Some(e) => {
                    let paused = is_paused_event(&e);
                    events.push(e);
                    if paused {
                        break;
                    }
                }
                None => panic!("event channel closed before a paused state"),
            }
        }
    })
    .await
    .expect("timed out waiting for a paused state");
    events.len() - 1
}

/// 暂停中的输入只排队：真暂停态不得起轮（否则 `pause()` 的承诺失效、
/// 恢复语义被绕过），`Resume` 之后才续接。
#[tokio::test]
async fn test_paused_query_queues_until_resume() {
    let (h, mut senders) = spawn_controllable_looper();
    reach_paused_during_tools(&h, &mut senders).await;

    // 暂停中投递 query —— 只入队：不起轮、不迁移状态
    h.send_query("queued".into()).await.expect("looper alive");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let stray = h.drain_events();
    assert!(
        stray.is_empty(),
        "暂停中的 query 不得起轮，实际收到事件 {stray:?}"
    );

    // Resume 才续接：恰好 PausedDuringTools→ExecutingTools + 在途轮完成
    h.resume().await.expect("looper alive");
    let resumed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match h.recv_event().await {
                Some(LooperEvent::StateChange {
                    from: LooperState::PausedDuringTools,
                    to,
                    ..
                }) => {
                    assert_eq!(to, LooperState::ExecutingTools, "恢复必须单迁移到执行位");
                    break;
                }
                Some(_) => continue,
                None => panic!("event channel closed before resume transition"),
            }
        }
    })
    .await;
    resumed.expect("恢复迁移应在超时内到达");

    // 在途轮的第二次模型调用 → 本轮完成
    feed_text_stream(&mut senders, "hello").await;
    let first = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match h.recv_event().await {
                Some(LooperEvent::TurnComplete { outcome, .. }) => break outcome,
                Some(_) => continue,
                None => panic!("event channel closed before TurnComplete"),
            }
        }
    })
    .await;
    match first.expect("在途轮应在超时内完成") {
        TurnOutcome::Success { text } => assert_eq!(text, "hello"),
        other => panic!("恢复后在途轮应成功，实际 {other:?}"),
    }

    // 排队输入由 ③ 启动续接轮
    feed_text_stream(&mut senders, "ok").await;
    expect_queued_continuation(&h).await;

    h.shutdown().await.expect("clean shutdown");
}

/// 恢复瞬间再按停（`Resume` 与 `Pause` 同批投递）：真的落回暂停态 ——
/// 排队输入不起轮、镜像回 true，再一次 `Resume` 才续接。
#[tokio::test]
async fn test_pause_during_resume_lands_back_paused() {
    let (h, mut senders) = spawn_controllable_looper();
    reach_paused_during_tools(&h, &mut senders).await;

    // 暂停中排队一条输入；随后 Resume / Pause 背靠背投递 —— looper 醒来后
    // 一次 ① 按 FIFO 连续消费（无 yield 间隙），Resume 单迁移到执行位后
    // 同批的 Pause 立即把它压回暂停（spawn 前落地）
    h.send_query("queued".into()).await.expect("looper alive");
    h.resume().await.expect("looper alive");
    h.pause().await.expect("looper alive");

    tokio::time::sleep(Duration::from_millis(400)).await;
    let stray = h.drain_events();
    assert!(
        stray.iter().any(|e| matches!(
            e,
            LooperEvent::StateChange {
                from: LooperState::PausedDuringTools,
                to: LooperState::ExecutingTools,
                ..
            }
        )) && stray.iter().any(|e| matches!(
            e,
            LooperEvent::StateChange {
                from: LooperState::ExecutingTools,
                to: LooperState::PausedDuringTools,
                ..
            }
        )),
        "应观察到恢复单迁移后立即被压回暂停，实际收到事件 {stray:?}"
    );
    assert!(
        !stray
            .iter()
            .any(|e| matches!(e, LooperEvent::ToolCallStart { .. })),
        "压回暂停前不得 spawn 工具，实际收到事件 {stray:?}"
    );
    assert!(
        !stray
            .iter()
            .any(|e| matches!(e, LooperEvent::TurnComplete { .. })),
        "恢复瞬间的暂停不得起轮，实际收到事件 {stray:?}"
    );
    assert!(h.is_paused(), "同批的第二条 Pause 必须落回暂停");

    // 再一次 Resume 才续接
    h.resume().await.expect("looper alive");
    let resumed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match h.recv_event().await {
                Some(LooperEvent::StateChange {
                    from: LooperState::PausedDuringTools,
                    to: LooperState::ExecutingTools,
                    ..
                }) => break,
                Some(_) => continue,
                None => panic!("event channel closed before resume transition"),
            }
        }
    })
    .await;
    resumed.expect("再次恢复迁移应在超时内到达");

    // 在途轮第二次模型调用 → 完成
    feed_text_stream(&mut senders, "hello").await;
    let first = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match h.recv_event().await {
                Some(LooperEvent::TurnComplete { outcome, .. }) => break outcome,
                Some(_) => continue,
                None => panic!("event channel closed before TurnComplete"),
            }
        }
    })
    .await;
    match first.expect("再次恢复后应在超时内完成") {
        TurnOutcome::Success { text } => assert_eq!(text, "hello"),
        other => panic!("再次恢复后应成功，实际 {other:?}"),
    }

    // 排队输入续接轮
    feed_text_stream(&mut senders, "ok").await;
    expect_queued_continuation(&h).await;

    h.shutdown().await.expect("clean shutdown");
}

/// 暂停中排队的输入被取消后不丢：留到下一次武装时与新输入合并执行。
#[tokio::test]
async fn test_paused_queued_query_survives_cancel_until_next_arm() {
    let (h, mut senders) = spawn_controllable_looper();
    reach_paused_during_tools(&h, &mut senders).await;

    // 暂停中投递的 query 石沉大海（前端无排队回显）—— 但不丢
    h.send_query("stranded".into()).await.expect("looper alive");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let stray = h.drain_events();
    assert!(
        stray.is_empty(),
        "暂停中的 query 不得起轮，实际收到事件 {stray:?}"
    );

    h.cancel().await.expect("looper alive");
    recv_until_state(&h, LooperState::Idle).await;
    assert!(!h.is_paused(), "取消收尾后镜像必须复位");

    // 下一次武装：stranded 与 second 合并进同一条 user 消息
    h.send_query("second".into()).await.expect("looper alive");
    feed_text_stream(&mut senders, "ok").await;
    let res = tokio::time::timeout(Duration::from_secs(5), async {
        let mut input = String::new();
        loop {
            match h.recv_event().await {
                Some(LooperEvent::TurnStart { user_input, .. }) => input = user_input,
                Some(LooperEvent::TurnComplete { outcome, .. }) => break (input, outcome),
                Some(_) => continue,
                None => panic!("event channel closed before TurnComplete"),
            }
        }
    })
    .await;
    let (input, outcome) = res.expect("下一次武装应在超时内起轮并完成");
    assert!(
        input.contains("stranded") && input.contains("second"),
        "取消搁置的输入必须与新输入合并执行，实际 {input:?}"
    );
    match outcome {
        TurnOutcome::Success { text } => assert_eq!(text, "ok"),
        other => panic!("合并轮应成功，实际 {other:?}"),
    }

    h.shutdown().await.expect("clean shutdown");
}

// ── 暂停延迟到模型调用边界（流式在途）tests ──────────────────────────

type ChunkSender =
    tokio::sync::mpsc::UnboundedSender<Result<StreamChunk, model_provider::ProviderError>>;

/// 可控慢流 provider：每次 `generate_stream` 把发送端交回测试，由测试
/// 决定 chunk 投放时机，从而把 looper 确定地停在 `Streaming`。
struct ControllableStreamProvider {
    senders: tokio::sync::mpsc::UnboundedSender<ChunkSender>,
}

#[async_trait::async_trait]
impl model_provider::ModelProvider for ControllableStreamProvider {
    fn name(&self) -> &str {
        "controllable-stream"
    }

    async fn generate_full(
        &self,
        _request: &model_provider::GenerateRequest,
    ) -> Result<model_provider::GenerateResult, model_provider::ProviderError> {
        panic!("controllable provider is streaming-only; test harness misconfigured")
    }

    async fn generate_stream(
        &self,
        _request: &model_provider::GenerateRequest,
    ) -> Result<GenerateStream, model_provider::ProviderError> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.senders
            .send(tx)
            .expect("test dropped the chunk-sender inbox");
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(GenerateStream::new(Box::pin(stream)))
    }
}

/// spawn 一个可控慢流 looper，返回句柄与「流发送端收件箱」。
fn spawn_controllable_looper() -> (
    LooperHandle,
    tokio::sync::mpsc::UnboundedReceiver<ChunkSender>,
) {
    let profile: crate::agent::AgentProfile = serde_yaml::from_str(
        "agent:\n  name: t\n  description: d\nllm:\n  provider: p\n  model: m\n",
    )
    .unwrap();
    let executor: Arc<dyn crate::tools::ToolExecutor> =
        Arc::new(crate::tools::DefaultToolsExecutor::new(Vec::new()));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let provider = Arc::new(ControllableStreamProvider { senders: tx });
    let agent = Arc::new(Agent::from_parts(
        std::path::PathBuf::from("/tmp/agent.md"),
        profile,
        "sys".to_string(),
        Arc::clone(&provider) as Arc<dyn model_provider::ModelProvider>,
        crate::agent::ModelConfigBuilder::new().stream(true).build(),
        Arc::clone(&executor),
        Arc::new(crate::mcp::McpManager::empty(executor)),
        None,
    ));
    let session = Session::new("s1".to_string(), "d".to_string());
    let handle = AgentLooper::spawn(
        agent,
        Box::new(session),
        LooperConfig::default(),
        Arc::new(crate::persistence::NullSessionPersister),
    );
    (handle, rx)
}

fn text_start() -> StreamChunk {
    StreamChunk::BlockStart {
        index: 0,
        block_type: model_provider::BlockType::Text,
    }
}

fn text_delta(delta: &str) -> StreamChunk {
    StreamChunk::TextDelta {
        index: 0,
        delta: delta.to_string(),
    }
}

fn text_end(text: &str) -> StreamChunk {
    StreamChunk::BlockEnd {
        index: 0,
        block: ContentBlock::Text {
            text: text.to_string(),
        },
    }
}

fn tool_call_end(call_id: &str, name: &str, arguments: &str) -> StreamChunk {
    StreamChunk::BlockEnd {
        index: 1,
        block: ContentBlock::ToolCall {
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        },
    }
}

fn stream_usage() -> StreamChunk {
    StreamChunk::Usage {
        usage: Usage {
            input_tokens: 10,
            output_tokens: 20,
            total_tokens: 30,
        },
    }
}

fn stream_finish() -> StreamChunk {
    StreamChunk::Finish {
        reason: FinishReason::Stop,
    }
}

/// 等下一次模型调用被拉起，并喂一条纯文本收尾流（无 tool_calls）。
async fn feed_text_stream(
    senders: &mut tokio::sync::mpsc::UnboundedReceiver<ChunkSender>,
    text: &str,
) {
    let tx = tokio::time::timeout(Duration::from_secs(5), senders.recv())
        .await
        .expect("timed out waiting for a model call")
        .expect("provider should start a stream");
    tx.send(Ok(text_start())).unwrap();
    tx.send(Ok(text_delta(text))).unwrap();
    tx.send(Ok(text_end(text))).unwrap();
    tx.send(Ok(stream_usage())).unwrap();
    tx.send(Ok(stream_finish())).unwrap();
}

/// 发起查询 → 开流 → 流式在途打 Pause → 以 tool_calls 收尾 —— 确定地把
/// looper 停在 `PausedDuringTools`（spawn 前落地：暂停先被消费、工具尚未
/// 启动）。返回到达暂停态为止的全部事件。
async fn reach_paused_during_tools(
    h: &LooperHandle,
    senders: &mut tokio::sync::mpsc::UnboundedReceiver<ChunkSender>,
) -> Vec<LooperEvent> {
    h.send_query("q".into()).await.expect("looper alive");
    let tx = senders
        .recv()
        .await
        .expect("provider should start a stream");

    tx.send(Ok(text_start())).unwrap();
    tx.send(Ok(text_delta("hel"))).unwrap();

    let mut events = Vec::new();
    drain_until_text_delta(h, &mut events, "hel").await;

    // 消费 Pause 后再收尾本次调用 —— 保证落地检查点在 spawn 前看到意图
    h.pause().await.expect("looper alive");
    tokio::time::sleep(Duration::from_millis(400)).await;

    tx.send(Ok(text_delta("lo"))).unwrap();
    tx.send(Ok(text_end("hello"))).unwrap();
    tx.send(Ok(tool_call_end("call-1", "unknown_tool", "{}")))
        .unwrap();
    tx.send(Ok(stream_usage())).unwrap();
    tx.send(Ok(stream_finish())).unwrap();

    drain_until_paused(h, &mut events).await;
    assert!(
        h.is_paused(),
        "StateChange 到达时暂停镜像必须已置 true —— 状态/镜像/事件同帧有序"
    );
    events
}

/// 收事件直到出现指定文本的 `TextDelta`，全程累积到 `events`。
async fn drain_until_text_delta(h: &LooperHandle, events: &mut Vec<LooperEvent>, want: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match h.recv_event().await {
                Some(e) => {
                    let hit = matches!(&e, LooperEvent::TextDelta { delta } if delta == want);
                    events.push(e);
                    if hit {
                        break;
                    }
                }
                None => panic!("event channel closed before TextDelta {want:?}"),
            }
        }
    })
    .await
    .expect("timed out waiting for TextDelta");
}

fn is_paused_event(e: &LooperEvent) -> bool {
    matches!(
        e,
        LooperEvent::StateChange { to, .. } if to.is_paused()
    )
}

/// 收事件直到状态迁到暂停态，返回该事件在 `events` 中的下标。
async fn drain_until_paused(h: &LooperHandle, events: &mut Vec<LooperEvent>) -> usize {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match h.recv_event().await {
                Some(e) => {
                    let paused = is_paused_event(&e);
                    events.push(e);
                    if paused {
                        break;
                    }
                }
                None => panic!("event channel closed before a paused state"),
            }
        }
    })
    .await
    .expect("timed out waiting for a paused state");
    events.len() - 1
}

/// 收事件直到 `TurnComplete`，全程累积到 `events`，避免把中间事件丢弃。
async fn drain_until_turn_complete(h: &LooperHandle, events: &mut Vec<LooperEvent>) -> TurnOutcome {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match h.recv_event().await {
                Some(e) => {
                    let outcome = match &e {
                        LooperEvent::TurnComplete { outcome, .. } => Some(outcome.clone()),
                        _ => None,
                    };
                    events.push(e);
                    if let Some(outcome) = outcome {
                        break outcome;
                    }
                }
                None => panic!("event channel closed before TurnComplete"),
            }
        }
    })
    .await
    .expect("timed out waiting for TurnComplete")
}

/// 收续接轮收尾：断言本轮输入为排队的 `queued`、且成功产出 `ok`。
async fn expect_queued_continuation(h: &LooperHandle) {
    let res = tokio::time::timeout(Duration::from_secs(5), async {
        let mut input = String::new();
        loop {
            match h.recv_event().await {
                Some(LooperEvent::TurnStart { user_input, .. }) => input = user_input,
                Some(LooperEvent::TurnComplete { outcome, .. }) => break (input, outcome),
                Some(_) => continue,
                None => panic!("event channel closed before queued TurnComplete"),
            }
        }
    })
    .await;
    let (input, outcome) = res.expect("排队输入应在超时内续接完成");
    assert_eq!(input, "queued", "续接轮输入应为排队的那条");
    match outcome {
        TurnOutcome::Success { text } => assert_eq!(text, "ok"),
        other => panic!("排队输入的续接轮应成功，实际 {other:?}"),
    }
}

/// 流式在途收到 Pause、本次调用收尾没有 tool_calls：暂停意图被轮次完成
/// 吸收 —— 事件流里从不出现暂停态，排队输入照常续接。
#[tokio::test]
async fn test_pause_during_stream_defers_to_call_boundary() {
    let (h, mut senders) = spawn_controllable_looper();
    h.send_query("q".into()).await.expect("looper alive");
    let tx = senders
        .recv()
        .await
        .expect("provider should start a stream");

    tx.send(Ok(text_start())).unwrap();
    tx.send(Ok(text_delta("hel"))).unwrap();

    let mut events = Vec::new();
    drain_until_text_delta(&h, &mut events, "hel").await;

    // 模型调用在途：此刻发 Pause，再等过 `STEP_POLL`，确保 looper 已回到循环顶
    // 消费过 Pause（长等待只会加强断言）
    h.pause().await.expect("looper alive");
    tokio::time::sleep(Duration::from_millis(400)).await;
    events.extend(h.drain_events());

    assert!(!h.is_paused(), "流式在途的暂停不得立即生效");
    assert!(
        !events.iter().any(is_paused_event),
        "流未收尾前不得出现暂停态迁移"
    );

    // 暂停中排队一条 —— 吸收后由 ③ 无暂停地起续接轮
    h.send_query("queued".into()).await.expect("looper alive");

    // 收尾本次调用（无 tool_calls）→ 暂停意图被本轮完成吸收
    tx.send(Ok(text_delta("lo"))).unwrap();
    tx.send(Ok(text_end("hello"))).unwrap();
    tx.send(Ok(stream_usage())).unwrap();
    tx.send(Ok(stream_finish())).unwrap();

    match drain_until_turn_complete(&h, &mut events).await {
        TurnOutcome::Success { text } => assert_eq!(text, "hello"),
        other => panic!("被吸收的暂停不应阻止本轮完成，实际 {other:?}"),
    }
    assert!(
        !events.iter().any(is_paused_event),
        "无 tool_calls 收尾时暂停意图被吸收，全程不得出现暂停态迁移"
    );
    assert!(!h.is_paused(), "吸收后不得残留暂停态");

    // 排队轮自动起 —— 意图随轮起点复位，续接轮不受上一轮挂起影响
    feed_text_stream(&mut senders, "ok").await;
    expect_queued_continuation(&h).await;
    assert!(
        !events.iter().any(is_paused_event),
        "续接轮同样不得出现暂停态迁移"
    );

    h.shutdown().await.expect("clean shutdown");
}

/// 流结束前 Resume 撤回待办：事件流里从不出现暂停态，本轮照常推进到工具执行。
///
/// 用「带 tool_calls 收尾」构造辨别力 —— 若 `Resume` 未撤回，落地点 B（spawn 前）
/// 会把本轮停成 `PausedDuringTools`；撤回后则直接 spawn 工具。
#[tokio::test]
async fn test_resume_before_stream_end_retracts_pending_pause() {
    let (h, mut senders) = spawn_controllable_looper();
    h.send_query("q".into()).await.expect("looper alive");
    let tx = senders
        .recv()
        .await
        .expect("provider should start a stream");

    tx.send(Ok(text_start())).unwrap();
    tx.send(Ok(text_delta("hel"))).unwrap();

    let mut events = Vec::new();
    drain_until_text_delta(&h, &mut events, "hel").await;

    // 在途暂停后立刻恢复 —— 待办被撤回，落地点 B 不得再挂起
    h.pause().await.expect("looper alive");
    h.resume().await.expect("looper alive");
    tokio::time::sleep(Duration::from_millis(400)).await;

    tx.send(Ok(text_delta("lo"))).unwrap();
    tx.send(Ok(text_end("hello"))).unwrap();
    tx.send(Ok(tool_call_end("call-1", "unknown_tool", "{}")))
        .unwrap();
    tx.send(Ok(stream_usage())).unwrap();
    tx.send(Ok(stream_finish())).unwrap();

    // 撤回后本轮不暂停，直接进入工具执行位（spawn 工具）
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match h.recv_event().await {
                Some(e) => {
                    let hit = matches!(e, LooperEvent::ToolCallStart { .. });
                    events.push(e);
                    if hit {
                        break;
                    }
                }
                None => panic!("event channel closed before ToolCallStart"),
            }
        }
    })
    .await
    .expect("撤回后应直接 spawn 工具（超时仍未启动）");

    assert!(
        !events.iter().any(is_paused_event),
        "被撤回的暂停不得在事件流中出现暂停态迁移"
    );
    assert!(!h.is_paused(), "撤回后不得残留暂停态");

    // 工具执行后本轮会再发起一次模型调用：喂一个能收尾的流，避免悬挂
    let tx2 = senders
        .recv()
        .await
        .expect("provider should start a follow-up stream");
    tx2.send(Ok(text_start())).unwrap();
    tx2.send(Ok(text_delta("done"))).unwrap();
    tx2.send(Ok(text_end("done"))).unwrap();
    tx2.send(Ok(stream_usage())).unwrap();
    tx2.send(Ok(stream_finish())).unwrap();

    match drain_until_turn_complete(&h, &mut events).await {
        TurnOutcome::Success { text } => assert_eq!(text, "done"),
        other => panic!("撤回暂停后本轮应正常完成，实际 {other:?}"),
    }

    h.shutdown().await.expect("clean shutdown");
}

/// 流式在途的 Pause 只记挂起意图、不改状态、不置镜像。
#[tokio::test]
async fn test_streaming_pause_records_pending_without_pausing() {
    let mut h = failure_looper_harness(false, false, false);
    h.looper.set_quiet(LooperState::Streaming);

    let _ = h.looper.handle_user_msg(UserMsg::Pause);

    assert!(h.looper.pause_pending, "流式在途的暂停必须记为挂起意图");
    assert_eq!(
        h.looper.state(),
        LooperState::Streaming,
        "挂起意图不得改动状态"
    );
    assert!(
        !h.looper.pause_mirror.load(Ordering::Acquire),
        "挂起阶段不得置暂停镜像"
    );
}

/// 撤回尚未落地的暂停意图：流式在途收到 Resume 时清 `pause_pending`，
/// 不改状态、不动镜像。
#[tokio::test]
async fn test_resume_retracts_pending_pause() {
    let mut h = failure_looper_harness(false, false, false);
    h.looper.set_quiet(LooperState::Streaming);
    let _ = h.looper.handle_user_msg(UserMsg::Pause);
    assert!(h.looper.pause_pending, "流式在途的暂停必须记为挂起意图");

    let _ = h.looper.handle_user_msg(UserMsg::Resume);

    assert!(!h.looper.pause_pending, "Resume 必须撤回待办暂停");
    assert_eq!(h.looper.state(), LooperState::Streaming);
    assert!(!h.looper.pause_mirror.load(Ordering::Acquire));
}

/// 暂停的落地早于工具执行：收尾（内含工具调用）后先挂起，resume 才启动工具。
#[tokio::test]
async fn test_deferred_pause_lands_before_tool_execution() {
    let (h, mut senders) = spawn_controllable_looper();
    h.send_query("q".into()).await.expect("looper alive");
    let tx = senders
        .recv()
        .await
        .expect("provider should start a stream");

    tx.send(Ok(text_start())).unwrap();
    tx.send(Ok(text_delta("hel"))).unwrap();

    let mut events = Vec::new();
    drain_until_text_delta(&h, &mut events, "hel").await;

    h.pause().await.expect("looper alive");
    tokio::time::sleep(Duration::from_millis(400)).await;

    tx.send(Ok(text_delta("lo"))).unwrap();
    tx.send(Ok(text_end("hello"))).unwrap();
    tx.send(Ok(tool_call_end("call-1", "unknown_tool", "{}")))
        .unwrap();
    tx.send(Ok(stream_usage())).unwrap();
    tx.send(Ok(stream_finish())).unwrap();

    let paused_idx = drain_until_paused(&h, &mut events).await;
    assert!(
        matches!(
            &events[paused_idx],
            LooperEvent::StateChange {
                to: LooperState::PausedDuringTools,
                ..
            }
        ),
        "落点必须是 PausedDuringTools（spawn 前），实际 {:?}",
        events[paused_idx]
    );
    assert!(
        !events[..=paused_idx]
            .iter()
            .any(|e| matches!(e, LooperEvent::ToolCallStart { .. })),
        "暂停落地前不得启动工具"
    );

    h.resume().await.expect("looper alive");

    // resume 之后才启动工具
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match h.recv_event().await {
                Some(e) => {
                    let hit = matches!(e, LooperEvent::ToolCallStart { .. });
                    events.push(e);
                    if hit {
                        break;
                    }
                }
                None => panic!("event channel closed before ToolCallStart"),
            }
        }
    })
    .await
    .expect("timed out waiting for ToolCallStart");

    // 工具执行后本轮会再发起一次模型调用：喂一个能收尾的流，避免悬挂
    let tx2 = senders
        .recv()
        .await
        .expect("provider should start a follow-up stream");
    tx2.send(Ok(text_start())).unwrap();
    tx2.send(Ok(text_delta("done"))).unwrap();
    tx2.send(Ok(text_end("done"))).unwrap();
    tx2.send(Ok(stream_usage())).unwrap();
    tx2.send(Ok(stream_finish())).unwrap();

    match drain_until_turn_complete(&h, &mut events).await {
        TurnOutcome::Success { text } => assert_eq!(text, "done"),
        other => panic!("工具轮收尾后应正常完成，实际 {other:?}"),
    }

    h.shutdown().await.expect("clean shutdown");
}

/// Resume 从暂停态重入执行位后，spawn 阶段只跑 `result == None` 的槽位 ——
/// 断点续执行：已完成项不重跑、不重复发 `ToolCallStart`。
#[tokio::test]
async fn test_resume_spawn_skips_completed_slots() {
    let mut h = failure_looper_harness(false, false, false);

    let mut slots = pending_slots(2);
    slots[0].result = Some(tool_result(0).1);
    h.looper.react_ctx.pending_tool_calls = slots;
    h.looper.set_quiet(LooperState::PausedDuringTools);

    let _ = h.looper.handle_user_msg(UserMsg::Resume);
    assert_eq!(
        h.looper.state(),
        LooperState::ExecutingTools,
        "Resume 必须单迁移到执行位（spawn 阶段重入）"
    );

    // 步进直到离开执行位（spawn → poll → 收尾）
    let mut steps = 0;
    while h.looper.state() == LooperState::ExecutingTools {
        h.looper.react_step().await;
        steps += 1;
        assert!(steps < 10, "执行位步进不收敛");
    }
    assert_eq!(
        h.looper.state(),
        LooperState::PreparingRequest,
        "工具批完成后进入下一次请求前"
    );

    // 只对未完成的 c1 发过 ToolCallStart（c0 已完成，不重跑）
    let mut started = Vec::new();
    while let Ok(ev) = h.events.try_recv() {
        if let LooperEvent::ToolCallStart { id, .. } = ev {
            started.push(id);
        }
    }
    assert_eq!(
        started,
        vec!["c1".to_string()],
        "已完成的 c0 不得重跑，实际 {started:?}"
    );

    // 结果面：c0 保留原结果 r0（未被覆盖），c1 拿到执行结果
    let staged = h.looper.session().staging_messages();
    assert!(
        staged.iter().any(|am| matches!(
            am.message.as_ref(),
            InputItem::FunctionCallOutput { call_id, output }
                if call_id == "c0" && matches!(output, Content::Text(t) if t == "r0")
        )),
        "c0 必须保留原结果 r0（未重跑），实际={:?}",
        staged.iter().map(|am| &am.message).collect::<Vec<_>>()
    );
    assert!(
        staged.iter().any(|am| matches!(
            am.message.as_ref(),
            InputItem::FunctionCallOutput { call_id, .. } if call_id == "c1"
        )),
        "c1 必须拿到执行结果"
    );
}

/// 待办暂停跨瞬时重试保留：重发被落点拦截 —— 退避重发**之前**先落
/// `PausedBeforeRequest`（重发流不启动），`Resume` 后才重发并完成。
#[tokio::test]
async fn test_deferred_pause_survives_transient_retry() {
    let (h, mut senders) = spawn_controllable_looper();
    h.send_query("q".into()).await.expect("looper alive");
    let tx = senders
        .recv()
        .await
        .expect("provider should start a stream");

    tx.send(Ok(text_start())).unwrap();
    tx.send(Ok(text_delta("hel"))).unwrap();

    let mut events = Vec::new();
    drain_until_text_delta(&h, &mut events, "hel").await;

    h.pause().await.expect("looper alive");
    tokio::time::sleep(Duration::from_millis(400)).await;

    tx.send(Err(model_provider::ProviderError::Stream(
        "transient blip".into(),
    )))
    .unwrap();

    // 重发安排（begin_retry 置回 PreparingRequest）后、请求发出前拦截落地
    let paused_idx = drain_until_paused(&h, &mut events).await;
    assert!(
        matches!(
            &events[paused_idx],
            LooperEvent::StateChange {
                to: LooperState::PausedBeforeRequest,
                ..
            }
        ),
        "重发前的落点必须是 PausedBeforeRequest，实际 {:?}",
        events[paused_idx]
    );

    // 落地即冻结重发：重发流不得启动
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(senders.try_recv().is_err(), "暂停落地后不得发出重发请求");
    assert!(h.is_paused(), "落地后镜像必须为 true");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, LooperEvent::TurnComplete { .. })),
        "重发前的暂停不得收尾本轮"
    );

    // Resume 后重发：退避 + 新的模型调用
    h.resume().await.expect("looper alive");
    let tx2 = tokio::time::timeout(Duration::from_secs(5), senders.recv())
        .await
        .expect("timed out waiting for the retry stream")
        .expect("provider should start a retry stream");
    tx2.send(Ok(text_start())).unwrap();
    tx2.send(Ok(text_delta("hello"))).unwrap();
    tx2.send(Ok(text_end("hello"))).unwrap();
    tx2.send(Ok(stream_usage())).unwrap();
    tx2.send(Ok(stream_finish())).unwrap();

    match drain_until_turn_complete(&h, &mut events).await {
        TurnOutcome::Success { text } => assert_eq!(text, "hello"),
        other => panic!("重试恢复后应正常完成，实际 {other:?}"),
    }

    h.shutdown().await.expect("clean shutdown");
}

/// 重试窗口内再按一次 `Pause`：第一条意图已随落点在重发前落地，第二条
/// 在暂停态上幂等 —— 全程恰好一条暂停迁移，一次 `Resume` 即可恢复。
#[tokio::test]
async fn test_second_pause_during_retry_lands_immediately() {
    let (h, mut senders) = spawn_controllable_looper();
    h.send_query("q".into()).await.expect("looper alive");
    let tx = senders
        .recv()
        .await
        .expect("provider should start a stream");

    tx.send(Ok(text_start())).unwrap();
    tx.send(Ok(text_delta("hel"))).unwrap();

    let mut events = Vec::new();
    drain_until_text_delta(&h, &mut events, "hel").await;

    // 流式在途的第一条 Pause：只记挂起意图（重发/待办本身必须无事件）
    h.pause().await.expect("looper alive");
    tokio::time::sleep(Duration::from_millis(400)).await;
    events.extend(h.drain_events());
    assert!(
        !events.iter().any(is_paused_event),
        "流式在途的挂起意图不得落地"
    );

    // 瞬时失败触发重发（begin_retry 置回 PreparingRequest），紧接着投第二条
    // Pause —— 无论它先于还是后于落点消费，意图已存在 / 已在暂停态，幂等
    tx.send(Err(model_provider::ProviderError::Stream(
        "transient blip".into(),
    )))
    .unwrap();
    h.pause().await.expect("looper alive");

    // 落点在重发前拦截：恰好一条暂停迁移
    drain_until_paused(&h, &mut events).await;
    assert_eq!(
        events.iter().filter(|e| is_paused_event(e)).count(),
        1,
        "第二条 Pause 必须幂等，只落出一条暂停迁移"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, LooperEvent::TurnComplete { .. })),
        "重发前的暂停不得收尾本轮"
    );
    assert!(h.is_paused(), "落地后镜像必须为 true");

    // 一次 Resume 即恢复：退避后重发
    h.resume().await.expect("looper alive");
    let tx2 = tokio::time::timeout(Duration::from_secs(5), senders.recv())
        .await
        .expect("timed out waiting for the retry stream")
        .expect("provider should start a retry stream");
    tx2.send(Ok(text_start())).unwrap();
    tx2.send(Ok(text_delta("hello"))).unwrap();
    tx2.send(Ok(text_end("hello"))).unwrap();
    tx2.send(Ok(stream_usage())).unwrap();
    tx2.send(Ok(stream_finish())).unwrap();

    match drain_until_turn_complete(&h, &mut events).await {
        TurnOutcome::Success { text } => assert_eq!(text, "hello"),
        other => panic!("恢复后重试应正常完成，实际 {other:?}"),
    }
    assert_eq!(
        events.iter().filter(|e| is_paused_event(e)).count(),
        1,
        "全程只允许出现一条暂停迁移"
    );

    h.shutdown().await.expect("clean shutdown");
}

#[tokio::test]
async fn test_truncation_retry_resends_with_raised_budget() {
    let mut h = retry_harness(
        vec![truncated("part"), completed("done")],
        LooperConfig {
            retry_limit: 1,
            retry_output_budget: 32_768,
            ..Default::default()
        },
    );

    drive_query(&mut h).await;

    assert_eq!(
        h.provider.budgets(),
        vec![None, Some(32_768)],
        "首次用配置预算；重试必须带上抬高的预算"
    );
    let outcomes = h.turn_outcomes();
    assert_eq!(outcomes.len(), 1, "必须恰好收尾一次");
    match &outcomes[0] {
        TurnOutcome::Success { text, .. } => {
            assert_eq!(text, "done", "截断那次的文本不得残留");
        }
        other => panic!("重试成功应收敛为 Success，实际 {other:?}"),
    }
    assert_eq!(h.committed_turns(), 1, "重试在同一轮内完成，只提交一轮");
    assert_eq!(h.looper.session().turn_index(), 1);
    assert!(
        h.looper.session().staging_messages().is_empty(),
        "轮次已提交，staging 必须已清空"
    );
}

#[tokio::test]
async fn test_truncation_retry_exhausted_falls_back_to_failed() {
    let mut h = retry_harness(
        vec![truncated("first"), truncated("second")],
        LooperConfig {
            retry_limit: 1,
            retry_output_budget: 32_768,
            ..Default::default()
        },
    );

    drive_query(&mut h).await;

    assert_eq!(
        h.provider.budgets().len(),
        2,
        "limit=1 恰好重试一次，不得继续重试"
    );
    let outcomes = h.turn_outcomes();
    assert_eq!(outcomes.len(), 1);
    match &outcomes[0] {
        TurnOutcome::Failed { reason, .. } => {
            let msg = format!("{reason:?}");
            assert!(msg.contains("truncated"), "失败原因应指向截断；实际 {msg}");
        }
        other => panic!("重试耗尽应回落为 Failed，实际 {other:?}"),
    }
    assert_eq!(h.committed_turns(), 1, "重试耗尽后仍冻结入史，与改动前一致");
}

#[tokio::test]
async fn test_truncation_retry_disabled_by_zero_limit() {
    let mut h = retry_harness(
        vec![truncated("part")],
        LooperConfig {
            retry_limit: 0,
            retry_output_budget: 32_768,
            ..Default::default()
        },
    );

    drive_query(&mut h).await;

    assert_eq!(h.provider.budgets().len(), 1, "limit=0 必须完全关闭重试");
    assert!(matches!(
        h.turn_outcomes().as_slice(),
        [TurnOutcome::Failed { .. }]
    ));
}

#[tokio::test]
async fn test_truncation_retry_skipped_when_budget_already_above_floor() {
    // 抬不动预算 = 重试请求逐字节不变、必然同样截断，白烧一次调用。
    let mut h = retry_harness_with_budget(
        vec![truncated("part")],
        LooperConfig {
            retry_limit: 1,
            retry_output_budget: 32_768,
            ..Default::default()
        },
        Some(40_000),
    );

    drive_query(&mut h).await;

    assert_eq!(
        h.provider.budgets(),
        vec![Some(40_000)],
        "预算已高于下限则不重试，且首次请求就带上配置值"
    );
    assert!(matches!(
        h.turn_outcomes().as_slice(),
        [TurnOutcome::Failed { .. }]
    ));
}

#[tokio::test]
async fn test_truncation_retry_ignores_non_max_tokens_status() {
    // Aborted → status Failed（不是 Incomplete），抬预算救不回来。
    let mut h = retry_harness(
        vec![text_chunks("cut", 100, FinishReason::Aborted)],
        LooperConfig {
            retry_limit: 1,
            retry_output_budget: 32_768,
            ..Default::default()
        },
    );

    drive_query(&mut h).await;

    assert_eq!(h.provider.budgets().len(), 1, "Aborted 不得触发重试");
    assert!(matches!(
        h.turn_outcomes().as_slice(),
        [TurnOutcome::Failed { .. }]
    ));
}

#[tokio::test]
async fn test_truncation_retry_request_error_discards_text_and_keeps_turn() {
    // 抬高的预算若超过模型真实上限，重试请求本身会失败（400）。
    // 被回退的截断文本**只丢弃不归还**：partial_text 为空、
    // 历史里不得再出现它；但轮次必须照常冻结（存活桩兜底），否则
    // interrupt_turn 退化成 rollback，用户提问连同整轮一起从历史消失。
    // 桩的具体形态由 `retry_then_failure_freezes_turn_with_stub` 钉住，
    // 本用例钉住「丢弃」这一半。
    let mut h = retry_harness(
        vec![
            truncated("salvaged"),
            Script::Fail(model_provider::ProviderError::Api {
                status: 400,
                body: "max_output_tokens too large".to_string(),
            }),
        ],
        LooperConfig {
            retry_limit: 1,
            retry_output_budget: 32_768,
            retry_base_delay_ms: 1,
            retry_max_delay_ms: 2,
            ..Default::default()
        },
    );

    drive_query(&mut h).await;

    assert_eq!(h.provider.budgets(), vec![None, Some(32_768)]);
    let outcomes = h.turn_outcomes();
    assert_eq!(outcomes.len(), 1, "必须恰好收尾一次");
    match &outcomes[0] {
        TurnOutcome::Failed { partial_text, .. } => {
            assert_eq!(
                partial_text, "",
                "被回退的截断文本不得作为 partial_text 归还"
            );
        }
        other => panic!("应收敛为 Failed，实际 {other:?}"),
    }
    assert!(h.looper.session().is_idle(), "收尾后必须回到 Idle");
    assert_eq!(
        h.committed_turns(),
        1,
        "重试失败仍须冻结整轮，不得退化成 rollback"
    );
    assert!(
        !h.looper
            .session()
            .committed_turns()
            .iter()
            .flatten()
            .any(|am| matches!(
                am.message.as_ref(),
                InputItem::Message { content, .. } if content.text_view() == "salvaged"
            )),
        "被丢弃的截断文本不得进历史"
    );
}

#[tokio::test]
async fn test_can_retry_truncation_no_headroom_is_false() {
    // 没有下一次模型调用额度时不重试：否则状态机刚被置回 PreparingRequest
    // 就撞上 MaxIterationsExceeded，把「截断」这个真实原因换掉。
    let mut h = retry_harness(
        vec![],
        LooperConfig {
            retry_limit: 1,
            retry_output_budget: 32_768,
            ..Default::default()
        },
    );
    h.looper.react_loop_iteration = h.looper.max_iterations;
    assert!(
        !h.looper
            .can_retry(&RetryCause::Truncated { output_tokens: 1 }),
        "无迭代余量必须不重试"
    );
}

#[test]
fn test_max_output_tokens_override_is_sticky_within_turn() {
    // 粘性：本轮发生过重试后，后续 ReAct 迭代继续用抬高的预算，
    // 否则 tool 调用后的下一次迭代可能同样截断。
    let mut h = retry_harness(
        vec![],
        LooperConfig {
            retry_limit: 1,
            retry_output_budget: 32_768,
            ..Default::default()
        },
    );
    assert_eq!(h.looper.max_output_tokens_override(), None);

    h.looper.budget_raised = true;
    assert_eq!(h.looper.max_output_tokens_override(), Some(32_768));

    // 计数器随新用户轮归零，粘性不出轮
    h.looper.reset_iteration_counters();
    assert_eq!(h.looper.max_output_tokens_override(), None);
}

#[test]
fn test_can_retry_truncation_false_once_budget_already_raised() {
    // limit >= 2 时，第二次重试的请求与第一次逐字节相同（预算已是抬高值），
    // 守卫必须挡住 —— 否则白烧一次调用和一次 react_loop_iteration。
    // 守卫因此比的是当前生效预算，不是配置值。
    let mut h = retry_harness_with_budget(
        vec![],
        LooperConfig {
            retry_limit: 3,
            retry_output_budget: 32_768,
            ..Default::default()
        },
        Some(4096),
    );

    let cause = RetryCause::Truncated { output_tokens: 1 };
    assert!(h.looper.can_retry(&cause), "首次：4096 < 32768，抬得动");

    // 单靠次数不抬预算（瞬时重发也走同一计数）—— 抬预算的是独立布尔。
    h.looper.retries_used = 1;
    assert!(
        h.looper.can_retry(&cause),
        "只耗过瞬时额度、预算未抬时仍抬得动"
    );

    h.looper.budget_raised = true;
    assert!(
        !h.looper.can_retry(&cause),
        "生效预算已是 32768，再重试只会发出同样的请求"
    );
}

#[tokio::test]
async fn test_batch_truncation_retry_resends_with_raised_budget() {
    let retry_cfg = LooperConfig {
        retry_limit: 1,
        retry_output_budget: 32_768,
        ..Default::default()
    };
    let mut h = batch_retry_harness(
        vec![
            batch_response(
                "part",
                4096,
                model_provider::ResponseStatus::Incomplete,
                Some(FinishReason::MaxTokens),
            ),
            batch_response(
                "done",
                20,
                model_provider::ResponseStatus::Completed,
                Some(FinishReason::Stop),
            ),
        ],
        retry_cfg,
    );

    drive_query(&mut h).await;

    assert_eq!(h.provider.budgets(), vec![None, Some(32_768)]);
    match &h.turn_outcomes()[0] {
        TurnOutcome::Success { text, .. } => assert_eq!(text, "done"),
        other => panic!("重试成功应收敛为 Success，实际 {other:?}"),
    }
}

#[tokio::test]
async fn test_batch_content_filter_does_not_retry() {
    // 收窄前的判据是裸 `Incomplete`，content_filter 也会被当成截断重发一次
    // —— 抬预算救不回来，纯白烧一次调用。`GenerateResult::finish_reason`
    // 让批量路径能做出与流式路径同样的区分。
    //
    // 上游没给原因（`finish_reason == None`）走同一条 else 分支，同样不重试 ——
    // 「不臆测成截断」由本用例一并守住。
    let mut h = batch_retry_harness(
        vec![batch_response(
            "filtered",
            12,
            model_provider::ResponseStatus::Incomplete,
            Some(FinishReason::Error),
        )],
        LooperConfig {
            retry_limit: 1,
            retry_output_budget: 32_768,
            ..Default::default()
        },
    );

    drive_query(&mut h).await;

    assert_eq!(h.provider.budgets().len(), 1, "content_filter 不得触发重试");
    assert!(matches!(
        h.turn_outcomes().as_slice(),
        [TurnOutcome::Failed { .. }]
    ));
}

#[tokio::test]
async fn test_truncation_retry_emits_notice_before_retried_text() {
    let mut h = retry_harness(
        vec![truncated("part"), completed("done")],
        LooperConfig {
            retry_limit: 1,
            retry_output_budget: 32_768,
            ..Default::default()
        },
    );

    drive_query(&mut h).await;
    let events = h.drain_events();

    // ── 通知本身：恰好一条，四个数字取自实际请求 ──
    let notices = truncation_notices(&events);
    assert_eq!(notices.len(), 1, "limit=1 恰好一条通知");
    let (notice_at, attempt, limit, output_tokens, retry_budget, discarded) = &notices[0];
    assert_eq!(*attempt, 1);
    assert_eq!(*limit, 1);
    assert_eq!(*output_tokens, 4096, "截断那次的输出 token 数");
    assert_eq!(*retry_budget, 32_768, "抬升后的预算");
    assert_eq!(discarded, "part", "载荷是被丢弃那次尝试的正文");

    // ── 顺序：通知必须早于重试尝试的第一条增量 ──
    // 前端据此把横幅插在残句与新正文之间；晚于增量就会插到答案下面。
    let second_delta_at = events
        .iter()
        .enumerate()
        .filter(|(_, ev)| matches!(ev, LooperEvent::TextDelta { .. }))
        .nth(1)
        .map(|(i, _)| i)
        .expect("重试尝试必须发出第二条 TextDelta");
    assert!(
        *notice_at < second_delta_at,
        "通知（下标 {notice_at}）必须早于重试的增量（下标 {second_delta_at}）"
    );

    // ── 落库侧的剥离不变量 ──
    // 通知到来这一刻，累加器必然**以载荷结尾** —— 该次尝试的增量是最后
    // 追加进累加器的内容，之后到通知之间不可能再有别的 delta（之间只隔
    // 一个 Finish / Usage 块）。落库侧按后缀剥离依赖的正是这条。
    //
    // 注意残句在**最终**画面里是前缀（后面还跟着重试的正文），所以剥离
    // 只能发生在通知这一刻，不能等到轮次结束再对着最终文本做。
    let up_to_notice = streamed_text(&events[..*notice_at]);
    assert_eq!(up_to_notice, "part");
    assert!(
        up_to_notice.ends_with(discarded.as_str()),
        "剥离不变量：通知到达时累加器以载荷结尾"
    );

    // ── 取舍的显式记录：客户端拿到的增量串是「残句 + 新正文」──
    // 本设计**不**在传输层删除已下发的增量，靠通知解释这段残句。
    // 断言最终串，避免将来有人「顺手」在 core 侧把残句吞掉而测试仍全绿。
    assert_eq!(streamed_text(&events), "partdone");
}

#[tokio::test]
async fn test_truncation_retry_notice_carries_reasoning_only_attempt() {
    // 正文为空、只有推理被吐出来的那次尝试同样要通知：画面上确实多了一段
    // 属于已作废尝试的推理内容。载荷正文为空，落库侧剥离是空操作。
    let mut h = retry_harness(
        vec![
            Script::Chunks(vec![
                StreamChunk::BlockStart {
                    index: 0,
                    block_type: model_provider::BlockType::Reasoning,
                },
                StreamChunk::ReasoningDelta {
                    index: 0,
                    delta: "想想".to_string(),
                },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Reasoning {
                        text: "想想".to_string(),
                    },
                },
                StreamChunk::Finish {
                    reason: FinishReason::MaxTokens,
                },
            ]),
            completed("done"),
        ],
        LooperConfig {
            retry_limit: 1,
            retry_output_budget: 32_768,
            ..Default::default()
        },
    );

    drive_query(&mut h).await;
    let events = h.drain_events();

    let notices = truncation_notices(&events);
    assert_eq!(notices.len(), 1, "只有推理产出也要通知");
    assert_eq!(notices[0].5, "", "载荷正文为空");
}

#[tokio::test]
async fn test_truncation_retry_notice_absent_on_zero_output() {
    // 截断但什么都没吐：没有需要解释的残句，发出去只会让前端插一条
    // 无上下文的横幅。重试本身照常进行。
    let mut h = retry_harness(
        vec![truncated(""), completed("done")],
        LooperConfig {
            retry_limit: 1,
            retry_output_budget: 32_768,
            ..Default::default()
        },
    );

    drive_query(&mut h).await;
    let events = h.drain_events();

    assert!(truncation_notices(&events).is_empty(), "零产出不得发通知");
    assert_eq!(
        h.provider.budgets().len(),
        2,
        "零产出仍要重试 —— 不发通知只是不发通知"
    );
}

#[tokio::test]
async fn test_batch_truncation_retry_emits_notice() {
    // 批量路径不发 delta，客户端没有残句可解释；通知照发（事件陈述的是
    // 「本次尝试作废」这个事实），落库侧的载荷是该次尝试的整段文本。
    let mut h = batch_retry_harness(
        vec![
            batch_response(
                "part",
                4096,
                model_provider::ResponseStatus::Incomplete,
                Some(FinishReason::MaxTokens),
            ),
            batch_response(
                "done",
                20,
                model_provider::ResponseStatus::Completed,
                Some(FinishReason::Stop),
            ),
        ],
        LooperConfig {
            retry_limit: 1,
            retry_output_budget: 32_768,
            ..Default::default()
        },
    );

    drive_query(&mut h).await;
    let events = h.drain_events();

    let notices = truncation_notices(&events);
    assert_eq!(notices.len(), 1, "批量路径同样发通知");
    assert_eq!(notices[0].5, "part", "载荷是整段文本，供落库侧剥离");
    assert!(
        streamed_text(&events).is_empty(),
        "批量路径本就不发 delta，客户端无残句"
    );
}

#[tokio::test]
async fn test_backoff_deadline_survives_future_drop() {
    // 防御性不变式：deadline 必须留在 self 上（只读等待），future 被 drop
    // 后重入继续等剩余时间 —— take() 会把它带进 future 栈帧，drop 即丢失
    // → 零退避立即重发。（run() 已不抢占在途 future；本测试手动 select
    // 模拟 drop 来守护该不变式。）
    let mut h = retry_harness(vec![], LooperConfig::default());
    h.looper.retry_deadline = Some(Instant::now() + Duration::from_millis(150));

    let started = Instant::now();
    tokio::select! {
        _ = h.looper.wait_retry_backoff() => panic!("退避不应在抢占窗口内完成"),
        _ = tokio::time::sleep(Duration::from_millis(30)) => {}
    }
    assert!(
        h.looper.retry_deadline.is_some(),
        "future 被 drop 不得丢失 deadline"
    );

    assert!(
        h.looper.wait_retry_backoff().await,
        "无人取消时退避应正常等满"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(130),
        "重入必须等满剩余退避（幂等），实际 {:?}",
        started.elapsed()
    );
}

// ── 瞬时故障重发与类型化失败原因 ──────────────────────────────────────

/// 瞬时重发测试用的快退避配置（避免测试真的等 500ms+）。
fn fast_transient_config(limit: u32) -> LooperConfig {
    LooperConfig {
        retry_limit: limit,
        retry_base_delay_ms: 1,
        retry_max_delay_ms: 2,
        ..Default::default()
    }
}

fn api_err(status: u16, body: &str) -> model_provider::ProviderError {
    model_provider::ProviderError::Api {
        status,
        body: body.to_string(),
    }
}

#[tokio::test]
async fn test_batch_rate_limited_retries_then_fails_with_typed_reason() {
    // 429 是瞬时类：重发 retry_limit 次后放弃，
    // 失败原因是类型化的 RateLimited，且保留 provider 原始 body。
    let body = r#"{"error":{"message":"Rate limit reached for deepseek-v4","code":"rate_limit_exceeded"}}"#;
    let mut h = batch_retry_harness(
        vec![
            Script::Fail(api_err(429, body)),
            Script::Fail(api_err(429, body)),
            Script::Fail(api_err(429, body)),
        ],
        fast_transient_config(2),
    );

    drive_query(&mut h).await;

    assert_eq!(
        h.provider.budgets().len(),
        3,
        "首次 + 2 次瞬时重发 = 3 次请求"
    );
    let outcomes = h.turn_outcomes();
    assert_eq!(outcomes.len(), 1, "必须恰好收尾一次");
    match &outcomes[0] {
        TurnOutcome::Failed { reason, .. } => match reason {
            TurnFailureReason::RateLimited { attempts, message } => {
                assert_eq!(*attempts, 3, "attempts = 实际发起的总请求数");
                assert!(
                    message.contains("Rate limit reached"),
                    "原始 msg 必须保留，实际 {message}"
                );
            }
            other => panic!("429 耗尽应收敛为 RateLimited，实际 {other:?}"),
        },
        other => panic!("期望 Failed，实际 {other:?}"),
    }
}

#[tokio::test]
async fn test_batch_auth_error_fails_without_retry() {
    // 401 是永久类：零重试，立即 AuthError。
    let mut h = batch_retry_harness(
        vec![Script::Fail(api_err(
            401,
            r#"{"error":{"message":"Invalid API key"}}"#,
        ))],
        fast_transient_config(2),
    );

    drive_query(&mut h).await;

    assert_eq!(h.provider.budgets().len(), 1, "永久类不得重发");
    match &h.turn_outcomes()[..] {
        [TurnOutcome::Failed { reason, .. }] => match reason {
            TurnFailureReason::AuthError { message } => {
                assert!(message.contains("Invalid API key"), "实际 {message}");
            }
            other => panic!("401 应为 AuthError，实际 {other:?}"),
        },
        other => panic!("期望单次 Failed，实际 {other:?}"),
    }
}

#[tokio::test]
async fn test_batch_quota_exhausted_fails_without_retry() {
    // 额度耗尽是永久类（重发也不会有额度）：零重试，类型化报错。
    let mut h = batch_retry_harness(
        vec![Script::Fail(api_err(
            429,
            r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota"}}"#,
        ))],
        fast_transient_config(2),
    );

    drive_query(&mut h).await;

    assert_eq!(h.provider.budgets().len(), 1, "额度耗尽不得重发");
    match &h.turn_outcomes()[..] {
        [TurnOutcome::Failed { reason, .. }] => {
            assert!(
                matches!(reason, TurnFailureReason::QuotaExhausted { .. }),
                "应为 QuotaExhausted，实际 {reason:?}"
            );
        }
        other => panic!("期望单次 Failed，实际 {other:?}"),
    }
}

#[tokio::test]
async fn test_stream_transient_error_rolls_back_and_retries() {
    // 流中途断开（established-stream transport error）：
    // 残句回退 + 通知（reason=Transient）+ 退避重发 → 第二次成功。
    let mut h = retry_harness(
        vec![
            Script::ChunksWithErr(vec![
                Ok(StreamChunk::BlockStart {
                    index: 0,
                    block_type: model_provider::BlockType::Text,
                }),
                Ok(StreamChunk::TextDelta {
                    index: 0,
                    delta: "半个答案".to_string(),
                }),
                Err(model_provider::ProviderError::Stream(
                    "connection reset by peer".into(),
                )),
            ]),
            completed("完整答案"),
        ],
        fast_transient_config(2),
    );

    drive_query(&mut h).await;

    assert_eq!(
        h.provider.budgets().len(),
        2,
        "断流后必须原样重发一次（不抬预算）"
    );
    assert_eq!(
        h.provider.budgets()[1],
        h.provider.budgets()[0],
        "瞬时重发不改输出预算"
    );

    let events = h.drain_events();
    // 通知：恰好一条，reason=Transient，载荷是被回退的残句
    let notices: Vec<_> = events
        .iter()
        .filter_map(|ev| match ev {
            LooperEvent::TruncationRetry {
                attempt,
                discarded_text,
                reason,
                ..
            } => Some((*attempt, discarded_text.clone(), *reason)),
            _ => None,
        })
        .collect();
    assert_eq!(notices.len(), 1, "断流重发必须恰好一条通知");
    assert_eq!(notices[0].0, 1);
    assert_eq!(notices[0].1, "半个答案", "残句必须作为载荷供落库剥离");
    assert!(
        notices[0].2 == RetryNoticeReason::Transient,
        "reason 应为 Transient"
    );

    // 通知早于重试尝试的第一条 delta —— 横幅才能插在残句与新正文之间。
    // 注意：被回退那次的 delta 也在事件流里（在通知之前），所以只看
    // 通知之后是否还有 delta，以及通知之后的第一段正文是否为重试内容。
    let notice_at = events
        .iter()
        .position(|ev| matches!(ev, LooperEvent::TruncationRetry { .. }))
        .unwrap();
    let post_notice_text: String = events
        .iter()
        .skip(notice_at + 1)
        .filter_map(|ev| match ev {
            LooperEvent::TextDelta { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        post_notice_text, "完整答案",
        "通知之后必须只包含重试尝试的正文"
    );

    // turn_outcomes 与 drain_events 互斥（同一 listener），从已 drain 的
    // 事件里直接取 TurnComplete。
    let outcomes: Vec<TurnOutcome> = events
        .iter()
        .filter_map(|ev| match ev {
            LooperEvent::TurnComplete { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .collect();
    match &outcomes[..] {
        [TurnOutcome::Success { text }] => {
            assert_eq!(text, "完整答案", "残句不得进入最终结果");
        }
        other => panic!("重试成功应收敛为 Success，实际 {other:?}"),
    }
    assert_eq!(h.committed_turns(), 1, "重试在同一轮内完成");
}

#[tokio::test]
async fn test_content_filter_fails_with_typed_reason_no_retry() {
    // FinishReason::ContentFilter → ContentFiltered，双路径都不重试。
    let mut h = batch_retry_harness(
        vec![batch_response(
            "filtered",
            12,
            model_provider::ResponseStatus::Failed,
            Some(FinishReason::ContentFilter),
        )],
        fast_transient_config(2),
    );

    drive_query(&mut h).await;

    assert_eq!(h.provider.budgets().len(), 1, "内容过滤不得重试");
    match &h.turn_outcomes()[..] {
        [TurnOutcome::Failed { reason, .. }] => {
            assert!(
                matches!(reason, TurnFailureReason::ContentFiltered { .. }),
                "应为 ContentFiltered，实际 {reason:?}"
            );
        }
        other => panic!("期望单次 Failed，实际 {other:?}"),
    }
}

#[tokio::test]
async fn test_stream_content_filter_fails_with_typed_reason_no_retry() {
    let mut h = retry_harness(
        vec![text_chunks("被过滤", 12, FinishReason::ContentFilter)],
        fast_transient_config(2),
    );

    drive_query(&mut h).await;

    assert_eq!(h.provider.budgets().len(), 1, "内容过滤不得重试");
    match &h.turn_outcomes()[..] {
        [TurnOutcome::Failed { reason, .. }] => {
            assert!(
                matches!(reason, TurnFailureReason::ContentFiltered { .. }),
                "应为 ContentFiltered，实际 {reason:?}"
            );
        }
        other => panic!("期望单次 Failed，实际 {other:?}"),
    }
}

#[tokio::test]
async fn test_stream_cut_without_finish_retries_transiently() {
    // 流自然关闭但从未收到 Finish、块也未闭合（上游中途消失）：
    // Incomplete + finish_reason=None → 瞬时重发。
    let mut h = retry_harness(
        vec![
            Script::Chunks(vec![
                StreamChunk::BlockStart {
                    index: 0,
                    block_type: model_provider::BlockType::Text,
                },
                StreamChunk::TextDelta {
                    index: 0,
                    delta: "断在半".to_string(),
                },
                // 无 BlockEnd、无 Finish — 模拟上游掐断
            ]),
            completed("重试成功"),
        ],
        fast_transient_config(2),
    );

    drive_query(&mut h).await;

    assert_eq!(h.provider.budgets().len(), 2, "掐断应触发一次瞬时重发");
    match &h.turn_outcomes()[..] {
        [TurnOutcome::Success { text }] => assert_eq!(text, "重试成功"),
        other => panic!("期望重试后 Success，实际 {other:?}"),
    }
}

#[test]
fn test_failure_label_includes_typed_reasons() {
    // 中断标签进模型上下文 — 必须是英文短句且带原始 msg 摘要。
    let rate = TurnFailureReason::RateLimited {
        attempts: 3,
        message: "Rate limit reached".into(),
    };
    let label = failure_label(&rate);
    assert!(label.starts_with("rate limited after 3 attempts"));
    assert!(label.contains("Rate limit reached"));

    let auth = TurnFailureReason::AuthError {
        message: "Invalid API key".into(),
    };
    assert_eq!(failure_label(&auth), "auth error: Invalid API key");

    // 长 msg 截断 — 标签不能把整个错误体塞进上下文
    let long = TurnFailureReason::ContextOverflow {
        message: "x".repeat(1000),
    };
    assert!(
        failure_label(&long).chars().count() < 300,
        "标签必须截断长 msg"
    );
}

#[test]
fn test_model_failure_reason_classification() {
    use model_provider::ApiErrorKind;

    // (错误, 期望匹配谓词) — 谓词用 fn 指针避免闭包类型推断噪音
    type Case = (
        model_provider::ProviderError,
        fn(&TurnFailureReason) -> bool,
    );

    fn is_rate(r: &TurnFailureReason) -> bool {
        matches!(r, TurnFailureReason::RateLimited { .. })
    }
    fn is_unavailable(r: &TurnFailureReason) -> bool {
        matches!(r, TurnFailureReason::ModelUnavailable { .. })
    }
    fn is_auth(r: &TurnFailureReason) -> bool {
        matches!(r, TurnFailureReason::AuthError { .. })
    }
    fn is_quota(r: &TurnFailureReason) -> bool {
        matches!(r, TurnFailureReason::QuotaExhausted { .. })
    }
    fn is_overflow(r: &TurnFailureReason) -> bool {
        matches!(r, TurnFailureReason::ContextOverflow { .. })
    }

    let cases: Vec<Case> = vec![
        (api_err(429, "rate limit"), is_rate),
        (
            model_provider::ProviderError::Stream("reset".into()),
            is_unavailable,
        ),
        (api_err(500, "oops"), is_unavailable),
        (api_err(401, "bad key"), is_auth),
        (api_err(400, "insufficient quota"), is_quota),
        (api_err(400, "context_length_exceeded"), is_overflow),
    ];
    for (err, pred) in cases {
        let reason = model_failure_reason(err.classify(), 1);
        assert!(
            pred(&reason),
            "{:?} 映射错误 → {reason:?}",
            err.classify().kind
        );
    }

    // is_transient 与映射一致：只有瞬时类进重发
    assert!(ApiErrorKind::RateLimited.is_transient());
    assert!(!ApiErrorKind::QuotaExhausted.is_transient());
}

// ── 统一重试：新增用例 ──────────────────────────────────────────────

#[tokio::test]
async fn transient_retry_does_not_raise_budget() {
    // 瞬时重发原样重发：不置 `budget_raised`，下一次请求的输出预算逐位不变。
    // 抬预算可能超出模型真实上限（网关 400），且违背「原样重发」语义。
    let mut h = batch_retry_harness(
        vec![
            Script::Fail(api_err(429, r#"{"error":{"message":"rate limited"}}"#)),
            batch_response(
                "done",
                20,
                model_provider::ResponseStatus::Completed,
                Some(FinishReason::Stop),
            ),
        ],
        fast_transient_config(3),
    );

    drive_query(&mut h).await;

    assert_eq!(h.provider.budgets().len(), 2, "瞬时重发一次");
    assert_eq!(
        h.provider.budgets()[1],
        h.provider.budgets()[0],
        "重发请求的输出预算必须与首次逐位相同"
    );
    assert!(
        !h.looper.budget_raised,
        "瞬时重发不得置位抬预算标志（即便它递增同一计数）"
    );
    assert_eq!(h.looper.max_output_tokens_override(), None);
    assert!(matches!(
        h.turn_outcomes().as_slice(),
        [TurnOutcome::Success { .. }]
    ));
}

#[tokio::test]
async fn mixed_causes_share_one_limit() {
    // 单计数单上限：截断 1 次 + 瞬时 2 次合计用尽 retry_limit=3，
    // 第 4 次重发被拒，收敛为类型化失败（不再是「还有一次瞬时额度」）。
    let body = r#"{"error":{"message":"Rate limit reached","code":"rate_limit_exceeded"}}"#;
    let mut h = retry_harness(
        vec![
            truncated("part"),
            Script::Fail(api_err(429, body)),
            Script::Fail(api_err(429, body)),
            Script::Fail(api_err(429, body)),
        ],
        LooperConfig {
            retry_limit: 3,
            retry_output_budget: 32_768,
            retry_base_delay_ms: 1,
            retry_max_delay_ms: 2,
            ..Default::default()
        },
    );

    drive_query(&mut h).await;

    assert_eq!(
        h.provider.budgets().len(),
        4,
        "首次 + 3 次重发（截断 1 + 瞬时 2）= 4 次请求"
    );
    assert_eq!(h.looper.retries_used, 3, "单计数恰好耗尽 retry_limit");
    match &h.turn_outcomes()[..] {
        [TurnOutcome::Failed { reason, .. }] => match reason {
            TurnFailureReason::RateLimited { attempts, .. } => {
                assert_eq!(*attempts, 4, "attempts = 实际发起的总请求数");
            }
            other => panic!("额度耗尽应收敛为 RateLimited，实际 {other:?}"),
        },
        other => panic!("期望单次 Failed，实际 {other:?}"),
    }
}

#[tokio::test]
async fn retry_then_failure_freezes_turn_with_stub() {
    // 截断重试已回退 staging、新尝试零产出即失败 —— 若不补存活桩，
    // interrupt_turn 见空 staging 退化 rollback，用户提问连同整轮从历史消失。
    let mut h = retry_harness(
        vec![
            truncated("salvaged"),
            Script::Fail(api_err(
                400,
                r#"{"error":{"message":"max_output_tokens too large"}}"#,
            )),
        ],
        LooperConfig {
            retry_limit: 1,
            retry_output_budget: 32_768,
            retry_base_delay_ms: 1,
            retry_max_delay_ms: 2,
            ..Default::default()
        },
    );

    drive_query(&mut h).await;

    match &h.turn_outcomes()[..] {
        [TurnOutcome::Failed { partial_text, .. }] => {
            assert_eq!(partial_text, "", "被回退的截断文本不得归还给失败卡片");
        }
        other => panic!("期望单次 Failed，实际 {other:?}"),
    }
    assert!(h.looper.session().is_idle(), "收尾后必须回到 Idle");
    assert_eq!(
        h.committed_turns(),
        1,
        "整轮必须冻结进历史，不得退化成 rollback"
    );
    let committed: Vec<_> = h
        .looper
        .session()
        .committed_turns()
        .iter()
        .flatten()
        .collect();
    assert!(
        committed.iter().any(|am| matches!(
            am.message.as_ref(),
            InputItem::Message { content, .. }
                if content.text_view() == "[response discarded after retry]"
        )),
        "零产出的重发失败必须留下存活桩"
    );
    assert!(
        !committed.iter().any(|am| matches!(
            am.message.as_ref(),
            InputItem::Message { content, .. } if content.text_view() == "salvaged"
        )),
        "被丢弃的截断文本不得进历史"
    );
}

#[tokio::test]
async fn plain_failure_without_retry_keeps_old_degradation() {
    // 桩按 `retry_happened` 门控 —— 首请求 401、零重试时不冻结、不补桩，
    // 轮次照旧退化消失。
    let mut h = batch_retry_harness(
        vec![Script::Fail(api_err(
            401,
            r#"{"error":{"message":"Invalid API key"}}"#,
        ))],
        fast_transient_config(3),
    );

    drive_query(&mut h).await;

    assert_eq!(h.provider.budgets().len(), 1, "永久类零重发");
    assert_eq!(h.looper.retries_used, 0, "未发生任何重发");
    match &h.turn_outcomes()[..] {
        [TurnOutcome::Failed { partial_text, .. }] => {
            assert_eq!(partial_text, "", "零产出失败的 partial_text 依旧为空");
        }
        other => panic!("期望单次 Failed，实际 {other:?}"),
    }
    assert!(
        h.committed_turns() == 0 && h.looper.session().turn_index() == 0,
        "无重试的普通失败照旧退化（不冻结、不补桩），实际 committed={}, turn_index={}",
        h.committed_turns(),
        h.looper.session().turn_index()
    );
}

#[tokio::test]
async fn stub_not_emitted_when_partial_exists() {
    // 重试后的新尝试自己吐过文本 → 走既有 partial_text 补条，
    // 存活桩不得叠加（历史里恰好一份新产出，且无桩文案）。
    let mut h = retry_harness(
        vec![
            truncated("first"),
            Script::ChunksWithErr(vec![
                Ok(StreamChunk::BlockStart {
                    index: 0,
                    block_type: model_provider::BlockType::Text,
                }),
                Ok(StreamChunk::TextDelta {
                    index: 0,
                    delta: "second".to_string(),
                }),
                Err(api_err(400, "bad request")),
            ]),
        ],
        LooperConfig {
            retry_limit: 1,
            retry_output_budget: 32_768,
            retry_base_delay_ms: 1,
            retry_max_delay_ms: 2,
            ..Default::default()
        },
    );

    drive_query(&mut h).await;

    match &h.turn_outcomes()[..] {
        [TurnOutcome::Failed { partial_text, .. }] => {
            assert_eq!(partial_text, "second", "partial_text 必须是新尝试的产出");
        }
        other => panic!("期望单次 Failed，实际 {other:?}"),
    }
    assert_eq!(h.committed_turns(), 1, "整轮照常冻结");
    let committed: Vec<_> = h
        .looper
        .session()
        .committed_turns()
        .iter()
        .flatten()
        .collect();
    let second_hits = committed
        .iter()
        .filter(|am| {
            matches!(
                am.message.as_ref(),
                InputItem::Message { content, .. } if content.text_view() == "second"
            )
        })
        .count();
    assert_eq!(second_hits, 1, "新产出只补一条，不得出双份");
    assert!(
        !committed.iter().any(|am| matches!(
            am.message.as_ref(),
            InputItem::Message { content, .. }
                if content.text_view() == "[response discarded after retry]"
        )),
        "partial_text 非空时不得再补存活桩"
    );
}

#[tokio::test]
async fn retry_output_budget_headroom_guard() {
    // `retry_output_budget` 既是抬升目标也是上限。
    // 配置 8000 < 8192 → 抬到 8192 重发；配置 8192 ≥ 8192 → headroom
    // 判据拒绝（重发与上次逐字节相同，白烧一次调用）。
    let cfg = |budget: u32| LooperConfig {
        retry_limit: 1,
        retry_output_budget: budget,
        retry_base_delay_ms: 1,
        retry_max_delay_ms: 2,
        ..Default::default()
    };

    let mut raised = retry_harness_with_budget(
        vec![truncated("part"), completed("done")],
        cfg(8_192),
        Some(8_000),
    );
    drive_query(&mut raised).await;
    assert_eq!(
        raised.provider.budgets(),
        vec![Some(8_000), Some(8_192)],
        "配置低于抬升目标时必须抬到目标值重发"
    );
    assert!(matches!(
        raised.turn_outcomes().as_slice(),
        [TurnOutcome::Success { .. }]
    ));

    let mut no_headroom =
        retry_harness_with_budget(vec![truncated("part")], cfg(8_192), Some(8_192));
    drive_query(&mut no_headroom).await;
    assert_eq!(
        no_headroom.provider.budgets(),
        vec![Some(8_192)],
        "配置已达抬升目标则不得重发"
    );
    assert!(matches!(
        no_headroom.turn_outcomes().as_slice(),
        [TurnOutcome::Failed { .. }]
    ));
}

#[test]
fn from_env_reads_all_retry_fields() {
    // env 是进程级的：本用例内设后清，避免污染其他并行测试。
    // edition 2024 下 set_var / remove_var 为 unsafe。
    const NAMES: [&str; 4] = [
        "PECO_RETRY_LIMIT",
        "PECO_RETRY_OUTPUT_BUDGET",
        "PECO_RETRY_BASE_DELAY_MS",
        "PECO_RETRY_MAX_DELAY_MS",
    ];
    unsafe {
        for name in NAMES {
            std::env::remove_var(name);
        }
    }

    let defaults = LooperConfig::from_env();
    assert_eq!(defaults.retry_limit, 3);
    assert_eq!(defaults.retry_output_budget, 32_768);
    assert_eq!(defaults.retry_base_delay_ms, 500);
    assert_eq!(defaults.retry_max_delay_ms, 5_000);

    unsafe {
        std::env::set_var("PECO_RETRY_LIMIT", "7");
        std::env::set_var("PECO_RETRY_OUTPUT_BUDGET", "8192");
        std::env::set_var("PECO_RETRY_BASE_DELAY_MS", "25");
        std::env::set_var("PECO_RETRY_MAX_DELAY_MS", "1500");
    }
    let parsed = LooperConfig::from_env();
    assert_eq!(parsed.retry_limit, 7);
    assert_eq!(parsed.retry_output_budget, 8_192);
    assert_eq!(parsed.retry_base_delay_ms, 25);
    assert_eq!(parsed.retry_max_delay_ms, 1_500);

    unsafe {
        for name in NAMES {
            std::env::remove_var(name);
        }
    }
}

// ── Done 分支的边界编排（apply_boundary → 落盘 → 钩子）──────────────

/// 边界钩子探针 —— Done 分支编排的唯一外部观察点。
#[derive(Default)]
struct BoundaryProbe {
    boundary_calls: AtomicUsize,
    boundary_folded: AtomicUsize,
    boundary_evaluated: AtomicUsize,
    compacted_calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LooperHook for BoundaryProbe {
    async fn on_boundary_reduction(&self, _session: &Session, reduction: &BoundaryReduction) {
        self.boundary_calls.fetch_add(1, Ordering::SeqCst);
        if reduction.folded() {
            self.boundary_folded.fetch_add(1, Ordering::SeqCst);
        }
        if reduction.strip_evaluation.is_some() {
            self.boundary_evaluated.fetch_add(1, Ordering::SeqCst);
        }
    }

    async fn on_context_compacted(&self, _outcome: &CompactionOutcome) {
        self.compacted_calls.fetch_add(1, Ordering::SeqCst);
    }
}

/// 固定短摘要的假摘要器 —— 折叠路径的最小驱动。
struct FixedSummarizer;

#[async_trait::async_trait]
impl super::super::compaction::TurnSummarizer for FixedSummarizer {
    async fn summarize(
        &self,
        _previous: Option<&str>,
        _evicted: &str,
    ) -> Result<String, AgentError> {
        Ok(format!(
            "{}早期历史{}",
            super::super::compaction::SUMMARY_OPEN,
            super::super::compaction::SUMMARY_CLOSE
        ))
    }

    async fn summarize_inflight(&self, _transcript: &str) -> Result<String, AgentError> {
        unimplemented!("boundary tests never compose turn epilogues")
    }
}

fn boundary_probe_config(
    policy: Arc<super::super::compaction::CompactionPolicy>,
) -> (LooperConfig, Arc<BoundaryProbe>) {
    let probe = Arc::new(BoundaryProbe::default());
    let config = LooperConfig {
        compaction: Some(policy),
        hooks: vec![Arc::clone(&probe) as Arc<dyn LooperHook>],
        ..Default::default()
    };
    (config, probe)
}

/// TurnComplete 已消费后，数到外层回 Idle 之间的折叠事件数 ——
/// TurnComplete 在压缩编排**之前**发出，不等 Idle 就断言会与在途钩子赛跑。
async fn drain_compacted_until_idle(h: &LooperHandle) -> usize {
    let res = tokio::time::timeout(Duration::from_secs(5), async {
        let mut compacted = 0;
        loop {
            match h.recv_event().await {
                Some(LooperEvent::ContextCompacted { .. }) => compacted += 1,
                Some(LooperEvent::StateChange {
                    to: LooperState::Idle,
                    ..
                }) => break compacted,
                Some(_) => continue,
                None => panic!("event channel closed before Idle"),
            }
        }
    })
    .await;
    res.expect("timed out waiting for Idle after turn")
}

/// 不折叠的边界：钩子每边界恰一次，评估值不触发二次落盘，不发折叠事件。
#[tokio::test]
async fn test_boundary_hook_fires_without_fold_and_no_extra_persist() {
    let policy = Arc::new(
        super::super::compaction::CompactionPolicy::new(
            usize::MAX,
            1000,
            Arc::new(FixedSummarizer),
        )
        .with_strip(super::super::compaction::StripPolicy {
            keep_recent_turns: 1,
            strip_reasoning: true,
            strip_tool_args: true,
            tool_output_max_chars: 0,
            enforce: false,
        }),
    );
    let (config, probe) = boundary_probe_config(policy);
    let persister = Arc::new(CountingPersister::default());
    let (h, _provider) = spawn_scripted_looper_with_persister(
        vec![completed("ok")],
        config,
        Arc::clone(&persister) as Arc<dyn crate::persistence::SessionPersister>,
    );

    h.send_query("hi".into()).await.expect("looper alive");
    match recv_until_turn_complete(&h).await {
        TurnOutcome::Success { text } => assert_eq!(text, "ok"),
        other => panic!("expected success, got {other:?}"),
    }
    let compacted_events = drain_compacted_until_idle(&h).await;

    assert_eq!(probe.boundary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        probe.boundary_evaluated.load(Ordering::SeqCst),
        1,
        "装配削减策略的边界必须产出评估值"
    );
    assert_eq!(probe.boundary_folded.load(Ordering::SeqCst), 0);
    assert_eq!(probe.compacted_calls.load(Ordering::SeqCst), 0);
    assert_eq!(compacted_events, 0, "未折叠不得发 ContextCompacted");
    assert_eq!(
        persister.saves.load(Ordering::SeqCst),
        1,
        "评估值不改会话，不得触发二次落盘"
    );

    h.shutdown().await.expect("clean shutdown");
}

/// 折叠的边界：折叠事件 + 折叠后二次落盘 + 两个压缩钩子都触发。
#[tokio::test]
async fn test_boundary_fold_emits_event_and_repersists() {
    // trigger=0：任意非空上下文都超阈值；单轮被「至少保留 1 轮」保底拦下，
    // 第二轮起才真正折叠 —— 顺带覆盖「保底不折叠」的分支。
    let policy = Arc::new(
        super::super::compaction::CompactionPolicy::new(0, 0, Arc::new(FixedSummarizer))
            .with_strip(super::super::compaction::StripPolicy {
                keep_recent_turns: 1,
                strip_reasoning: true,
                strip_tool_args: true,
                tool_output_max_chars: 0,
                enforce: false,
            }),
    );
    let (config, probe) = boundary_probe_config(policy);
    let persister = Arc::new(CountingPersister::default());
    let (h, _provider) = spawn_scripted_looper_with_persister(
        vec![completed("ok1"), completed("ok2")],
        config,
        Arc::clone(&persister) as Arc<dyn crate::persistence::SessionPersister>,
    );

    // 第 1 轮：唯一一轮被保底留住 ⇒ 不折叠，仅 commit 落盘
    h.send_query("q1".into()).await.expect("looper alive");
    recv_until_turn_complete(&h).await;
    drain_compacted_until_idle(&h).await;
    assert_eq!(probe.boundary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(probe.boundary_folded.load(Ordering::SeqCst), 0);
    assert_eq!(persister.saves.load(Ordering::SeqCst), 1);

    // 第 2 轮：2 轮超阈值 ⇒ 驱逐 1 轮 + pinned，commit 与折叠各落一次盘
    h.send_query("q2".into()).await.expect("looper alive");
    recv_until_turn_complete(&h).await;
    let compacted_events = drain_compacted_until_idle(&h).await;

    assert_eq!(compacted_events, 1, "折叠必须发 ContextCompacted");
    assert_eq!(probe.boundary_calls.load(Ordering::SeqCst), 2);
    assert_eq!(probe.boundary_folded.load(Ordering::SeqCst), 1);
    assert_eq!(probe.compacted_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        persister.saves.load(Ordering::SeqCst),
        3,
        "每轮 commit 落盘一次，折叠再生效一次"
    );

    h.shutdown().await.expect("clean shutdown");
}

/// BL-4：strip 生效但本轮不折叠 → `mutated()` → commit 之外二次落盘。
#[tokio::test]
async fn test_boundary_strip_enforce_repersists_without_fold() {
    let policy = Arc::new(
        super::super::compaction::CompactionPolicy::new(
            usize::MAX,
            1000,
            Arc::new(FixedSummarizer),
        )
        .with_strip(super::super::compaction::StripPolicy {
            keep_recent_turns: 1,
            strip_reasoning: true,
            strip_tool_args: true,
            tool_output_max_chars: 0,
            enforce: true,
        }),
    );
    let (config, probe) = boundary_probe_config(policy);
    let persister = Arc::new(CountingPersister::default());
    let (h, _provider) = spawn_scripted_looper_with_persister(
        vec![reasoning_then_text("想想", "ok1"), completed("ok2")],
        config,
        Arc::clone(&persister) as Arc<dyn crate::persistence::SessionPersister>,
    );

    // 第 1 轮：scope = 1 − 1 = 0 ⇒ 无可削，无折叠 ⇒ 仅 commit 落盘
    h.send_query("q1".into()).await.expect("looper alive");
    match recv_until_turn_complete(&h).await {
        TurnOutcome::Success { text } => assert_eq!(text, "ok1"),
        other => panic!("expected success, got {other:?}"),
    }
    drain_compacted_until_idle(&h).await;
    assert_eq!(probe.boundary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        persister.saves.load(Ordering::SeqCst),
        1,
        "零改写边界不得二次落盘"
    );

    // 第 2 轮：scope 覆盖 turn0（含 reasoning）⇒ 削减生效但不折叠 ⇒ 二次落盘
    h.send_query("q2".into()).await.expect("looper alive");
    match recv_until_turn_complete(&h).await {
        TurnOutcome::Success { text } => assert_eq!(text, "ok2"),
        other => panic!("expected success, got {other:?}"),
    }
    let compacted_events = drain_compacted_until_idle(&h).await;

    assert_eq!(compacted_events, 0, "trigger=usize::MAX 不得折叠");
    assert_eq!(probe.boundary_calls.load(Ordering::SeqCst), 2);
    assert_eq!(probe.boundary_folded.load(Ordering::SeqCst), 0);
    assert_eq!(
        probe.boundary_evaluated.load(Ordering::SeqCst),
        2,
        "enforce 同样产出评估值（同一份扫描）"
    );
    assert_eq!(
        persister.saves.load(Ordering::SeqCst),
        3,
        "每轮 commit 一次 + 削减生效再生效一次"
    );

    h.shutdown().await.expect("clean shutdown");
}

/// BL-4：`maybe_compact` 返 Err → looper 不 panic、不额外落盘、
/// 仅走 error! 分支，会话照常接下一轮。
#[tokio::test]
async fn test_boundary_compaction_error_is_nonfatal_no_persist() {
    let policy = Arc::new(super::super::compaction::CompactionPolicy::new(
        0,
        0,
        Arc::new(FailingBoundarySummarizer),
    ));
    let (config, probe) = boundary_probe_config(policy);
    let persister = Arc::new(CountingPersister::default());
    let (h, _provider) = spawn_scripted_looper_with_persister(
        vec![completed("ok1"), completed("ok2"), completed("ok3")],
        config,
        Arc::clone(&persister) as Arc<dyn crate::persistence::SessionPersister>,
    );

    // 第 1 轮：evict=0（保底）⇒ Ok(None) ⇒ 钩子照发、无额外落盘
    h.send_query("q1".into()).await.expect("looper alive");
    match recv_until_turn_complete(&h).await {
        TurnOutcome::Success { text } => assert_eq!(text, "ok1"),
        other => panic!("expected success, got {other:?}"),
    }
    drain_compacted_until_idle(&h).await;
    assert_eq!(probe.boundary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(persister.saves.load(Ordering::SeqCst), 1);

    // 第 2 轮：evict=1 ⇒ 摘要失败（failures=1 < 3）⇒ apply_boundary Err
    // ⇒ 不 panic、不落盘、不发边界钩子
    h.send_query("q2".into()).await.expect("looper alive");
    match recv_until_turn_complete(&h).await {
        TurnOutcome::Success { text } => assert_eq!(text, "ok2"),
        other => panic!("expected success, got {other:?}"),
    }
    let compacted_events = drain_compacted_until_idle(&h).await;
    assert_eq!(compacted_events, 0, "折叠失败不得发折叠事件");
    assert_eq!(
        probe.boundary_calls.load(Ordering::SeqCst),
        1,
        "Err 分支不发边界钩子"
    );
    assert_eq!(
        persister.saves.load(Ordering::SeqCst),
        2,
        "折叠失败不得二次落盘（每轮仅 commit 一次）"
    );

    // 第 3 轮：会话照常继续（failures=2 仍 < 3，依然 Err，依然非致命）
    h.send_query("q3".into()).await.expect("looper alive");
    match recv_until_turn_complete(&h).await {
        TurnOutcome::Success { text } => assert_eq!(text, "ok3"),
        other => panic!("session must continue after boundary error, got {other:?}"),
    }
    drain_compacted_until_idle(&h).await;
    assert_eq!(persister.saves.load(Ordering::SeqCst), 3);
    assert_eq!(probe.boundary_calls.load(Ordering::SeqCst), 1);

    h.shutdown().await.expect("clean shutdown");
}

/// reasoning 块 + 文本块的脚本 —— 给 committed 轮灌入可削减的 Reasoning 条目。
fn reasoning_then_text(reason: &str, text: &str) -> Script {
    Script::Chunks(vec![
        StreamChunk::BlockStart {
            index: 0,
            block_type: model_provider::BlockType::Reasoning,
        },
        StreamChunk::ReasoningDelta {
            index: 0,
            delta: reason.to_string(),
        },
        StreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::Reasoning {
                text: reason.to_string(),
            },
        },
        StreamChunk::BlockStart {
            index: 1,
            block_type: model_provider::BlockType::Text,
        },
        StreamChunk::TextDelta {
            index: 1,
            delta: text.to_string(),
        },
        StreamChunk::BlockEnd {
            index: 1,
            block: ContentBlock::Text {
                text: text.to_string(),
            },
        },
        StreamChunk::Usage {
            usage: Usage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
            },
        },
        StreamChunk::Finish {
            reason: FinishReason::Stop,
        },
    ])
}

/// 恒失败摘要器 —— 驱动 `maybe_compact` 返 Err（连续失败 < 3 次的窗口）。
struct FailingBoundarySummarizer;

#[async_trait::async_trait]
impl super::super::compaction::TurnSummarizer for FailingBoundarySummarizer {
    async fn summarize(
        &self,
        _previous: Option<&str>,
        _evicted: &str,
    ) -> Result<String, AgentError> {
        Err(AgentError::Compaction(
            "summary generation failed".to_string(),
        ))
    }

    async fn summarize_inflight(&self, _transcript: &str) -> Result<String, AgentError> {
        unimplemented!("boundary tests never compose turn epilogues")
    }
}

// ── 产物声明丢失告警（emit_event 的失败分支）──────────────────────────

const ARTIFACTS_LOST: &str = "Artifact declarations lost";

fn decl(path: &str) -> crate::artifacts::ArtifactDecl {
    crate::artifacts::ArtifactDecl {
        path: path.to_string(),
        title: None,
        mime: None,
    }
}

fn tool_result_with(name: &str, artifacts: Vec<crate::artifacts::ArtifactDecl>) -> LooperEvent {
    LooperEvent::ToolResult {
        id: "call-1".to_string(),
        name: name.to_string(),
        result: "ok".to_string(),
        images: Vec::new(),
        artifacts,
    }
}

/// 采集日志的 writer：把字节流追加到共享缓冲，供告警断言。
///
/// 日志断言必须用 `#[tokio::test]` 默认的 `current_thread` 运行时：默认
/// subscriber 是线程局部的，后台 task 的日志会落到别的线程上，断言恒红 ——
/// 所以本组用例直接在测试里构造 looper 并同步调 `emit_event`。
#[derive(Clone, Default)]
struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

impl Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogs;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl CapturedLogs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

fn capture_logs() -> (CapturedLogs, tracing::subscriber::DefaultGuard) {
    let writer = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(writer.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .without_time()
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    (writer, guard)
}

/// 只用来发事件的 looper：`PanicProvider` 保证本组用例永不触达模型。
///
/// `session_id` 由调用方指定：全组共用一个字面量时，日志里的会话归属
/// 与写死的常量不可区分，字段读错也照样全绿。
fn event_looper(buffer: usize, session_id: &str) -> (AgentLooper, Listener<LooperEvent>) {
    let profile: crate::agent::AgentProfile = serde_yaml::from_str(
        "agent:\n  name: t\n  description: d\nllm:\n  provider: p\n  model: m\n",
    )
    .unwrap();
    let executor: Arc<dyn crate::tools::ToolExecutor> =
        Arc::new(crate::tools::DefaultToolsExecutor::new(Vec::new()));
    let agent = Arc::new(Agent::from_parts(
        std::path::PathBuf::from("/tmp/agent.md"),
        profile,
        "sys".to_string(),
        Arc::new(PanicProvider),
        crate::agent::ModelConfigBuilder::new().build(),
        Arc::clone(&executor),
        Arc::new(crate::mcp::McpManager::empty(executor)),
        None,
    ));
    let (looper_side, caller_side) = make_async_intercom_pair::<LooperEvent, UserMsg>(buffer);
    let (event_speaker, _user_listener) = looper_side.split();
    let (_user_speaker, event_listener) = caller_side.split();
    let looper = AgentLooper::new(
        agent,
        Box::new(Session::new(session_id.to_string(), "d".to_string())),
        event_speaker,
        LooperConfig::default(),
        Arc::new(crate::persistence::NullSessionPersister),
    );
    (looper, event_listener)
}

/// listener 全部 drop（用户关掉 SSE 页面、run 仍在后台跑）时，带声明的
/// `ToolResult` 不能无声蒸发。
///
/// 变异敏感度：本条完全依赖告警可见 —— 实现若吞掉失败或降级到 debug，
/// 日志里就没有可断言的告警；会话与工具名取本用例专属值，把任一字段
/// 写死成常量都会让对应断言变红；把条数写死成常量 ⇒ `count=2` 断言
/// 变红；同一次丢失发两条告警 ⇒ 条数断言变红。
#[tokio::test]
async fn test_tool_result_artifacts_loss_warns_when_listener_closed() {
    let (logs, _guard) = capture_logs();
    let (looper, event_listener) = event_looper(4, "s-closed");
    drop(event_listener);

    looper.emit_event(tool_result_with(
        "t-close",
        vec![decl("/tmp/a.png"), decl("/tmp/b.png")],
    ));

    let text = logs.text();
    assert!(
        text.contains(ARTIFACTS_LOST),
        "接收端关闭时丢声明必须告警；实际日志 {text:?}"
    );
    // `capture_logs` 对任何级别都放行，级别只能在这里锁住：` WARN ` 与
    // `DEBUG` 都是 fmt 默认的级别渲染字面量，取值不依赖告警文案。
    assert!(
        text.contains(" WARN "),
        "告警须以 warn 级别发出；实际日志 {text:?}"
    );
    assert!(
        !text.contains("DEBUG"),
        "告警不得降级到 debug 以下；实际日志 {text:?}"
    );
    // 同进程多会话下没有会话归属就无法定位是哪次 run 丢了产物。
    assert!(
        text.contains("session_id=s-closed"),
        "告警须带会话归属；实际日志 {text:?}"
    );
    assert!(
        text.contains("tool=t-close"),
        "告警须带 tool 名；实际日志 {text:?}"
    );
    assert!(text.contains("count=2"), "告警须带条数；实际日志 {text:?}");
    assert!(
        text.contains("reason=closed"),
        "告警须区分关闭与满载；实际日志 {text:?}"
    );
    assert_eq!(
        text.matches(ARTIFACTS_LOST).count(),
        1,
        "同一次丢失只许告警一条：多条会让日志噪声翻倍；实际日志 {text:?}"
    );
}

/// listener 活着但缓冲已满（消费过慢）同样丢声明，原因须与 closed 可区分。
///
/// `buffer=1`：先塞一条填充事件占满唯一缓冲位，再发带声明的 `ToolResult`。
///
/// 变异敏感度：吞掉 `Err` ⇒ 无日志，本条变红；把失败原因写死成
/// `closed` ⇒ `reason=full` 断言变红；会话与工具名取本用例专属值，
/// 把任一字段写死成常量都会让对应断言变红；成功投递也留痕（`Ok(())`
/// 分支加 `debug!`）⇒ 「成功投递不得留痕」断言变红。
#[tokio::test]
async fn test_tool_result_artifacts_loss_warns_when_buffer_full() {
    let (logs, _guard) = capture_logs();
    let (looper, mut event_listener) = event_looper(1, "s-full");

    let baseline = logs.text().len();
    looper.emit_event(LooperEvent::TextDelta {
        delta: "fill".to_string(),
    });
    // 填充事件走的是成功路径，跑完不得比基线多留一个字节。
    assert_eq!(logs.text().len(), baseline, "成功投递不得留痕");

    looper.emit_event(tool_result_with("t-full", vec![decl("/tmp/a.png")]));

    // 先确认这次确实是「满」而不是「关」：缓冲里只剩填充事件，带声明的被拒收。
    assert!(
        event_listener.try_recv().is_ok(),
        "填充事件须占住唯一缓冲位"
    );
    assert!(
        matches!(
            event_listener.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "带声明的 ToolResult 应当被拒收"
    );

    let text = logs.text();
    assert!(
        text.contains(ARTIFACTS_LOST),
        "缓冲满载时丢声明必须告警；实际日志 {text:?}"
    );
    assert!(
        text.contains("session_id=s-full"),
        "告警须带会话归属；实际日志 {text:?}"
    );
    assert!(
        text.contains("tool=t-full"),
        "告警须带 tool 名；实际日志 {text:?}"
    );
    assert!(text.contains("count=1"), "告警须带条数；实际日志 {text:?}");
    assert!(
        text.contains("reason=full"),
        "满载须报 full；实际日志 {text:?}"
    );
    assert!(
        !text.contains("reason=closed"),
        "满载不得被报成 closed；实际日志 {text:?}"
    );
}

/// 无产物的 `ToolResult` 是每轮正常对话的常态，丢弃不得告警刷屏。
///
/// 变异敏感度：去掉 `artifacts.is_empty()` 守卫 ⇒ 前半段的静默断言变红。
/// 静默段比的是日志字节数而非告警文案，任何级别的留痕（含 `debug!`）都会
/// 变红。后半段拿同类带声明事件做阳性对照，证明采集管道在本用例里确实生效
/// （否则前半段可能因捕获为空而空过）。
#[tokio::test]
async fn test_tool_result_without_artifacts_stays_silent() {
    let (logs, _guard) = capture_logs();
    let (looper, event_listener) = event_looper(4, "s-silent");
    drop(event_listener);

    let baseline = logs.text().len();
    looper.emit_event(tool_result_with("t-silent", Vec::new()));
    assert_eq!(
        logs.text().len(),
        baseline,
        "无产物丢失时不得留任何日志；实际日志 {:?}",
        logs.text()
    );

    looper.emit_event(tool_result_with("t-silent", vec![decl("/tmp/a.png")]));
    assert!(
        logs.text().contains(ARTIFACTS_LOST),
        "阳性对照未捕获到告警，说明采集管道未生效；实际日志 {:?}",
        logs.text()
    );
}

/// 丢弃非 `ToolResult` 事件不得留痕：`TextDelta` 每 token 一条，用户关掉
/// SSE 后 run 仍在后台跑，逐条告警会把日志淹掉。
///
/// 变异敏感度：凡在丢弃路径留痕的实现都会让前半段变红 —— 静默段比的是
/// 日志字节数，既不认文案也不认级别，`warn!` 与 `debug!` 一视同仁；默认
/// 部署的过滤级别是 `peco=debug`，debug 留痕同样会进用户日志。
/// 后半段拿带声明的 `ToolResult` 做阳性对照，证明采集管道在本用例里确实
/// 生效（否则前半段可能因捕获为空而空过）。
#[tokio::test]
async fn test_non_tool_result_loss_stays_silent() {
    let (logs, _guard) = capture_logs();
    let (looper, event_listener) = event_looper(4, "s-silent");
    drop(event_listener);

    let baseline = logs.text().len();
    looper.emit_event(LooperEvent::TextDelta {
        delta: "tok".to_string(),
    });
    looper.emit_event(LooperEvent::ToolCallDelta {
        id: "call-1".to_string(),
        name: Some("t-silent".to_string()),
        arguments: "{".to_string(),
    });
    assert_eq!(
        logs.text().len(),
        baseline,
        "非 ToolResult 事件丢弃时不得留任何日志（debug 亦不可）；实际日志 {:?}",
        logs.text()
    );

    looper.emit_event(tool_result_with("t-silent", vec![decl("/tmp/a.png")]));
    assert!(
        logs.text().contains(ARTIFACTS_LOST),
        "阳性对照未捕获到告警，说明采集管道未生效；实际日志 {:?}",
        logs.text()
    );
}
