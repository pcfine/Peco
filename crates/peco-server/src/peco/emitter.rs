// ============================================================================
// RunEmitter — runner 的唯一事件出口
// ============================================================================
//
// runner 驱动 looper，把每条 `LooperEvent` 交给 `RunEmitter::emit`，由它决定
// 该事件要不要先落库，再统一广播给零到多个桥接任务。

use std::sync::Arc;

use peco_core::agent::LooperEvent;
use tokio::sync::broadcast;
use tracing::warn;

use crate::chat::sse::{ChatSseEvent, map_looper_event};
use crate::workspace::WorkspaceManager;

use super::session::{SESSION_AGENT_NAME, SESSION_TITLE, private_session_id};

/// runner 广播给桥接任务的事件。
///
/// 构造权只属于 [`RunEmitter`]：外部拿到的值只能读、不能凭空造一条，故「跳过
/// 落库直接广播」在类型层面就不存在。
// 不派生 `Debug`：`ChatSseEvent::ToolResult` 带完整工具结果正文，一旦被 `{:?}`
// 就是内容泄漏。`Clone` 是 `broadcast::channel` 的硬要求，不能去。
#[derive(Clone)]
pub struct PecoRunEvent(PecoRunEventKind);

#[derive(Clone)]
enum PecoRunEventKind {
    /// 尚未落库、待桥接层纯映射的原始事件。
    Looper(LooperEvent),
    /// 已落库并构造好的 SSE 事件（产物项已回填）。
    Persisted(ChatSseEvent),
}

/// runner 的事件出口：`LooperEvent` 进，`PecoRunEvent` 出，中途该落库的落库。
pub struct RunEmitter {
    tx: broadcast::Sender<PecoRunEvent>,
    db: sqlx::SqlitePool,
    workspace_manager: Arc<WorkspaceManager>,
    user_id: String,
}

impl RunEmitter {
    pub fn new(
        tx: broadcast::Sender<PecoRunEvent>,
        db: sqlx::SqlitePool,
        workspace_manager: Arc<WorkspaceManager>,
        user_id: String,
    ) -> Self {
        Self {
            tx,
            db,
            workspace_manager,
            user_id,
        }
    }

    /// 广播一条 looper 事件：带产物声明的 `ToolResult` **先落库、后广播**。
    ///
    /// 事件里的 artifact id 是前端取流的唯一凭据，反过来会给出一个指向不存在
    /// id 的事件。落库放在这里（runner 侧）而非桥接层，是因为桥接任务随 SSE
    /// 连接断开而终止 —— 挂在那里的落库会随连接一起消失，断开期间生成的产物
    /// 永久丢失。本函数是全 run 唯一不依赖订阅者的出口。
    pub async fn emit(&self, event: LooperEvent) {
        let out = match event {
            // `artifacts` 为空是绝大多数工具调用的常态：不建行、不落库，原样透传。
            LooperEvent::ToolResult {
                id,
                name,
                result,
                images,
                artifacts,
            } if !artifacts.is_empty() => {
                // 全函数只派生一次：私有会话行与产物归属必须是同一个 id。
                let conversation_id = private_session_id(&self.user_id);
                // 懒建在此处而非启动时：覆盖新建 run / 附着 / 重连三种入口，且对
                // 存量安装天然生效。失败只降级 —— 随后每条声明会被外键逐条拒掉，
                // 事件里 artifacts 为空，主对话照常，run 不因此退出。
                let ensured = crate::db::conversations::ensure_private_session(
                    &self.db,
                    &self.user_id,
                    &conversation_id,
                    SESSION_AGENT_NAME,
                    SESSION_TITLE,
                )
                .await;
                if let Err(e) = ensured {
                    warn!(
                        user_id = %self.user_id,
                        error = %e,
                        "peco private session row unavailable; artifact persist will degrade"
                    );
                }
                let work = crate::artifact::work_dirs(&self.workspace_manager, &self.user_id);
                PecoRunEvent(PecoRunEventKind::Persisted(
                    crate::artifact::build_tool_result_event(
                        &self.db,
                        work.as_ref(),
                        &conversation_id,
                        &self.user_id,
                        id,
                        name,
                        result,
                        images,
                        &artifacts,
                    )
                    .await,
                ))
            }
            other => PecoRunEvent(PecoRunEventKind::Looper(other)),
        };

        // 无订阅者时广播失败是常态（Err 忽略）；落库已在上面完成，不随它失败。
        let _ = self.tx.send(out);
    }
}

/// 把 runner 广播的事件转成待发送的 SSE 事件；不需要发事件的返回 `None`。
///
/// 纯映射，不碰 DB：需要落库的产物事件已由 [`RunEmitter::emit`] 构造好。
pub fn run_event_to_sse(user_id: &str, event: PecoRunEvent) -> Option<ChatSseEvent> {
    match event.0 {
        PecoRunEventKind::Persisted(ev) => Some(ev),
        PecoRunEventKind::Looper(ev) => map_looper_event(ev, &private_session_id(user_id)),
    }
}
