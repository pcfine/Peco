//! 空间单会话的生命周期：`/conversation/ensure` 惰性创建，`/conversation/clear` 清空消息。
//!
//! 前端点击空间时会 ensure 一次；而唤醒投递的目标正是这条会话，所以它也会在
//! 用户还没进聊天页时被 `wake::deliver` 幂等地建出来 —— 消息必须落在他打开空间
//! 后看到的那段历史里，另开会话等于把唤醒藏进一个没人打开的会话。

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use peco_core::session::SessionPersister;
use serde::{Deserialize, Serialize};

use crate::chat::handler::SuccessResponse;
use crate::db::{conversations, messages, workspaces};
use crate::error::DaemonError;
use crate::session_store::SqliteSessionPersister;
use crate::state::AppState;

/// 空间页唯一对话的默认 agent；请求体缺省时用它。
const DEFAULT_AGENT_NAME: &str = "@master";

/// 请求体整体可选：前端在「已经知道要聊谁」之前不必构造 JSON。
#[derive(Debug, Default, Deserialize)]
pub struct EnsureRequest {
    pub agent_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EnsureResponse {
    pub conversation_id: String,
    /// 本次是否真的插了新行：前端据此决定要不要去拉历史消息。
    pub created: bool,
}

pub async fn ensure(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
    body: Option<Json<EnsureRequest>>,
) -> Result<Json<EnsureResponse>, DaemonError> {
    if workspaces::by_id(&state.pool, &ws_id).await?.is_none() {
        return Err(DaemonError::SpaceNotFound { ws_id });
    }
    let agent_name = agent_name_of(body);
    let (conversation_id, created) =
        conversations::ensure_space_session(&state.pool, &ws_id, &agent_name).await?;
    tracing::debug!(
        ws_id = %ws_id,
        agent_name = %agent_name,
        conversation_id = %conversation_id,
        created,
        "space session ensured"
    );
    Ok(Json(EnsureResponse {
        conversation_id,
        created,
    }))
}

/// 请求体缺省即 `@master`：两条端点共用同一条约定，各写一遍迟早会漂。
fn agent_name_of(body: Option<Json<EnsureRequest>>) -> String {
    body.and_then(|Json(request)| request.agent_name)
        .unwrap_or_else(|| DEFAULT_AGENT_NAME.to_string())
}

/// `POST /api/spaces/{ws_id}/conversation/clear`
///
/// 清空这条会话的全部消息，**不删 `conversations` 行**：`artifacts.conversation_id`
/// 是 `ON DELETE CASCADE`，删行会连带抹掉该会话的产物登记（磁盘文件留着，
/// 「成果/文件」区却少行）；空间维度的产物清单也是靠这行反查归属的。
///
/// 有一轮在跑时拒绝（409）：那时清空的定义是「清到一半」，等这一轮结束再清才有
/// 唯一的答案。
pub async fn clear(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
    body: Option<Json<EnsureRequest>>,
) -> Result<Json<SuccessResponse>, DaemonError> {
    if workspaces::by_id(&state.pool, &ws_id).await?.is_none() {
        return Err(DaemonError::SpaceNotFound { ws_id });
    }
    let agent_name = agent_name_of(body);

    // 还没聊过的空间没有会话可清：清空是幂等动作，让前端去分辨 404 只会
    // 把「本来就干净」显示成一个错误。
    let Some(conversation_id) =
        conversations::find_space_session(&state.pool, &ws_id, &agent_name).await?
    else {
        return Ok(Json(SuccessResponse { success: true }));
    };

    let persister = SqliteSessionPersister::new(state.pool.clone());

    // 运行态判定：检查点行只说明上一轮没跑完，进程崩过一次后它就是残渣。
    // 拿它当门槛，「上一轮没跑完」这件事会永久拦住清空。
    if state.chat_runs.is_active(&conversation_id) {
        return Err(DaemonError::ConversationInFlight { ws_id });
    }

    messages::delete_by_conversation(&state.pool, &conversation_id).await?;

    // 会话 id 不变，快照与检查点里的上下文却还是旧的：留着它们，下一轮
    // 会把已删的消息水化回模型上下文，「消息清空」就只清了一半。检查点留到
    // 这里已不是为了防半清 —— 那一层由上面的运行态门槛负责 —— 而是清残渣。
    if let Err(e) = persister.delete(&conversation_id).await {
        tracing::warn!(
            conversation_id = %conversation_id,
            error = %e,
            "session snapshot not deleted; the next turn may rehydrate from it"
        );
    }
    if let Err(e) = persister.delete_inflight(&conversation_id).await {
        tracing::warn!(
            conversation_id = %conversation_id,
            error = %e,
            "inflight checkpoint not deleted; the next turn may rehydrate from it"
        );
    }

    tracing::info!(
        ws_id = %ws_id,
        agent_name = %agent_name,
        conversation_id = %conversation_id,
        "space session cleared"
    );
    Ok(Json(SuccessResponse { success: true }))
}
