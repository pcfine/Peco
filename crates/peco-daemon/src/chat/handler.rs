// ============================================================================
// Chat Handlers — Agent 作用域下的对话管理 + SSE 流式对话
// ============================================================================
//
// 端点路径：/api/chat/{agentId}/conversations
//
// 会话归属由请求的空间选择器（`LocalUser.ws_id`）决定，落库写进
// `conversations.workspace_id`。`{agentId}` 不参与归属判定：同团队两个空间
// 聊同一个 Agent 是两份互不可见的历史。

use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::stream::Stream;
use model_provider::{Content, ContentPart};
use peco_core::session::SessionPersister;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api_error::ApiError;
use crate::auth::LocalUser;
use crate::db::{conversations, messages};
use crate::session_dto::{
    ItemView, TurnData, group_input_items, turn_interrupted_reason, turns_to_dto,
};
use crate::session_store::SqliteSessionPersister;
use crate::state::AppState;

use super::conversation::auto_archive_oldest_if_needed;
use super::feed::{self, LiveFrames};
use super::run::{RunAudience, RunDeps, RunError, spawn_run};
use super::sse::UsageData;

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

/// 在途注入请求体。
///
/// 与 `/stream` 的查询参数不同：注入是 POST，图片引用走 JSON 数组而不是逗号分隔串。
#[derive(Debug, Deserialize)]
pub struct SteerRequest {
    pub message: String,
    #[serde(default)]
    pub image_ids: Vec<String>,
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

/// 用户文本 + 图片引用 → 交给 looper 的 [`Content`]。
///
/// `/stream` 与 `/steer` 必须是同一份编码：无图走 `Content::Text`、有图走
/// `Content::Parts`（文本在前），否则同一段输入经两条入口进入会话会得到两种形状。
async fn build_message_content(
    state: &AppState,
    ws_id: &str,
    message: &str,
    image_ids: &[String],
) -> Result<Content, ApiError> {
    let image_parts = super::images::resolve_image_parts(state, ws_id, image_ids).await?;
    if image_parts.is_empty() {
        return Ok(Content::Text(message.to_string()));
    }
    let mut parts = Vec::with_capacity(1 + image_parts.len());
    parts.push(ContentPart::Text {
        text: message.to_string(),
    });
    parts.extend(image_parts);
    Ok(Content::Parts(parts))
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

// ── SSE Stream Handler ───────────────────────────────────────────────────────

/// 装配失败原样落到 HTTP：空间未打开仍是 409，agent 装载失败仍是 500。
impl From<RunError> for ApiError {
    fn from(err: RunError) -> Self {
        match err {
            RunError::Space(e) => ApiError::from(e),
            RunError::Agent(message) => ApiError::Internal(message),
            // 配置不可用不是「稍后重试就好」（不是 503），也不是请求写错了（不是
            // 4xx）：状态码留在服务端侧，前端按 `code` 分流。文案是中文可操作句
            // + core 原文，只拼 provider 名与变量名，不拼任何配置值。
            RunError::Provider(failure, detail) => ApiError::coded(
                StatusCode::INTERNAL_SERVER_ERROR,
                failure.code(),
                failure.user_message(&detail),
            ),
            // 这一轮还在跑（与有没有人连着无关）：不再静默顶替 —— 顶替会让另一个
            // 标签页正在看的一轮被无声截断，且两条 run 争同一会话的 session 快照。
            // 前端按 `code` 分支提示「先停止再发」，不解析文案。
            RunError::Busy => ApiError::coded(
                StatusCode::CONFLICT,
                "conversation_busy",
                "the conversation is already running",
            ),
        }
    }
}

/// 把一订阅端包成 SSE 响应。keep-alive 保留：一轮可以长时间没有帧（模型在读长
/// 上下文），连接不能因此被中间设备判死。
fn sse_response(frames: LiveFrames) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(feed::frame_stream(frames)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// `GET /api/chat/{agentId}/conversations/{id}/stream?message=`
///
/// 发一轮并跟随它的帧流。首帧是 `turn_snapshot`，末帧是 `done`。
///
/// **连接不是这一轮的所有权**：断连只丢掉这条出口，轮照旧跑完并落库；客户端回来
/// 用 `GET .../stream/live` 接上，不必重发。有在途轮时返回 409 `conversation_busy`。
pub async fn stream_chat(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
    Query(params): Query<StreamQuery>,
) -> Result<Response, ApiError> {
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
    let message_content = build_message_content(&state, &ws_id, &message, &image_ids).await?;

    // ── 1. 加载对话（本空间内的会话，别的一律 404）──────────────────────
    let conv = conversations::find_by_id_and_workspace(&state.pool, &conv_id, &ws_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("conversation '{conv_id}' not found")))?;

    // ── 2. 跑起来并跟上它的帧流 ────────────────────────────────────────
    // 注册交给 `spawn_run`，位置在空间/agent/会话校验之后：一个注定 409/500 的
    // 请求不该把该会话在途的 run 挤掉。
    let frames = spawn_run(
        RunDeps {
            pool: &state.pool,
            env: &state.env,
            spaces: &state.spaces,
            chat_runs: &state.chat_runs,
        },
        &ws_id,
        &conv,
        message,
        message_content,
        RunAudience::Live,
    )
    .await?;

    Ok(sse_response(frames).into_response())
}

/// `GET /api/chat/{agentId}/conversations/{id}/stream/live`
///
/// 只订阅、不新开一轮：客户端回来接上在途轮。
///
/// `204 No Content` 表示该会话没有未结束的在途轮 —— 此时**历史即权威**（落库严格
/// 早于终态帧），客户端直接用 `GET .../messages` 的结果替换本地气泡即可。返回 200
/// 时首帧是 `turn_snapshot`，其后与 `/stream` 同构。
pub async fn subscribe_chat(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    require_owned_conversation(&state, &ws_id, &conv_id).await?;

    match state.chat_runs.attach(&conv_id) {
        Some(frames) => Ok(sse_response(frames).into_response()),
        None => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}

/// `POST /api/chat/{agentId}/conversations/{id}/cancel`
///
/// 取消该会话在途的一轮。`200` 只表示**命令已投递**：收尾发生在 looper 的步进
/// 边界（流式=单 chunk、工具=≤200ms 轮询、batch=整段生成），可能晚于响应。实际
/// 收尾经帧流下发（`error{cancelled:true}` 之后跟 `done`），这一轮已产出的正文
/// 照旧落库。
pub async fn cancel_chat(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
) -> Result<Json<SuccessResponse>, ApiError> {
    require_owned_conversation(&state, &ws_id, &conv_id).await?;
    if state.chat_runs.cancel(&conv_id) {
        Ok(Json(SuccessResponse { success: true }))
    } else {
        Err(ApiError::coded(
            StatusCode::CONFLICT,
            "no_running_task",
            format!("no running task for conversation '{conv_id}'"),
        ))
    }
}

/// `POST /api/chat/{agentId}/conversations/{id}/steer`
///
/// 把一段用户输入注入该会话**在途**的那一轮：内容进 pending 队列，下一次模型请求
/// 前才并入（`200` 只表示已投递，不是已送达）。无在途轮时返回 409
/// `no_running_task`，前端据此回退成普通发送。
///
/// user 行由 run 任务写（单写者），见 `chat/run.rs` 的 `ChatControl::Query` 分支：
/// 那条路径上原消息行必已落库，注入行因此排在它之后；在这里写就与 run 任务竞争，
/// 同一秒内按 `rowid` 排会让历史顺序反过来。
pub async fn steer_chat(
    LocalUser { ws_id }: LocalUser,
    State(state): State<AppState>,
    Path((_agent_name, conv_id)): Path<(String, String)>,
    Json(req): Json<SteerRequest>,
) -> Result<Json<SuccessResponse>, ApiError> {
    let message = req.message.trim().to_string();
    if message.is_empty() {
        return Err(ApiError::BadRequest("message is required".into()));
    }

    require_owned_conversation(&state, &ws_id, &conv_id).await?;

    let content = build_message_content(&state, &ws_id, &message, &req.image_ids).await?;

    // 投递失败说明这一轮已经结束：调用方回退成 `/stream` 发一条新的。
    if !state.chat_runs.steer(&conv_id, content) {
        return Err(ApiError::coded(
            StatusCode::CONFLICT,
            "no_running_task",
            format!("no running task for conversation '{conv_id}'"),
        ));
    }

    // 投递已经发生：touch 失败只是活跃时间没刷新，不能因此回 5xx —— 前端会把
    // 「已投递」当失败退回重发，同一条输入于是被注入两次。
    if let Err(e) = conversations::touch(&state.pool, &conv_id).await {
        tracing::warn!(
            conversation_id = %conv_id,
            error = %e,
            "steer delivered but conversation touch failed"
        );
    }

    Ok(Json(SuccessResponse { success: true }))
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
    use super::*;
    use crate::provider::ProviderFailure;

    /// 装配期的 provider 失败落到 HTTP：稳定 code + 中文可操作句 + core 原文。
    ///
    /// 状态码留在服务端侧（500）是设计选择：这不是请求写错了（不是 4xx），也不是
    /// 「稍后重试就好」（不是 503），前端按 `code` 分流。
    #[test]
    fn provider_failure_maps_to_a_coded_error() {
        let error: ApiError = RunError::Provider(
            ProviderFailure::KeyUnresolved {
                provider: "gw".to_string(),
                var: "PECO_TEST_MISSING".to_string(),
            },
            "failed to load agent '@master': env var missing".to_string(),
        )
        .into();

        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error.code(), "provider_key_unresolved");
        let message = error.message();
        assert!(
            message.starts_with("provider 'gw' 的密钥引用了环境变量 PECO_TEST_MISSING"),
            "中文可操作句要在最前面：{message}"
        );
        assert!(
            message.contains('\n'),
            "中文句与 core 原文分两行：{message}"
        );
        assert!(
            message.contains("failed to load agent '@master'"),
            "core 原文必须原样保留，便于排查：{message}"
        );

        let unconfigured: ApiError =
            RunError::Provider(ProviderFailure::NoProviders, "detail".into()).into();
        assert_eq!(unconfigured.code(), "provider_unconfigured");
        assert_eq!(unconfigured.status(), StatusCode::INTERNAL_SERVER_ERROR);

        // agent 装载失败但配置健康：仍是通用 internal，code 不变。
        let plain: ApiError = RunError::Agent("boom".into()).into();
        assert_eq!(plain.code(), "internal");
        assert_eq!(plain.message(), "boom");
    }
}
