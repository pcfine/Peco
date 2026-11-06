// ============================================================================
// Chat Handlers — Agent 作用域下的对话管理 + SSE 流式对话
// ============================================================================
//
// 端点路径：/api/chat/{agentId}/conversations
//
// 会话归属由请求的空间选择器（`LocalUser.ws_id`）决定，落库写进
// `conversations.workspace_id`。`{agentId}` 不参与归属判定：同团队两个空间
// 聊同一个 Agent 是两份互不可见的历史。

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{KeepAlive, Sse};
use futures::stream::Stream;
use model_provider::{Content, ContentPart, text_of_blocks};
use peco_core::agent::{AgentLooper, LooperEvent, ResponseOrigin};
use peco_core::session::Session;
use peco_core::session::SessionPersister;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::LOCAL_USER_ID;
use crate::api_error::ApiError;
use crate::auth::LocalUser;
use crate::db::{agents, conversations, messages};
use crate::session_dto::{
    ItemView, TurnData, group_input_items, turn_interrupted_reason, turns_to_dto,
};
use crate::session_store::{SqliteSessionPersister, hydrate_inflight_turn};
use crate::state::AppState;

use super::active::ChatControl;
use super::conversation::auto_archive_oldest_if_needed;
use super::sse::{
    ChatSseEvent, SubAgentInfo, UsageData, extract_sub_agent_result, map_looper_event,
    parse_sub_agent_infos, strip_suffix,
};

// ── Request / Response 类型 ─────────────────────────────────────────────────

/// 创建对话请求。
#[derive(Debug, Deserialize)]
pub struct CreateConversationRequest {
    #[serde(default)]
    pub title: Option<String>,
}

/// 更新对话请求。
#[derive(Debug, Deserialize)]
pub struct UpdateConversationRequest {
    pub title: Option<String>,
    /// 设为 true 以归档对话
    pub archive: Option<bool>,
    /// 设为 true 以恢复已归档对话
    pub unarchive: Option<bool>,
}

/// 对话列表项响应。
#[derive(Debug, Serialize)]
pub struct ConversationResponse {
    pub id: String,
    pub title: String,
    pub agent_name: String,
    /// 会话归属的空间。前端据此断言列表没有跨空间串数据。
    pub workspace_id: String,
    pub archived: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 对话列表查询参数。
#[derive(Debug, Deserialize)]
pub struct ConversationListQuery {
    /// 归档状态过滤："active"（默认）, "archived", "all"
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "active".to_string()
}

/// 消息列表项响应。
#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub id: String,
    pub role: String,
    pub content: String,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub created_at: String,
}

/// SSE 流式查询参数。
#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    pub message: String,
    /// 逗号分隔的图片引用 id（上传端点返回）；发送时解析为图片部件。
    #[serde(default)]
    pub image_ids: Option<String>,
}

/// 消息列表查询参数。
#[derive(Debug, Deserialize)]
pub struct MessagesQuery {
    #[serde(default = "default_offset")]
    pub offset: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_offset() -> i64 {
    0
}
fn default_limit() -> i64 {
    50
}

/// 简单成功响应。
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
}

// ── 辅助函数 ─────────────────────────────────────────────────────────────────

pub(crate) fn row_to_response(row: &conversations::ConversationRow) -> ConversationResponse {
    ConversationResponse {
        id: row.id.clone(),
        title: row.title.clone(),
        agent_name: row.agent_name.clone(),
        workspace_id: row.workspace_id.clone(),
        archived: row.archived_at.is_some(),
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    }
}

// ── Conversation Handlers ────────────────────────────────────────────────────

/// `GET /api/chat/{agentId}/conversations`
///
/// 返回本空间某 Agent 下的对话列表。支持 `?status=active|archived|all`。
pub async fn list_conversations(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path(agent_name): Path<String>,
    Query(params): Query<ConversationListQuery>,
) -> Result<Json<Vec<ConversationResponse>>, ApiError> {
    let include_archived = matches!(params.status.as_str(), "all" | "archived");

    let rows = conversations::list_by_workspace_and_agent(
        &state.pool,
        &ws_id,
        &agent_name,
        include_archived,
    )
    .await?;

    let mut responses: Vec<ConversationResponse> = rows.iter().map(row_to_response).collect();

    // 如果只查询 archived，过滤掉活跃的
    if params.status == "archived" {
        responses.retain(|r| r.archived);
    } else if params.status == "active" {
        responses.retain(|r| !r.archived);
    }

    Ok(Json(responses))
}

