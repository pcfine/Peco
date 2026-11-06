//! 一次会话运行的执行体：HTTP 的 SSE 流与无客户端的唤醒投递共用同一条 looper 链。
//!
//! 与 [`handler`](super::handler) 的分工：handler 只做鉴权、取参、会话归属与响应
//! 封装；把 user 文本交给 looper、把 assistant 文本与产物落库、把 looper 事件转成
//! SSE 都在这里。这些若各入口各写一份，前端看到的气泡就会与 `messages` 表分叉。
//!
//! 注册（这个会话谁在跑）在装配校验之后、looper spawn 之前完成，由 [`spawn_run`]
//! 自己做：HTTP 入口要顶替语义（页面重载得挤掉旧连接），唤醒入口要不顶替语义
//! （不能挤掉正在看页面的用户），差异只能落在 [`Registration`] 上。放进函数里
//! 是因为注册是顶替语义 —— 注册早于「空间是否打开 / agent 是否还在」的校验，
//! 一个注定 409 的请求就会把该会话在途的 run 挤掉。

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::response::sse::Event as SseEvent;
use model_provider::{Content, text_of_blocks};
use peco_core::agent::{AgentLooper, LooperEvent, LooperState, ResponseOrigin};
use peco_core::session::Session;
use peco_core::session::SessionPersister;
use sqlx::SqlitePool;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::LOCAL_USER_ID;
use crate::db::conversations::ConversationRow;
use crate::db::{agents, conversations, messages};
use crate::error::DaemonError;
use crate::session_store::{SqliteSessionPersister, hydrate_inflight_turn};
use crate::space::SpaceEnv;
use crate::space::registry::SpaceRegistry;

use super::active::{ChatActiveRuns, ChatControl, RunRegistration};
use super::sse::{
    ChatSseEvent, SubAgentInfo, UsageData, extract_sub_agent_result, map_looper_event,
    parse_sub_agent_infos, strip_suffix,
};

/// 事件的出口。
pub(crate) enum RunSink {
    /// HTTP 的 SSE 流：发送失败或连接关闭即「没有人再看这个会话了」。
    Sse(mpsc::Sender<Result<SseEvent, Infallible>>),
    /// 没有客户端的一次运行：事件没有出口，文本与产物照旧落库。
    Headless,
}

impl RunSink {
    /// 送出事件；`false` 表示客户端已断（`Headless` 没有可断的通道，恒为 `true`）。
    async fn send(&self, event: SseEvent) -> bool {
        match self {
            Self::Sse(tx) => tx.send(Ok(event)).await.is_ok(),
            Self::Headless => true,
        }
    }

    /// 客户端仍在的信号；`Headless` 没有客户端，等它等于永久等待。
    async fn closed(&self) {
        match self {
            Self::Sse(tx) => tx.closed().await,
            Self::Headless => std::future::pending().await,
        }
    }

    fn is_headless(&self) -> bool {
        matches!(self, Self::Headless)
    }
}

/// 无客户端一轮的整体上限。
///
/// `RunSink::Headless` 既没有断连信号、也没有人能发 Resume：一个卡住的模型调用会
/// 让这一轮永不结束，而守卫不 drop 就意味着该会话的唤醒队列被永久让路。上限取
/// 10 分钟 —— 比台账的 [`crate::wake::DELIVERING_TIMEOUT`] 长一个数量级，正常
/// 长回答不会被它掐断。
pub(crate) const HEADLESS_RUN_TIMEOUT: Duration = Duration::from_secs(600);

