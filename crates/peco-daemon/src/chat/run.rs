//! 一次会话运行的执行体：HTTP 触发的轮与无客户端的唤醒投递共用同一条 looper 链。
//!
//! 与 [`handler`](super::handler) 的分工：handler 只做鉴权、取参、会话归属与响应
//! 封装；把 user 文本交给 looper、把 assistant 文本与产物落库、把 looper 事件转成
//! 帧都在这里。这些若各入口各写一份，前端看到的气泡就会与 `messages` 表分叉。
//!
//! **这一轮的生命周期不挂在 SSE 连接上**：事件出口恒为 [`TurnFeed`]，有没有订阅者
//! 是 feed 内部的事（见 [`RunSink`]）。客户端断连因此不再是事件 —— 少了这条分支，
//! 「关页面 = 杀掉这一轮、已产出的正文一个字都不落库」才不再成立。
//!
//! 注册（这个会话谁在跑）在装配校验之后、looper spawn 之前完成，由 [`spawn_run`]
//! 自己做：注册早于「空间是否打开 / agent 是否还在」的校验，一个注定 409 的请求
//! 就会把该会话在途的 run 挤掉。有在途轮即 `Busy`（409）—— 顶替语义随 detach 一起
//! 删除：页面重载只需 attach，不需要挤掉别人正在看的那一轮。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use model_provider::{Content, Usage, text_of_blocks};
use peco_core::agent::{AgentLooper, LooperEvent, LooperState, ResponseOrigin};
use peco_core::session::Session;
use peco_core::session::SessionPersister;
use sqlx::SqlitePool;
use tokio::time::Instant;
use uuid::Uuid;

use crate::LOCAL_USER_ID;
use crate::db::conversations::ConversationRow;
use crate::db::{agents, conversations, messages};
use crate::error::DaemonError;
use crate::provider::{self, ProviderFailure};
use crate::session_store::{SqliteSessionPersister, hydrate_inflight_turn};
use crate::space::SpaceEnv;
use crate::space::registry::SpaceRegistry;

use super::active::{ChatActiveRuns, ChatControl, RunRegistration};
use super::feed::{LiveFrames, TurnFeed};
use super::sse::{
    ChatSseEvent, SubAgentInfo, UsageData, extract_sub_agent_result, map_looper_event,
    parse_sub_agent_infos, strip_suffix,
};

/// 一轮事件的出口。
pub(crate) enum RunSink {
    /// 有 HTTP 订阅者的一轮：出口是 feed，订阅者的来去与这一轮无关。
    Live(Arc<TurnFeed>),
    /// 唤醒轮：没有前端在看，事件只需进 feed（快照与终态判定照旧工作）。
    Headless(Arc<TurnFeed>),
}

impl RunSink {
    /// 送出一帧；永不失败 —— 失败面被 [`TurnFeed::publish`] 内部消化掉了。
    fn send(&self, event: ChatSseEvent) -> u64 {
        self.feed().publish(event)
    }

    fn feed(&self) -> &Arc<TurnFeed> {
        match self {
            Self::Live(feed) | Self::Headless(feed) => feed,
        }
    }

    /// 只决定唤醒侧副作用（唤醒信号、首轮标题），不决定运行生命周期。
    fn is_headless(&self) -> bool {
        matches!(self, Self::Headless(_))
    }
}

/// 唤醒轮结束的信号：Drop 即广播一条 `wake_turn_done`。
///
/// 挂在 Drop 上而不是在循环末尾写一句调用：早退（查询送不进去、控制通道关闭、
/// 看门狗到点）与 panic 展开都会跳过末尾语句，而「前端不刷新就看不到这一轮」
/// 恰恰要在这些路径上也成立。
struct WakeTurnSignal {
    tasks: Arc<crate::space::tasks::TaskObserver>,
    ws_id: String,
    conversation_id: String,
}

impl Drop for WakeTurnSignal {
    fn drop(&mut self) {
        self.tasks
            .notify_wake_turn(&self.ws_id, &self.conversation_id);
    }
}

/// 任务收尾守卫：Drop 时保证 feed 已经发出 `done`。
///
/// 正常路径由事件循环自己发终态帧；早退（查询送不进去）与 panic 展开会跳过它，
/// 订阅者就会一直等一个永远不来的 `done`。挂在 Drop 上而不是每处补调用：这些
/// 路径都要收敛，漏一处就是一条永远不断流的响应。
struct TurnEndGuard {
    feed: Arc<TurnFeed>,
}

impl Drop for TurnEndGuard {
    fn drop(&mut self) {
        self.feed.finish();
    }
}

/// 一轮「多久没有 looper 事件」的上限，**只在没有任何订阅者时**生效。
///
/// detach 之后 SSE 轮也可能永远没人看，而守卫不 drop 会永久占住该会话的注册
/// 条目、让唤醒队列永久让路（`wake::deliver` 的 `is_busy` 会一直为真）。
///
/// 判据有两半（见 [`idle_timeout_ends_the_turn`]）：**没有人在看这一轮**，且
/// **事件空闲时长到点**（每收到一条事件就重置，故慢但在产出的长回答不算）。有人在
/// 看就不掐断：一次长工具调用在 `ToolCallStart` 之后、第一个工具结果之前不发任何
/// 事件，把这段沉默当成死轮等于掐断用户正在读的对话。上限取 10 分钟，比台账的
/// [`crate::wake::DELIVERING_TIMEOUT`] 长一个数量级。
pub(crate) const RUN_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// 到点是否真要收尾（判据见 [`RUN_IDLE_TIMEOUT`]）。单独成函数是为了能单测 ——
/// 循环体在 `spawn_run` 的 `tokio::spawn` 里，装配它要真实 DB、已打开的空间与带
/// provider 的 Agent。
fn idle_timeout_ends_the_turn(has_subscribers: bool) -> bool {
    !has_subscribers
}