/// `POST /api/chat/{agentId}/conversations`
///
/// 创建新对话。自动检查上限，超限时自动归档最旧对话。
pub async fn create_conversation(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path(agent_name): Path<String>,
    Json(req): Json<CreateConversationRequest>,
) -> Result<(StatusCode, Json<ConversationResponse>), ApiError> {
    let title = req
        .title
        .unwrap_or_else(|| "新对话".to_string())
        .trim()
        .to_string();

    // ── 上限检查：超限自动归档最旧对话 ────────────────────────────────
    let archived = auto_archive_oldest_if_needed(&state.pool, &ws_id, &agent_name).await?;
    if archived > 0 {
        tracing::info!(
            ws_id = %ws_id,
            agent_name = %agent_name,
            archived,
            "Auto-archived old conversations before creating new one"
        );
    }

    // ── 创建对话 ────────────────────────────────────────────────────────
    let conv_id = Uuid::new_v4().to_string();
    let params = conversations::CreateConversationParams {
        id: conv_id.clone(),
        workspace_id: ws_id.clone(),
        agent_id: None,
        agent_name: agent_name.clone(),
        title: title.clone(),
    };

    conversations::insert(&state.pool, &params).await?;

    let row = conversations::find_by_id(&state.pool, &conv_id)
        .await?
        .ok_or_else(|| ApiError::Internal("conversation created but not found".into()))?;

    let response = row_to_response(&row);

    tracing::info!(
        ws_id = %ws_id,
        conversation_id = %conv_id,
        agent_name = %agent_name,
        "Conversation created"
    );

    Ok((StatusCode::CREATED, Json(response)))
}