/// 这次运行怎么占住会话的注册表条目。
pub(crate) enum Registration {
    /// HTTP 入口：页面重载要靠它挤掉旧连接。
    Replace,
    /// 唤醒入口：不能挤掉正在看页面的用户，冲突即让路。
    TryNew,
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
    /// 该会话已有在途运行，且本次不允许顶替。
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

/// 跑一轮：把 `message` / `content` 交给会话的 looper，事件经 `sink` 出去，
/// assistant 文本与工具产物照旧落库。
///
/// 返回 `Ok` 表示校验全部通过、控制通道已登记、looper 已 spawn。查询的投递与
/// user 行的落库都发生在 spawn 出的任务内，早于本函数返回无法保证；投递失败作为
/// 一条错误事件送出，不再回传。
pub(crate) async fn spawn_run(
    deps: RunDeps<'_>,
    ws_id: &str,
    conv: &ConversationRow,
    message: String,
    content: Content,
    sink: RunSink,
    mode: Registration,
) -> Result<(), RunError> {
    let conv_id = conv.id.clone();

    // ── 1. 加载空间实例与 Agent（从 agent_name 字段，非 agent_id）───────
    // 经 `SpaceRegistry` 取实例：空间未打开时是 409，路径与四个资源模块一致。
    let ws = deps
        .spaces
        .workspace(ws_id)
        .await
        .map_err(RunError::Space)?;
    let agent = ws
        .agent_manager()
        .load_cached(&conv.agent_name)
        .map_err(|e| RunError::Agent(format!("failed to load agent '{}': {e}", conv.agent_name)))?;
    let team_id = deps.spaces.team_id(ws_id).await.map_err(RunError::Space)?;

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
    // 注册在装配校验之后：顶替语义下，一个注定 409/500 的请求不该把该会话在途
    // 的 run 挤掉（旧连接会因此被踢下线）。
    let RunRegistration {
        mut control_rx,
        guard,
    } = match mode {
        Registration::Replace => {
            let (control_rx, guard) = deps.chat_runs.register(&conv_id);
            RunRegistration { control_rx, guard }
        }
        Registration::TryNew => deps
            .chat_runs
            .try_register(&conv_id)
            .ok_or(RunError::Busy)?,
    };

    // ── 4. 装配 looper 配置 ──────────────────────────────────────────────
    // 观察 hook 与动态上下文在 spawn 之前装配：两者都要读 `ws` 的 work_dir 与
    // `team_id`，而这两者随后会被 move 进后台任务。
    let config = deps
        .env
        .looper_config(ws_id, &team_id, ws.work_dir().root().to_path_buf());

    let db_for_bg = deps.pool.clone();
    let conv_id_for_bg = conv_id.clone();
    let message_for_bg = message.clone();
    let message_content_for_bg = content;
    let team_id_for_bg = team_id;
    let ws_for_bg = ws.clone();

    // ── 5. 后台执行 AgentLooper ──────────────────────────────────────────
    tokio::spawn(async move {
        // 持有至任务结束：守卫 drop 才移除注册表条目（见 `chat::active`）。
        let _run_guard = guard;

        let persister: Arc<dyn SessionPersister> =
            Arc::new(SqliteSessionPersister::new(db_for_bg.clone()));

        let handle = AgentLooper::spawn(agent, session, config, persister.clone());

        if let Err(e) = handle.send_query_content(message_content_for_bg).await {
            let err_event = ChatSseEvent::Error {
                message: format!("Failed to send message: {e}"),
                cancelled: false,
                conversation_id: conv_id_for_bg.clone(),
            };
            if let Ok(ev) = err_event.to_sse_event() {
                let _ = sink.send(ev).await;
            }
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
        let mut sub_agent_registry: HashMap<String, Vec<SubAgentInfo>> = HashMap::new();
        // is_first_turn is captured from the outer scope — based on session
        // snapshot committed_turns, NOT reset per SSE connection.
        let mut is_first_turn = is_first_turn;

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

        // 无客户端一轮的整体看门狗：见 [`HEADLESS_RUN_TIMEOUT`]。SSE 侧不受影响
        // —— 那种运行有断连信号，用户随时能关掉页面。
        let headless_deadline = tokio::time::sleep(HEADLESS_RUN_TIMEOUT);
        tokio::pin!(headless_deadline);

        loop {
            // 三路复用：looper 事件 / 暂停恢复控制 / 客户端断连。
            //
            // 断连一路不可省 —— chat 链路没有 peco runner 那样的桥接任务，
            // 少了它，停靠（尤其暂停）中的 looper 会随无人观看的连接一直挂着。
            // axum 的 SSE keep-alive 写入失败后响应体被 drop，`closed()` 随即就绪。
            // 无客户端的运行没有这条信号，改由下面 looper 事件里的 Idle 分支退出。
            tokio::select! {
                _ = sink.closed() => break,

                event = handle.recv_event() => match event {
                Some(LooperEvent::TextDelta { delta }) => {
                    saw_text_delta = true;
                    assistant_text.push_str(&delta);
                    if let Some(sse_ev) =
                        map_looper_event(LooperEvent::TextDelta { delta }, &conv_id_for_bg)
                        && let Ok(ev) = sse_ev.to_sse_event()
                        && !sink.send(ev).await
                    {
                        break;
                    }
                }

                // ── batch 模型输出 ───────────────────────────────────────
                // 与 TextDelta 共用同一条落库累加器：SSE 展示了、messages 表却是
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
                    if let Some(sse_ev) = map_looper_event(event, &conv_id_for_bg)
                        && let Ok(ev) = sse_ev.to_sse_event()
                        && !sink.send(ev).await
                    {
                        break;
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

                        let sse_ev = ChatSseEvent::AgentCallStart {
                            call_id: info.call_id.clone(),
                            agent_id: info.agent_id.clone(),
                            agent_name: info.agent_name.clone(),
                            task,
                            conversation_id: conv_id_for_bg.clone(),
                        };
                        if let Ok(ev) = sse_ev.to_sse_event()
                            && !sink.send(ev).await
                        {
                            break;
                        }
                    }
                    sub_agent_registry.insert(id.clone(), infos);

                    if let Some(sse_ev) = map_looper_event(
                        LooperEvent::ToolCallStart {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
                        &conv_id_for_bg,
                    ) && let Ok(ev) = sse_ev.to_sse_event()
                        && !sink.send(ev).await
                    {
                        break;
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
                            let sse_ev = ChatSseEvent::AgentCallEnd {
                                call_id: info.call_id,
                                agent_id: info.agent_id,
                                agent_name: info.agent_name,
                                result: sub_result,
                                conversation_id: conv_id_for_bg.clone(),
                            };
                            if let Ok(ev) = sse_ev.to_sse_event()
                                && !sink.send(ev).await
                            {
                                break;
                            }
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
                    if let Ok(ev) = sse_ev.to_sse_event()
                        && !sink.send(ev).await
                    {
                        break;
                    }
                }

                Some(event @ LooperEvent::TurnComplete { .. }) => {
                    // batch 轮文本只来自可丢弃的 `ModelOutput(Batch)`：慢客户端
                    // 把通道塞满时事件会被丢掉。TurnComplete 必达，用它兜底，
                    // 否则成功轮会写不出 assistant 行。
                    if let LooperEvent::TurnComplete { outcome, .. } = &event {
                        backfill_assistant_text(
                            &mut assistant_text,
                            saw_text_delta,
                            outcome.text(),
                        );
                    }
                    if !assistant_text.is_empty() {
                        // 与 user 行同理：落库失败不影响这一轮，但必须留痕 —— 唤醒
                        // 轮里「台账记 delivered、消息却不在表里」是没有任何 UI 症状
                        // 的丢消息。
                        if let Err(e) = messages::insert(
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
                                "assistant message not persisted"
                            );
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

                    if let Some(sse_ev) = map_looper_event(event, &conv_id_for_bg)
                        && let Ok(ev) = sse_ev.to_sse_event()
                        && !sink.send(ev).await
                    {
                        break;
                    }
                }

                // ── 截断重试 ─────────────────────────────────────────────
                // 就地剥离被作废那次的正文 —— 它不属于最终答案，不该进
                // `messages` 表。前端侧不删自己已收到的增量，只把这条事件当
                // 通知看：落库内容对齐最终答案、SSE 内容对齐用户一路看到的
                // 东西，两者在这里分道。
                Some(event @ LooperEvent::TruncationRetry { .. }) => {
                    if let LooperEvent::TruncationRetry { discarded_text, .. } = &event {
                        strip_suffix(&mut assistant_text, discarded_text);
                    }
                    if let Some(sse_ev) = map_looper_event(event, &conv_id_for_bg)
                        && let Ok(ev) = sse_ev.to_sse_event()
                        && !sink.send(ev).await
                    {
                        break;
                    }
                }

                Some(LooperEvent::Shutdown { total_usage, .. }) => {
                    let done_ev = ChatSseEvent::Done {
                        usage: UsageData::from(total_usage),
                        conversation_id: conv_id_for_bg.clone(),
                    };
                    if let Ok(ev) = done_ev.to_sse_event() {
                        let _ = sink.send(ev).await;
                    }
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
                    if let Ok(ev) = sse_ev.to_sse_event()
                        && !sink.send(ev).await
                    {
                        break;
                    }
                }

                // 唤醒投递的一轮结束在 Idle：没人会关连接，不在这里退出就会一直
                // 挂在空闲态等一个不会到来的查询。`set()` 对同态迁移是 no-op，
                // 收到 Idle 即意味着这一轮真的停了。
                Some(LooperEvent::StateChange {
                    to: LooperState::Idle,
                    ..
                }) if sink.is_headless() => break,

                Some(event) => {
                    if let Some(sse_ev) = map_looper_event(event, &conv_id_for_bg)
                        && let Ok(ev) = sse_ev.to_sse_event()
                        && !sink.send(ev).await
                    {
                        break;
                    }
                }

                None => break,
                },

                // 到点仍未结束：放弃这一轮，让守卫随任务结束 drop。looper 侧的
                // 任务会随 handle drop 一起收掉。
                _ = &mut headless_deadline, if sink.is_headless() => {
                    tracing::warn!(
                        conversation_id = %conv_id_for_bg,
                        timeout_secs = HEADLESS_RUN_TIMEOUT.as_secs(),
                        "headless run exceeded the watchdog; giving up on this turn"
                    );
                    break;
                }

                // 暂停/恢复：来自 `POST .../pause` / `.../resume`。控制通道关闭
                // 说明本流已被同会话的新 stream 顶替，退出让位。
                cmd = control_rx.recv() => match cmd {
                    // 无客户端的运行没有人能发 Resume：真去 pause 会把 looper 与
                    // 守卫一起钉在暂停态，该会话的唤醒队列从此永久让路。
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
                    None => break,
                },
            }
        }

        drop(handle);
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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

    /// 看门狗必须明显长于台账的认领超时：反过来的话，一条还在正常跑的长回答会
    /// 先被回收成「认领方已死」，同一行被重投第二遍。
    #[test]
    fn the_headless_watchdog_outlives_the_claim_timeout() {
        assert!(
            HEADLESS_RUN_TIMEOUT > crate::wake::DELIVERING_TIMEOUT,
            "运行上限 {HEADLESS_RUN_TIMEOUT:?} 不得短于认领超时"
        );
        assert!(
            HEADLESS_RUN_TIMEOUT >= Duration::from_secs(300),
            "上限太短会把正常的长回答掐断"
        );
    }

    #[tokio::test]
    async fn headless_sink_never_reports_a_broken_client() {
        let sink = RunSink::Headless;
        assert!(sink.is_headless());
        assert!(
            sink.send(SseEvent::default().data("x")).await,
            "没有客户端就没有断连，send 恒成功"
        );
    }

    #[tokio::test]
    async fn headless_sink_has_no_disconnect_signal() {
        let sink = RunSink::Headless;
        let waited = tokio::time::timeout(Duration::from_millis(20), sink.closed()).await;
        assert!(waited.is_err(), "Headless 的 closed() 不得就绪");
    }
}