// 暂停无时限，也不留替代性的兜底超时。代价是明确的：暂停中的轮会一直占着
// `ChatActiveRuns` 的条目 —— 该会话再发消息是 409，唤醒队列也一直让路 —— 直到
// 用户 Resume 或 Cancel。这是要的语义：暂停若会自己到点取消，用户回来时看到的
// 就是一句悄悄消失的对话。条目何时释放，完全由用户决定。

/// 这一轮有没有 HTTP 订阅者。
///
/// **只**影响唤醒侧副作用：唤醒轮要发 `wake_turn_done`、不改会话标题。运行生命
/// 周期（什么时候收尾、落不落库）与它无关。
pub(crate) enum RunAudience {
    Live,
    Headless,
}

/// 一次运行所需的外部依赖。
///
/// 只取运行真正用到的几件，而不是整个 `AppState`：唤醒侧的装配本就绕不开
/// `AppState`，运行再反过来持有一条就会成环，而它也不需要 HTTP 配置与广播入口。
pub(crate) struct RunDeps<'a> {
    pub pool: &'a SqlitePool,
    /// looper 配置与派发 hook 的唯一装配点。
    pub env: &'a SpaceEnv,
    /// 已打开实例表：空间未打开时的失败面只从这里来。
    pub spaces: &'a SpaceRegistry,
    /// 会话级运行注册表：注册由 [`spawn_run`] 在装配校验之后完成。
    pub chat_runs: &'a ChatActiveRuns,
}

/// 装配失败面：只覆盖 spawn 之前的一段，跑起来之后的错误走日志与落库。
#[derive(Debug)]
pub(crate) enum RunError {
    /// 空间未索引 / 未打开。
    Space(DaemonError),
    /// 会话记着的 agent 已不在。
    Agent(String),
    /// agent 装不上，且原因是 provider 配置不可用（`diagnose` 给出的分类 + core 原文）。
    Provider(ProviderFailure, String),
    /// 该会话已有在途运行（这一轮在跑，与有没有人连着无关）。
    Busy,
}

/// 用必达的 `TurnComplete` 文本补齐落库累加器。
///
/// batch 轮的 `assistant_text` 全部来自可丢弃的 `ModelOutput(Batch)`：事件被
/// `try_send` 丢掉时，成功轮会写不出 assistant 行。`TurnComplete` 是必达的，
/// 用它补上最后的整段文本。`saw_text_delta` 为真说明是流式轮、文本已逐块累加
/// （掉一块后再补整段会让已到达的片段重复），故不补。
fn backfill_assistant_text(
    assistant_text: &mut String,
    saw_text_delta: bool,
    final_text: Option<&str>,
) {
    let Some(text) = final_text else { return };
    if saw_text_delta || text.is_empty() || assistant_text.ends_with(text) {
        return;
    }
    assistant_text.push_str(text);
}

/// 记下本轮的 token 用量 —— `TurnComplete` 是它唯一出现的载荷。
///
/// 走到 Idle 时本轮会话已不可读（`total_usage()` 随之不可再取），而结束帧要带的
/// 用量只能是这一轮的量，故在事件扫过来时当场留下。
fn remember_turn_usage(last_usage: &mut Usage, event: &LooperEvent) {
    if let LooperEvent::TurnComplete { usage, .. } = event {
        *last_usage = usage.clone();
    }
}

/// 一轮收尾要下发的 SSE 事件序列：先那条状态迁移，再结束帧。
///
/// `done` 是「流结束」的唯一信号 —— 少了它，客户端只能把每次正常收尾当成意外
/// 断流，进而去重取。用量来自本轮的 `TurnComplete.usage`（即
/// `session.total_usage()`），与 `Shutdown.total_usage` 同源。
fn turn_end_events(
    state_change: LooperEvent,
    last_usage: Usage,
    conversation_id: &str,
) -> Vec<ChatSseEvent> {
    let mut events: Vec<ChatSseEvent> = map_looper_event(state_change, conversation_id)
        .into_iter()
        .collect();
    events.push(ChatSseEvent::Done {
        usage: last_usage.into(),
        conversation_id: conversation_id.to_string(),
    });
    events
}