/// `PATCH /api/chat/{agentId}/conversations/{id}`
///
/// 更新对话：重命名、归档、恢复。
pub async fn update_conversation(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
    Json(req): Json<UpdateConversationRequest>,
) -> Result<Json<ConversationResponse>, ApiError> {
    let _conv = conversations::find_by_id_and_workspace(&state.pool, &conv_id, &ws_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    if let Some(ref title) = req.title {
        conversations::update_title(&state.pool, &conv_id, title.trim()).await?;
    }

    if req.archive.unwrap_or(false) {
        conversations::archive(&state.pool, &conv_id).await?;
    }

    if req.unarchive.unwrap_or(false) {
        conversations::unarchive(&state.pool, &conv_id).await?;
    }

    let updated = conversations::find_by_id(&state.pool, &conv_id)
        .await?
        .ok_or_else(|| ApiError::Internal("conversation updated but not found".into()))?;

    Ok(Json(row_to_response(&updated)))
}

/// `DELETE /api/chat/{agentId}/conversations/{id}`
///
/// 永久删除对话及关联消息和会话快照。
pub async fn delete_conversation(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
) -> Result<Json<SuccessResponse>, ApiError> {
    // 只认 agent 会话：peco 私有会话的产物行靠外键挂在它上面，删会话会连带删产物。
    let conv =
        conversations::find_agent_conversation_by_id_and_workspace(&state.pool, &conv_id, &ws_id)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    // 删除关联消息
    messages::delete_by_conversation(&state.pool, &conv_id).await?;

    // 删除会话快照与在途轮检查点（后者不删会在会话 ID 复用时被水化错位）
    let persister = SqliteSessionPersister::new(state.pool.clone());
    let _ = persister.delete(&conv_id).await;
    let _ = persister.delete_inflight(&conv_id).await;

    // 删除对话记录
    conversations::delete(&state.pool, &conv_id).await?;

    tracing::info!(
        ws_id = %conv.workspace_id,
        conversation_id = %conv_id,
        "Conversation permanently deleted"
    );

    Ok(Json(SuccessResponse { success: true }))
}

/// `GET /api/chat/{agentId}/conversations/{id}/messages`
///
/// 获取对话消息列表，支持分页。
pub async fn get_messages(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
    Query(params): Query<MessagesQuery>,
) -> Result<Json<Vec<MessageResponse>>, ApiError> {
    conversations::find_by_id_and_workspace(&state.pool, &conv_id, &ws_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    let rows =
        messages::list_by_conversation(&state.pool, &conv_id, params.offset, params.limit).await?;

    let responses: Vec<MessageResponse> = rows
        .iter()
        .map(|r| MessageResponse {
            id: r.id.clone(),
            role: r.role.clone(),
            content: r.content.clone(),
            agent_id: r.agent_id.clone(),
            agent_name: r.agent_name.clone(),
            created_at: r.created_at.clone(),
        })
        .collect();

    Ok(Json(responses))
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

// ── SSE Stream Handler ───────────────────────────────────────────────────────

/// `GET /api/chat/{agentId}/conversations/{id}/stream?message=`
///
/// SSE 流式对话。
pub async fn stream_chat(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
    Query(params): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<axum::response::sse::Event, Infallible>>>, ApiError> {
    let message = params.message.trim().to_string();
    if message.is_empty() {
        return Err(ApiError::BadRequest("message is required".into()));
    }

    // ── 0. 解析图片引用（跨空间 / 非法 id 一律 4xx）────────────────────
    let image_ids: Vec<String> = params
        .image_ids
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let image_parts = super::images::resolve_image_parts(&state, &ws_id, &image_ids).await?;
    let message_content = if image_parts.is_empty() {
        Content::Text(message.clone())
    } else {
        let mut parts = Vec::with_capacity(1 + image_parts.len());
        parts.push(ContentPart::Text {
            text: message.clone(),
        });
        parts.extend(image_parts);
        Content::Parts(parts)
    };

    // ── 1. 加载对话（本空间内的会话，别的一律 404）──────────────────────
    let conv = conversations::find_by_id_and_workspace(&state.pool, &conv_id, &ws_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    // ── 2. 加载空间实例与 Agent（从 agent_name 字段，非 agent_id）───────
    // 经 `SpaceRegistry` 取实例：空间未打开时是 409，路径与四个资源模块一致。
    let ws = state.spaces.workspace(&ws_id).await?;
    let agent = ws
        .agent_manager()
        .load_cached(&conv.agent_name)
        .map_err(|e| {
            ApiError::Internal(format!("failed to load agent '{}': {e}", conv.agent_name))
        })?;
    let team_id = state.spaces.team_id(&ws_id).await?;

    // ── 3. 加载或创建 Session ────────────────────────────────────────────
    let persister = SqliteSessionPersister::new(state.pool.clone());
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

    // ── 3b. 崩溃恢复：冻结上次进程留下的在途轮 ────────────────────────────
    hydrate_inflight_turn(&persister, &mut session).await;

    // 「首轮」据水化**之后**的历史判定 —— 水化刚冻结的轮次同样是历史，
    // 否则新一轮消息会去覆盖一个已有内容的会话标题。
    let is_first_turn = session.committed_turns().is_empty();

    // ── 4. 创建 SSE channel ──────────────────────────────────────────────
    let (sse_tx, sse_rx) = mpsc::channel::<Result<axum::response::sse::Event, Infallible>>(256);

    // 观察 hook 与动态上下文在 spawn 之前装配：两者都要读 `ws` 的 work_dir 与
    // `team_id`，而这两者随后会被 move 进后台任务。
    let config = state.looper_config(&ws_id, &team_id, ws.work_dir().root().to_path_buf());

    let db_for_bg = state.pool.clone();
    let conv_id_for_bg = conv_id.clone();
    let message_for_bg = message.clone();
    let message_content_for_bg = message_content;
    let team_id_for_bg = team_id;
    let ws_for_bg = ws.clone();

    // ── 5. 后台执行 AgentLooper ──────────────────────────────────────────
    // 控制通道先于任务注册：否则「任务已在跑、注册表尚无条目」的窗口里
    // 暂停请求会被误判为无在途运行。
    let (mut control_rx, run_guard) = state.chat_runs.register(&conv_id);
    tokio::spawn(async move {
        // 持有至任务结束：守卫 drop 才移除注册表条目（见 `chat::active`）。
        let _run_guard = run_guard;

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
                let _ = sse_tx.send(Ok(ev)).await;
            }
            return;
        }

        let _ = conversations::touch(&db_for_bg, &conv_id_for_bg).await;

        let _ = messages::insert(
            &db_for_bg,
            &Uuid::new_v4().to_string(),
            &conv_id_for_bg,
            "user",
            &message_for_bg,
            None,
            None,
        )
        .await;

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

        loop {
            // 三路复用：looper 事件 / 暂停恢复控制 / 客户端断连。
            //
            // 断连一路不可省 —— chat 链路没有 peco runner 那样的桥接任务，
            // 少了它，停靠（尤其暂停）中的 looper 会随无人观看的连接一直挂着。
            // axum 的 SSE keep-alive 写入失败后响应体被 drop，`closed()` 随即就绪。
            tokio::select! {
                _ = sse_tx.closed() => break,

                event = handle.recv_event() => match event {
                Some(LooperEvent::TextDelta { delta }) => {
                    saw_text_delta = true;
                    assistant_text.push_str(&delta);
                    if let Some(sse_ev) =
                        map_looper_event(LooperEvent::TextDelta { delta }, &conv_id_for_bg)
                        && let Ok(ev) = sse_ev.to_sse_event()
                        && sse_tx.send(Ok(ev)).await.is_err()
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
                        && sse_tx.send(Ok(ev)).await.is_err()
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
                            && sse_tx.send(Ok(ev)).await.is_err()
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
                        && sse_tx.send(Ok(ev)).await.is_err()
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
                                && sse_tx.send(Ok(ev)).await.is_err()
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
                        && sse_tx.send(Ok(ev)).await.is_err()
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
                        let _ = messages::insert(
                            &db_for_bg,
                            &Uuid::new_v4().to_string(),
                            &conv_id_for_bg,
                            "assistant",
                            &assistant_text,
                            None,
                            None,
                        )
                        .await;

                        // 仅首轮对话时根据用户第一条消息自动生成标题
                        if is_first_turn {
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
                        && sse_tx.send(Ok(ev)).await.is_err()
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
                        && sse_tx.send(Ok(ev)).await.is_err()
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
                        let _ = sse_tx.send(Ok(ev)).await;
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
                        && sse_tx.send(Ok(ev)).await.is_err()
                    {
                        break;
                    }
                }

                Some(event) => {
                    if let Some(sse_ev) = map_looper_event(event, &conv_id_for_bg)
                        && let Ok(ev) = sse_ev.to_sse_event()
                        && sse_tx.send(Ok(ev)).await.is_err()
                    {
                        break;
                    }
                }

                None => break,
                },

                // 暂停/恢复：来自 `POST .../pause` / `.../resume`。控制通道关闭
                // 说明本流已被同会话的新 stream 顶替，退出让位。
                cmd = control_rx.recv() => match cmd {
                    Some(ChatControl::Pause) => {
                        if let Err(e) = handle.pause().await {
                            tracing::warn!(
                                conversation_id = %conv_id_for_bg,
                                error = %e,
                                "Failed to deliver pause"
                            );
                        }
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

    let stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx);
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

// ── 暂停 / 恢复 ──────────────────────────────────────────────────────────────

/// 校验会话归属（不存在 / 不属于本空间一律 404，不泄露别的空间的会话）。
async fn require_owned_conversation(
    state: &AppState,
    ws_id: &str,
    conv_id: &str,
) -> Result<(), ApiError> {
    conversations::find_by_id_and_workspace(&state.pool, conv_id, ws_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;
    Ok(())
}

/// `POST /api/chat/{agentId}/conversations/{id}/pause`
///
/// 请求暂停该会话在途的 ReAct 轮。**暂停在边界检查点生效**（请求发出前 / 工具
/// 执行前：流式在途时等本次模型调用收尾、工具执行中等本批跑完），因此本接口只表示「已收到请求」；
/// 实际状态经 SSE `looper_state` 事件下发。无在途运行时返回 409。
pub async fn pause_chat(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
) -> Result<Json<SuccessResponse>, ApiError> {
    require_owned_conversation(&state, &ws_id, &conv_id).await?;
    if state.chat_runs.pause(&conv_id) {
        Ok(Json(SuccessResponse { success: true }))
    } else {
        Err(ApiError::Conflict(format!(
            "no running task for conversation '{conv_id}'"
        )))
    }
}

/// `POST /api/chat/{agentId}/conversations/{id}/resume`
///
/// 请求恢复该会话被暂停的 ReAct 轮；幂等（未暂停时由 looper 侧忽略）。
pub async fn resume_chat(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
) -> Result<Json<SuccessResponse>, ApiError> {
    require_owned_conversation(&state, &ws_id, &conv_id).await?;
    if state.chat_runs.resume(&conv_id) {
        Ok(Json(SuccessResponse { success: true }))
    } else {
        Err(ApiError::Conflict(format!(
            "no running task for conversation '{conv_id}'"
        )))
    }
}

// ── Session Snapshot Handler ────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SessionSnapshotResponse {
    pub conversation_id: String,
    pub turns: Vec<TurnData>,
    pub total_usage: UsageData,
}

/// `GET /api/chat/{agentId}/conversations/{id}/session`
pub async fn get_session_snapshot(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
) -> Result<Json<SessionSnapshotResponse>, ApiError> {
    let _conv = conversations::find_by_id_and_workspace(&state.pool, &conv_id, &ws_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    let persister = SqliteSessionPersister::new(state.pool.clone());
    let snapshot_opt = persister
        .load(&conv_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to load session: {e}")))?;

    let (turns, usage) = match snapshot_opt {
        Some((snap, _meta)) => {
            // 全量窗口（chat 链路不分页，保持行为不变）。
            let turns = turns_to_dto(&snap.committed_turns, 0..snap.committed_turns.len());

            let usage = UsageData {
                input_tokens: snap.total_usage.input_tokens,
                output_tokens: snap.total_usage.output_tokens,
            };
            (turns, usage)
        }
        None => (
            Vec::new(),
            UsageData {
                input_tokens: 0,
                output_tokens: 0,
            },
        ),
    };

    Ok(Json(SessionSnapshotResponse {
        conversation_id: conv_id,
        turns,
        total_usage: usage,
    }))
}

// ── Export Handler ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(default = "default_export_format")]
    pub format: String,
}

fn default_export_format() -> String {
    "json".to_string()
}

/// `GET /api/chat/{agentId}/conversations/{id}/export?format=json|markdown`
pub async fn export_conversation(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
    Query(params): Query<ExportQuery>,
) -> Result<axum::response::Response, ApiError> {
    let _conv = conversations::find_by_id_and_workspace(&state.pool, &conv_id, &ws_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    let persister = SqliteSessionPersister::new(state.pool.clone());
    let snapshot_opt = persister
        .load(&conv_id)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to load session: {e}")))?;

    match params.format.as_str() {
        "markdown" => {
            let md = snapshot_to_markdown(&snapshot_opt, &conv_id);
            Ok(axum::response::Response::builder()
                .header("Content-Type", "text/markdown; charset=utf-8")
                .header(
                    "Content-Disposition",
                    format!("attachment; filename=\"conversation-{conv_id}.md\""),
                )
                .body(axum::body::Body::from(md))
                .unwrap())
        }
        _ => {
            let json = serde_json::to_string_pretty(&snapshot_opt).unwrap_or_default();
            Ok(axum::response::Response::builder()
                .header("Content-Type", "application/json; charset=utf-8")
                .header(
                    "Content-Disposition",
                    format!("attachment; filename=\"conversation-{conv_id}.json\""),
                )
                .body(axum::body::Body::from(json))
                .unwrap())
        }
    }
}

pub(crate) fn snapshot_to_markdown(
    snapshot_opt: &Option<(
        peco_core::session::SessionSnapshot,
        peco_core::session::SessionMeta,
    )>,
    conv_id: &str,
) -> String {
    let mut md = format!("# 对话记录 — {conv_id}\n\n");
    md.push_str(&conversation_turns_markdown(snapshot_opt));
    md
}

/// 轮次正文渲染 — 导出 Markdown 的唯一渲染源。
fn conversation_turns_markdown(
    snapshot_opt: &Option<(
        peco_core::session::SessionSnapshot,
        peco_core::session::SessionMeta,
    )>,
) -> String {
    let mut md = String::new();
    if let Some((snap, _meta)) = snapshot_opt {
        for turn in &snap.committed_turns {
            // 导出正文与 UI 保持一致：中断轮先给一行横幅，再照常渲染其产物。
            if let Some(reason) = turn_interrupted_reason(turn) {
                md.push_str(&format!("\n> ⏹ 本轮被中断（{reason}）\n"));
            }
            for msg in group_input_items(&ItemView::from_turn(turn)) {
                match msg.role {
                    "user" => {
                        if let Some(content) = msg.content {
                            md.push_str(&format!("\n## 用户\n{content}\n"));
                        }
                        if !msg.images.is_empty() {
                            md.push_str(&format!("[图片 x{}]\n", msg.images.len()));
                        }
                    }
                    "assistant" => {
                        if let Some(reasoning) = msg.reasoning_content {
                            md.push_str(&format!("\n> 推理：{reasoning}\n"));
                        }
                        for tc in &msg.tool_calls {
                            md.push_str(&format!(
                                "\n> 🔧 {}: `{}`\n",
                                tc.function.name,
                                tc.function.arguments.chars().take(200).collect::<String>()
                            ));
                        }
                        if let Some(content) = msg.content {
                            md.push_str(&format!("\n## 助手\n{content}\n"));
                        }
                    }
                    "tool" => {
                        if let Some(content) = msg.content {
                            let preview: String = content.chars().take(300).collect();
                            md.push_str(&format!("\n> 工具输出：\n> ```\n> {preview}\n> ```\n"));
                        }
                    }
                    _ => {}
                }
            }
            md.push_str("\n---\n");
        }
    }
    md
}

#[cfg(test)]
mod tests {
    use super::backfill_assistant_text;

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
}