/// 跑一轮：把 `message` / `content` 交给会话的 looper，事件经 feed 出去，
/// assistant 文本与工具产物照旧落库。
///
/// 返回这一轮的订阅端（首帧快照 + 后续帧）。`RunAudience::Headless` 的调用方直接
/// 丢弃它 —— 唤醒轮没有人在看帧。
///
/// 返回 `Ok` 表示校验全部通过、控制通道已登记、looper 已 spawn。查询的投递与
/// user 行的落库都发生在 spawn 出的任务内，早于本函数返回无法保证；投递失败作为
/// 一条错误帧送出，不再回传。
pub(crate) async fn spawn_run(
    deps: RunDeps<'_>,
    ws_id: &str,
    conv: &ConversationRow,
    message: String,
    content: Content,
    audience: RunAudience,
) -> Result<LiveFrames, RunError> {
    let conv_id = conv.id.clone();

    // ── 1. 加载空间实例与 Agent（从 agent_name 字段，非 agent_id）───────
    // 经 `SpaceRegistry` 取实例：空间未打开时是 409，路径与四个资源模块一致。
    let ws = deps
        .spaces
        .workspace(ws_id)
        .await
        .map_err(RunError::Space)?;
    // team_id 先取：agent 加载失败时要靠它重算层列表，才能判断失败是不是
    // 「provider 配置不可用」这一类。
    let team_id = deps.spaces.team_id(ws_id).await.map_err(RunError::Space)?;
    let agent = ws
        .agent_manager()
        .load_cached(&conv.agent_name)
        .map_err(|e| {
            let detail = format!("failed to load agent '{}': {e}", conv.agent_name);
            // 先判配置、再保留 core 的原文当证据：分类来自配置判定，不来自解析
            // core 的英文报错（那些文案会演进）。
            match provider::diagnose(&ws.config(), &deps.spaces.provider_layer_roots(&team_id)) {
                Some(failure) => RunError::Provider(failure, detail),
                None => RunError::Agent(detail),
            }
        })?;

    // ── 2. 加载或创建 Session ────────────────────────────────────────────
    let persister = SqliteSessionPersister::new(deps.pool.clone());
    let mut session: Box<Session> = match persister.load(&conv_id).await {
        Ok(Some((snapshot, _meta))) => {
            tracing::info!(
                conversation_id = %conv_id,
                turns = snapshot.committed_turns.len(),
                "Session restored from snapshot"
            );
            let created_at = snapshot
                .committed_turns
                .first()
                .and_then(|t| t.first())
                .map(|m| m.timestamp_ms / 1000)
                .unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                });
            Box::new(Session::from_snapshot(
                conv_id.clone(),
                conv.title.clone(),
                created_at,
                snapshot,
            ))
        }
        Err(e) => {
            tracing::warn!(
                conversation_id = %conv_id,
                error = %e,
                "failed to load session snapshot; starting a new session (history lost)"
            );
            Box::new(Session::new(conv_id.clone(), conv.title.clone()))
        }
        Ok(None) => {
            tracing::info!(conversation_id = %conv_id, "Creating new session");
            Box::new(Session::new(conv_id.clone(), conv.title.clone()))
        }
    };

    // ── 2b. 崩溃恢复：冻结上次进程留下的在途轮 ────────────────────────────
    hydrate_inflight_turn(&persister, &mut session).await;

    // 「首轮」据水化**之后**的历史判定 —— 水化刚冻结的轮次同样是历史，
    // 否则新一轮消息会去覆盖一个已有内容的会话标题。
    let is_first_turn = session.committed_turns().is_empty();

    // ── 3. 登记本次运行的控制端 ──────────────────────────────────────────
    // 注册落在装配校验之后（理由见模块文档）；有在途轮就让路 `Busy`。
    let RunRegistration {
        mut control_rx,
        guard,
        feed,
    } = deps
        .chat_runs
        .try_register(&conv_id)
        .ok_or(RunError::Busy)?;

    // 受众在首帧快照里下发，晚于 `feed.start()` 就赶不上了。
    if matches!(audience, RunAudience::Headless) {
        feed.mark_headless();
    }
    let sink = match audience {
        RunAudience::Live => RunSink::Live(Arc::clone(&feed)),
        RunAudience::Headless => RunSink::Headless(Arc::clone(&feed)),
    };
    // 订阅端在 spawn 之前取：此刻 feed 还没有任何帧、也必然未进入终态。放到
    // spawn 之后取，一轮极快的对话可能已经发完 `done`，这一轮的首帧就没了。
    let frames = feed.start();

    // ── 4. 装配 looper 配置 ──────────────────────────────────────────────
    // 观察 hook 与动态上下文在 spawn 之前装配：两者都要读 `ws` 的 work_dir 与
    // `team_id`，而这两者随后会被 move 进后台任务。压缩与收尾报告挂在这条聊天
    // 路径上，摘要模型取自本轮 agent 的 provider（跨空间不必配第二份模型）。
    let config = deps.env.chat_looper_config(
        ws_id,
        &team_id,
        ws.work_dir().root().to_path_buf(),
        Arc::clone(agent.provider()),
        // 记忆写进本轮空间自己的库：同一个团队的空间共用一套 KB，标签才分得开。
        Arc::clone(ws.knowledge_manager()),
        // 压缩日志与边界观测按会话归档，指标才能只读本会话的时间线。
        &conv_id,
    );

    let db_for_bg = deps.pool.clone();
    // 唤醒轮没有客户端，只能靠广播告诉前端「这个会话里多了一条消息」。
    let tasks_for_bg = Arc::clone(&deps.env.tasks);
    let ws_id_for_bg = ws_id.to_string();
    let conv_id_for_bg = conv_id.clone();
    let message_for_bg = message.clone();
    let message_content_for_bg = content;
    let team_id_for_bg = team_id;
    let ws_for_bg = ws.clone();

    // ── 5. 后台执行 AgentLooper ──────────────────────────────────────────
    tokio::spawn(async move {
        // 持有至任务结束：守卫 drop 才移除注册表条目（见 `chat::active`）。
        let _run_guard = guard;
        // 声明在 `_run_guard` 之后 ⇒ 先 drop：终态帧早于条目移除（订阅者据此把
        // 「拿不到在途轮」读成历史权威），晚于下面所有落库（I1：先落库，后收尾）。
        let _end_guard = TurnEndGuard {
            feed: Arc::clone(sink.feed()),
        };

        // 只有无客户端的一轮需要这条信号：HTTP 入口的前端已经在看帧流。
        let _wake_turn_signal = sink.is_headless().then(|| WakeTurnSignal {
            tasks: tasks_for_bg,
            ws_id: ws_id_for_bg,
            conversation_id: conv_id_for_bg.clone(),
        });

        let persister: Arc<dyn SessionPersister> =
            Arc::new(SqliteSessionPersister::new(db_for_bg.clone()));

        let handle = AgentLooper::spawn(agent, session, config, persister.clone());

        if let Err(e) = handle.send_query_content(message_content_for_bg).await {
            // 查询没送进去：这一轮从未开始，但订阅者已经在等帧流，收尾要给出解释
            // （`_end_guard` 只兜 `done`）。
            sink.send(ChatSseEvent::Error {
                message: format!("Failed to send message: {e}"),
                cancelled: false,
                conversation_id: conv_id_for_bg.clone(),
            });
            return;
        }

        let _ = conversations::touch(&db_for_bg, &conv_id_for_bg).await;

        // 落库失败不影响这一轮，但必须留痕：唤醒轮里「台账记 delivered、消息却
        // 没进表」是没有任何 UI 症状的丢消息。
        if let Err(e) = messages::insert(
            &db_for_bg,
            &Uuid::new_v4().to_string(),
            &conv_id_for_bg,
            "user",
            &message_for_bg,
            None,
            None,
        )
        .await
        {
            tracing::warn!(
                conversation_id = %conv_id_for_bg,
                error = %e,
                "user message not persisted"
            );
        }

        let mut assistant_text = String::new();
        // 流式轮是否真的收到过增量：决定 TurnComplete 的兜底能否追加整段文本。
        let mut saw_text_delta = false;
        // 最近一轮的用量：轮末的结束帧只能用它（见 `remember_turn_usage`）。
        let mut last_usage = Usage::default();
        let mut sub_agent_registry: HashMap<String, Vec<SubAgentInfo>> = HashMap::new();
        // is_first_turn is captured from the outer scope — based on session
        // snapshot committed_turns, NOT reset per turn.
        let mut is_first_turn = is_first_turn;
        // 本轮 assistant 正文是否已经写过库。兜底落库靠它保持幂等：一次运行只投
        // 一条查询（steering 的续轮由另一次运行承担），故「本轮」= 本任务。
        let mut persisted_this_turn = false;
        // 终态帧（`done`）是否已经发出。没有就在收尾时补一条。
        let mut terminal_published = false;
        // 非正常退出的解释帧：不补的话订阅者只能把「没有 done 的断流」当成网络故障。
        let mut abnormal_exit: Option<ChatSseEvent> = None;

        let resolve_agent_id = |agent_name: &str| -> String {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(agents::find_id_by_name_and_team(
                    &db_for_bg,
                    agent_name,
                    &team_id_for_bg,
                ))
            })
            .ok()
            .flatten()
            .unwrap_or_else(|| "unknown".to_string())
        };

        // 空闲看门狗：见 [`RUN_IDLE_TIMEOUT`]，每条事件重置。
        let idle_deadline = tokio::time::sleep(RUN_IDLE_TIMEOUT);
        tokio::pin!(idle_deadline);
        // 是否处在暂停态。暂停无时限：看门狗在暂停期间整个关掉（见下方空闲分支）。
        let mut paused = false;

        loop {
            // 三路复用：looper 事件 / 空闲看门狗 / 控制命令。暂停中空闲那一路
            // 整个关掉，于是只剩事件与控制 —— 暂停因此没有时限。
            //
            // 没有「客户端断连」这一路 —— 连接只是出口之一，它的消失不是事件。
            tokio::select! {
                event = handle.recv_event() => {
                    // 每收到一条事件就重置空闲计时：判据是「多久没有产出」。
                    idle_deadline.as_mut().reset(Instant::now() + RUN_IDLE_TIMEOUT);
                    match event {
                    Some(LooperEvent::TextDelta { delta }) => {
                        saw_text_delta = true;
                        assistant_text.push_str(&delta);
                        if let Some(sse_ev) =
                            map_looper_event(LooperEvent::TextDelta { delta }, &conv_id_for_bg)
                        {
                            sink.send(sse_ev);
                        }
                    }

                    // ── batch 模型输出 ───────────────────────────────────────
                    // 与 TextDelta 共用同一条落库累加器：帧流展示了、messages 表却是
                    // 空的，是比「不展示」更糟的不一致。
                    Some(LooperEvent::ModelOutput {
                        call_index,
                        origin,
                        output,
                    }) if origin == ResponseOrigin::Batch => {
                        assistant_text.push_str(&text_of_blocks(&output));
                        // 文本提取只借用，随后把同一份 `output` 原样交给映射层
                        // （不 clone、也不需要再解构一次事件）。
                        let event = LooperEvent::ModelOutput {
                            call_index,
                            origin,
                            output,
                        };
                        if let Some(sse_ev) = map_looper_event(event, &conv_id_for_bg) {
                            sink.send(sse_ev);
                        }
                    }

                    // ── 子 Agent 调用 ────────────────────────────────────────
                    Some(LooperEvent::ToolCallStart {
                        ref id,
                        ref name,
                        ref arguments,
                    }) if name == "delegate_sub_agent" || name == "run_parallel_sub_agents" => {
                        let infos = parse_sub_agent_infos(id, name, arguments, resolve_agent_id);

                        for info in &infos {
                            let task = if name == "delegate_sub_agent" {
                                serde_json::from_str::<serde_json::Value>(arguments)
                                    .ok()
                                    .and_then(|v| v["prompt"].as_str().map(|s| s.to_string()))
                                    .unwrap_or_default()
                            } else {
                                String::new()
                            };

                            sink.send(ChatSseEvent::AgentCallStart {
                                call_id: info.call_id.clone(),
                                agent_id: info.agent_id.clone(),
                                agent_name: info.agent_name.clone(),
                                task,
                                conversation_id: conv_id_for_bg.clone(),
                            });
                        }
                        sub_agent_registry.insert(id.clone(), infos);

                        if let Some(sse_ev) = map_looper_event(
                            LooperEvent::ToolCallStart {
                                id: id.clone(),
                                name: name.clone(),
                                arguments: arguments.clone(),
                            },
                            &conv_id_for_bg,
                        ) {
                            sink.send(sse_ev);
                        }
                    }

                    // ── 子 Agent 调用结束 ────────────────────────────────────
                    Some(LooperEvent::ToolResult {
                        ref id,
                        ref name,
                        ref result,
                        ref artifacts,
                        ..
                    }) if name == "delegate_sub_agent" || name == "run_parallel_sub_agents" => {
                        if let Some(infos) = sub_agent_registry.remove(id.as_str()) {
                            for info in infos {
                                let sub_result = extract_sub_agent_result(result, &info, name);
                                sink.send(ChatSseEvent::AgentCallEnd {
                                    call_id: info.call_id,
                                    agent_id: info.agent_id,
                                    agent_name: info.agent_name,
                                    result: sub_result,
                                    conversation_id: conv_id_for_bg.clone(),
                                });
                            }
                        }

                        // 子 agent 的最终正文若带声明行，父层构造结果时已剥离并挂在
                        // 本事件上；`AgentCallEnd` 只带文本，不能吞掉 —— 必须同样落库。
                        let sse_ev = crate::artifact::build_tool_result_event(
                            &db_for_bg,
                            crate::artifact::work_dir(&ws_for_bg),
                            &conv_id_for_bg,
                            LOCAL_USER_ID,
                            id.clone(),
                            name.clone(),
                            String::new(),
                            Vec::new(),
                            artifacts,
                        )
                        .await;
                        sink.send(sse_ev);
                    }

                    Some(event @ LooperEvent::TurnComplete { .. }) => {
                        remember_turn_usage(&mut last_usage, &event);
                        if let LooperEvent::TurnComplete { outcome, .. } = &event {
                            backfill_assistant_text(
                                &mut assistant_text,
                                saw_text_delta,
                                outcome.text(),
                            );
                        }
                        if !assistant_text.is_empty() {
                            // 与 user 行同理：落库失败只留痕，不影响这一轮。
                            match messages::insert(
                                &db_for_bg,
                                &Uuid::new_v4().to_string(),
                                &conv_id_for_bg,
                                "assistant",
                                &assistant_text,
                                None,
                                None,
                            )
                            .await
                            {
                                Ok(()) => persisted_this_turn = true,
                                Err(e) => tracing::warn!(
                                    conversation_id = %conv_id_for_bg,
                                    error = %e,
                                    "assistant message not persisted"
                                ),
                            }

                            // 仅首轮对话时根据用户第一条消息自动生成标题；唤醒轮没有
                            // 用户，把标题改成 `[peco-wake] …` 只会在列表里留一条没人
                            // 认得的记录。
                            if is_first_turn && !sink.is_headless() {
                                let short: String = message_for_bg.chars().take(50).collect();
                                let new_title = if short.len() >= message_for_bg.len() {
                                    short
                                } else {
                                    format!("{short}...")
                                };
                                if new_title.len() > 3 {
                                    let _ = conversations::update_title(
                                        &db_for_bg,
                                        &conv_id_for_bg,
                                        &new_title,
                                    )
                                    .await;
                                }
                                is_first_turn = false;
                            }
                        }
                        assistant_text.clear();
                        saw_text_delta = false;

                        if let Some(sse_ev) = map_looper_event(event, &conv_id_for_bg) {
                            sink.send(sse_ev);
                        }
                    }

                    // ── 截断重试 ─────────────────────────────────────────────
                    // 就地剥离被作废那次的正文 —— 它不属于最终答案，不该进
                    // `messages` 表。前端侧不删自己已收到的增量，只把这条事件当
                    // 通知看：落库内容对齐最终答案、帧流内容对齐用户一路看到的
                    // 东西，两者在这里分道。
                    Some(event @ LooperEvent::TruncationRetry { .. }) => {
                        if let LooperEvent::TruncationRetry { discarded_text, .. } = &event {
                            strip_suffix(&mut assistant_text, discarded_text);
                        }
                        if let Some(sse_ev) = map_looper_event(event, &conv_id_for_bg) {
                            sink.send(sse_ev);
                        }
                    }

                    Some(LooperEvent::Shutdown { total_usage, .. }) => {
                        sink.send(ChatSseEvent::Done {
                            usage: UsageData::from(total_usage),
                            conversation_id: conv_id_for_bg.clone(),
                        });
                        terminal_published = true;
                        break;
                    }

                    // ── 工具声明的产物 ────────────────────────────────────────
                    // 必须先落库再发事件：事件里的 id 是前端访问内容/下载端点的
                    // 唯一凭据，不能等到前端自己上报。带产物的 ToolResult 因此
                    // 不进 `map_looper_event`（那是纯函数，拿不到 DB）。
                    Some(LooperEvent::ToolResult {
                        ref id,
                        ref name,
                        ref result,
                        ref images,
                        ref artifacts,
                    }) if !artifacts.is_empty() => {
                        let sse_ev = crate::artifact::build_tool_result_event(
                            &db_for_bg,
                            crate::artifact::work_dir(&ws_for_bg),
                            &conv_id_for_bg,
                            LOCAL_USER_ID,
                            id.clone(),
                            name.clone(),
                            result.clone(),
                            images.clone(),
                            artifacts,
                        )
                        .await;
                        sink.send(sse_ev);
                    }

                    // 一轮结束在 Idle：一轮一条流，注册条目随任务 drop，于是「忙」
                    // 等于「这一轮真的在跑」，而不是「连接还开着」。
                    // `set()` 对同态迁移是 no-op，收到 Idle 即意味着这一轮真的停了。
                    //
                    // 终态帧一律发：状态迁移承载「迁出暂停态」，少了它一轮结束后
                    // 订阅者会停在「暂停中」。顺序固定 —— 先状态、后结束帧。
                    // 没有订阅者时是两次内存写，代价远小于「这一轮永远不结束」。
                    Some(event @ LooperEvent::StateChange {
                        to: LooperState::Idle,
                        ..
                    }) => {
                        for sse_ev in turn_end_events(event, last_usage.clone(), &conv_id_for_bg)
                        {
                            sink.send(sse_ev);
                        }
                        terminal_published = true;
                        break;
                    }

                    // 暂停态以 looper 的状态迁移为准：控制命令只表示意图，落地点在
                    // 边界检查点。
                    Some(event @ LooperEvent::StateChange { to, .. }) => {
                        if to.is_paused() {
                            paused = true;
                        } else if paused {
                            // 恢复：空闲计时在暂停期间已经过期，重新起算。
                            paused = false;
                            idle_deadline.as_mut().reset(Instant::now() + RUN_IDLE_TIMEOUT);
                        }
                        if let Some(sse_ev) = map_looper_event(event, &conv_id_for_bg) {
                            sink.send(sse_ev);
                        }
                    }

                    Some(event) => {
                        if let Some(sse_ev) = map_looper_event(event, &conv_id_for_bg) {
                            sink.send(sse_ev);
                        }
                    }

                    None => {
                        // `Shutdown` 是必达事件，走到这里说明 looper 没走正常收尾就没了。
                        tracing::warn!(
                            conversation_id = %conv_id_for_bg,
                            "looper event stream ended without a shutdown event"
                        );
                        abnormal_exit = Some(ChatSseEvent::Error {
                            message: "本轮意外结束".to_string(),
                            cancelled: false,
                            conversation_id: conv_id_for_bg.clone(),
                        });
                        break;
                    }
                    }
                }

                // 空闲到点：见 [`RUN_IDLE_TIMEOUT`]。
                _ = &mut idle_deadline, if !paused => {
                    if !idle_timeout_ends_the_turn(sink.feed().has_subscribers()) {
                        // 不发帧、不改状态：又一个空闲窗口过去不是用户可见的事件。
                        idle_deadline.as_mut().reset(Instant::now() + RUN_IDLE_TIMEOUT);
                        continue;
                    }
                    tracing::warn!(
                        conversation_id = %conv_id_for_bg,
                        timeout_secs = RUN_IDLE_TIMEOUT.as_secs(),
                        "run produced no event within the idle timeout; ending the turn"
                    );
                    abnormal_exit = Some(ChatSseEvent::Error {
                        message: "这一轮长时间没有任何产出，服务端已收尾（已产出的正文照旧落库）"
                            .to_string(),
                        cancelled: false,
                        conversation_id: conv_id_for_bg.clone(),
                    });
                    break;
                }

                // 暂停 / 恢复 / 取消：来自 `POST .../pause|resume|cancel`。
                cmd = control_rx.recv() => {
                    idle_deadline.as_mut().reset(Instant::now() + RUN_IDLE_TIMEOUT);
                    match cmd {
                        // 无客户端的运行没有人能发 Resume：真去 pause 会把 looper 与守卫
                        // 一起钉在暂停态，该会话的唤醒队列从此永久让路。
                        Some(ChatControl::Pause) if sink.is_headless() => {
                            tracing::warn!(
                                conversation_id = %conv_id_for_bg,
                                "headless run ignores pause; no client can resume it"
                            );
                        }
                        Some(ChatControl::Pause) => {
                            if let Err(e) = handle.pause().await {
                                tracing::warn!(
                                    conversation_id = %conv_id_for_bg,
                                    error = %e,
                                    "Failed to deliver pause"
                                );
                            }
                        }
                        Some(ChatControl::Resume) if sink.is_headless() => {
                            tracing::warn!(
                                conversation_id = %conv_id_for_bg,
                                "headless run has nothing to resume"
                            );
                        }
                        Some(ChatControl::Resume) => {
                            if let Err(e) = handle.resume().await {
                                tracing::warn!(
                                    conversation_id = %conv_id_for_bg,
                                    error = %e,
                                    "Failed to deliver resume"
                                );
                            }
                        }
                        Some(ChatControl::Query(content)) => {
                            // 用户行由本任务写：原消息行已在进入本循环前写完，故这里写出
                            // 的行必然排在它之后。交给 handler 写会与那次插入竞争（同一
                            // 秒内按 rowid 排，历史顺序会反过来）。
                            let text = content.text_view_plain();
                            if let Err(e) = messages::insert(
                                &db_for_bg,
                                &Uuid::new_v4().to_string(),
                                &conv_id_for_bg,
                                "user",
                                &text,
                                None,
                                None,
                            )
                            .await
                            {
                                tracing::warn!(
                                    conversation_id = %conv_id_for_bg,
                                    error = %e,
                                    "steered user message not persisted"
                                );
                            }

                            // 注入只是把用户输入排进这一轮的 pending：与 pause 不同，
                            // 它不会把队列钉死，故 headless 轮不需要特判。送不进去说明
                            // looper 已经退出，订阅者很快会收到 `done`，不必额外收尾。
                            if let Err(e) = handle.send_query_content(content).await {
                                tracing::warn!(
                                    conversation_id = %conv_id_for_bg,
                                    error = %e,
                                    "Failed to deliver steering input"
                                );
                            }
                        }
                        Some(ChatControl::Cancel) => {
                            // 取消是会话级命令：先让快照进入 cancelling（此刻及之后
                            // 的订阅者据此渲染「正在停止」），收尾在 looper 的步进
                            // 边界，可能晚于 HTTP 响应。
                            sink.feed().mark_cancelling();
                            if let Err(e) = handle.cancel().await {
                                tracing::warn!(
                                    conversation_id = %conv_id_for_bg,
                                    error = %e,
                                    "Failed to deliver cancel"
                                );
                            }
                        }
                        None => {
                            // 控制发送端关闭（条目被摘除/顶替）：没有继续的理由。
                            tracing::warn!(
                                conversation_id = %conv_id_for_bg,
                                "control channel closed; ending the turn"
                            );
                            abnormal_exit = Some(ChatSseEvent::Error {
                                message: "本轮的控制端已关闭，服务端已收尾".to_string(),
                                cancelled: false,
                                conversation_id: conv_id_for_bg.clone(),
                            });
                            break;
                        }
                    }
                }
            }
        }

        // ── 收尾（所有退出路径共用）────────────────────────────────────────
        // 兜底落库：中途退出（空闲超时、控制端关闭、looper 意外结束）
        // 走不到 `TurnComplete` 分支，这一轮已产出的正文只活在内存里。这是「不丢」
        // 的下限，与轮末的正常写入不重复：单任务串行 + `persisted_this_turn`。
        // 顺序即 I1：落库严格早于终态帧（`_end_guard` 补的 `done` 同样在后面）。
        if !persisted_this_turn
            && !assistant_text.is_empty()
            && let Err(e) = messages::insert(
                &db_for_bg,
                &Uuid::new_v4().to_string(),
                &conv_id_for_bg,
                "assistant",
                &assistant_text,
                None,
                None,
            )
            .await
        {
            tracing::warn!(
                conversation_id = %conv_id_for_bg,
                error = %e,
                "salvage insert of the partial assistant text failed"
            );
        }

        if let Some(err) = abnormal_exit {
            sink.send(err);
        }
        if !terminal_published {
            sink.send(ChatSseEvent::Done {
                usage: last_usage.into(),
                conversation_id: conv_id_for_bg.clone(),
            });
        }

        drop(handle);
    });

    Ok(frames)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use peco_core::agent::TurnOutcome;

    use super::*;

    #[test]
    fn backfills_when_no_delta_was_seen() {
        let mut acc = String::new();
        backfill_assistant_text(&mut acc, false, Some("answer"));
        assert_eq!(acc, "answer");
    }

    #[test]
    fn appends_final_text_when_earlier_batch_calls_landed() {
        let mut acc = "first".to_string();
        backfill_assistant_text(&mut acc, false, Some("answer"));
        assert_eq!(acc, "firstanswer");
    }

    #[test]
    fn does_not_duplicate_when_final_text_is_already_the_suffix() {
        let mut acc = "firstanswer".to_string();
        backfill_assistant_text(&mut acc, false, Some("answer"));
        assert_eq!(acc, "firstanswer");
    }

    #[test]
    fn leaves_streamed_text_untouched() {
        // 流式轮已逐块累加；掉一块时补整段会让已到达的片段重复。
        let mut acc = " world".to_string();
        backfill_assistant_text(&mut acc, true, Some("hello world"));
        assert_eq!(acc, " world");
    }

    #[test]
    fn failed_turns_have_no_final_text() {
        let mut acc = String::new();
        backfill_assistant_text(&mut acc, false, None);
        assert!(acc.is_empty());
    }

    /// 一轮收尾非 headless 时必须紧跟一条 `done`，用量取自本轮 `TurnComplete`。
    ///
    /// 端到端跑不了：事件循环体在 `spawn_run` 的 `tokio::spawn` 里，装配它要真实
    /// DB、已打开的空间与带 provider 的 Agent（会真的调模型），crate 内单测构造
    /// 不出来。断言因此落在收尾序列与用量记忆这两段真实生产代码上。
    ///
    /// 变异方法：结束帧漏发 → 序列只剩 `looper_state`；用量取 `Usage::default()`
    /// 而非 `TurnComplete` 的值 → 用量断言变红。
    #[test]
    fn idle_exit_emits_done_carrying_the_turn_usage() {
        let turn_usage = Usage {
            input_tokens: 120,
            output_tokens: 34,
            total_tokens: 154,
        };
        let mut last_usage = Usage::default();
        remember_turn_usage(
            &mut last_usage,
            &LooperEvent::TurnComplete {
                turn_index: 2,
                outcome: TurnOutcome::Success {
                    text: "好了".to_string(),
                },
                usage: turn_usage.clone(),
            },
        );
        assert_eq!(last_usage, turn_usage, "TurnComplete 的用量必须被记住");

        let events = turn_end_events(
            LooperEvent::StateChange {
                from: LooperState::PausedDuringTools,
                to: LooperState::Idle,
            },
            last_usage,
            "conv-1",
        );

        assert!(
            matches!(
                events.first(),
                Some(ChatSseEvent::LooperState {
                    paused: false,
                    conversation_id,
                }) if conversation_id == "conv-1"
            ),
            "迁出暂停态的状态事件在前，实际 {events:?}"
        );
        let Some(ChatSseEvent::Done {
            usage,
            conversation_id,
        }) = events.get(1)
        else {
            panic!("`done` 必须紧跟状态事件，且是最后一条，实际 {events:?}");
        };
        assert_eq!(events.len(), 2, "收尾只有这两条，实际 {events:?}");
        assert_eq!(usage.input_tokens, turn_usage.input_tokens);
        assert_eq!(usage.output_tokens, turn_usage.output_tokens);
        assert_eq!(conversation_id, "conv-1");
    }

    /// 未跨暂停态的 Idle 没有状态事件可发，但 `done` 照发 —— 否则最常见的
    /// 「一轮跑完正常退出」反而收不到结束帧。
    #[test]
    fn idle_exit_without_pause_transition_still_emits_done() {
        let events = turn_end_events(
            LooperEvent::StateChange {
                from: LooperState::PreparingRequest,
                to: LooperState::Idle,
            },
            Usage::default(),
            "conv-1",
        );
        assert!(
            matches!(events.as_slice(), [ChatSseEvent::Done { .. }]),
            "实际 {events:?}"
        );
    }

    /// 空闲到点只在没人看着这一轮时收尾。
    ///
    /// 变异方法：把返回值取反 → 两条断言互相颠倒，用例变红。
    #[test]
    fn the_idle_watchdog_ends_only_unwatched_turns() {
        assert!(
            idle_timeout_ends_the_turn(false),
            "没有订阅者：到点必须收尾，否则条目永久占住该会话的唤醒队列"
        );
        assert!(
            !idle_timeout_ends_the_turn(true),
            "有订阅者：到点只重置计时，掐断用户正在看的对话是回归"
        );
    }

    /// 裁决的入参就取自 feed：接起来跑一遍，防「有没有订阅者」被读成相反的极性。
    #[test]
    fn a_watched_turn_is_not_ended_by_an_idle_deadline() {
        let feed = Arc::new(TurnFeed::new("conv-1".to_string()));
        let frames = feed.start();
        assert!(!idle_timeout_ends_the_turn(feed.has_subscribers()));

        drop(frames);
        assert!(idle_timeout_ends_the_turn(feed.has_subscribers()));
    }

    /// 空闲看门狗必须明显长于台账的认领超时：反过来的话，一条还在正常跑的长回答会
    /// 先被回收成「认领方已死」，同一行被重投第二遍。
    #[test]
    fn the_idle_watchdog_outlives_the_claim_timeout() {
        assert!(
            RUN_IDLE_TIMEOUT > crate::wake::DELIVERING_TIMEOUT,
            "空闲上限 {RUN_IDLE_TIMEOUT:?} 不得短于认领超时"
        );
        assert!(
            RUN_IDLE_TIMEOUT >= Duration::from_secs(300),
            "上限太短会把正常的长回答掐断"
        );
    }

    /// 出口是 feed：没人订阅时 `send` 照旧分配序号并进快照。
    ///
    /// 变异方法：把 `send` 改回「发送失败即 false / 断流」→ 这里拿不到序号，
    /// 且集成用例 V1 的轮会随断连退出、assistant 行写不出来。
    #[test]
    fn a_live_sink_keeps_publishing_without_subscribers() {
        let feed = Arc::new(TurnFeed::new("conv-1".to_string()));
        let sink = RunSink::Live(Arc::clone(&feed));
        assert!(!sink.is_headless(), "HTTP 入口不是唤醒轮");

        let seq = sink.send(ChatSseEvent::TextDelta {
            content: "半段".to_string(),
            conversation_id: "conv-1".to_string(),
        });
        assert_eq!(seq, 1, "没有订阅者时序号照旧分配");
        assert_eq!(feed.snapshot().text, "半段", "快照是这一轮的唯一在途视图");
    }

    /// 唤醒轮的出口也是 feed（只是没人订阅），且 `is_headless` 仍然只描述唤醒侧。
    #[test]
    fn a_headless_sink_publishes_into_its_feed() {
        let feed = Arc::new(TurnFeed::new("conv-2".to_string()));
        let sink = RunSink::Headless(Arc::clone(&feed));
        assert!(sink.is_headless());

        sink.send(ChatSseEvent::ReasoningDelta {
            content: "想".to_string(),
            conversation_id: "conv-2".to_string(),
        });
        assert_eq!(feed.snapshot().reasoning, "想");
    }

    /// 收尾守卫必须在任何退出路径上补出 `done` —— 订阅者靠它结束帧流，缺一次就是
    /// 一条永不结束的响应。已经发过 `done` 时不得重复发（客户端会当成第二轮结束）。
    #[test]
    fn the_end_guard_closes_the_feed_exactly_once() {
        let feed = Arc::new(TurnFeed::new("conv-3".to_string()));
        let frames = feed.start();

        {
            let _guard = TurnEndGuard {
                feed: Arc::clone(&feed),
            };
        }
        // 第二次（正常收尾路径已经发过帧之后）什么都不做。
        let _guard = TurnEndGuard {
            feed: Arc::clone(&feed),
        };
        drop(_guard);

        let events = collect_wire_events(frames);
        assert_eq!(events.len(), 2, "首帧快照 + 一条 done，实际 {events:?}");
        assert_eq!(events[0].0, Some(0), "首帧是序号 0 的快照");
        assert_eq!(events[1].0, Some(1));
        assert!(
            events[1].1.starts_with("event: done"),
            "实际 {:?}",
            events[1]
        );
    }

    /// 把帧流收成 `(id, 报文首行)`，用来断言帧序、序号与事件名。
    pub(super) fn collect_wire_events(frames: LiveFrames) -> Vec<(Option<u64>, String)> {
        use futures::StreamExt;
        use futures::executor::block_on;

        let stream = super::super::feed::frame_stream(frames);
        block_on(stream.collect::<Vec<_>>())
            .into_iter()
            .map(|item| {
                let event = item.expect("帧流不会产出错误");
                let wire = wire_of(event);
                (sse_id(&wire), wire)
            })
            .collect()
    }

    /// `Event` 没有公开读接口，故把它重新走一遍 SSE 序列化拿到报文字节。
    fn wire_of(event: axum::response::sse::Event) -> String {
        use axum::response::IntoResponse;

        futures::executor::block_on(async {
            let body = axum::response::Sse::new(futures::stream::once(async move {
                Ok::<_, std::convert::Infallible>(event)
            }))
            .into_response()
            .into_body();
            let bytes = axum::body::to_bytes(body, usize::MAX)
                .await
                .expect("读取 SSE 报文失败");
            String::from_utf8(bytes.into()).expect("SSE 报文必须是 UTF-8")
        })
    }

    fn sse_id(wire: &str) -> Option<u64> {
        wire.lines()
            .find_map(|line| line.strip_prefix("id: "))
            .and_then(|value| value.trim().parse().ok())
    }
}
