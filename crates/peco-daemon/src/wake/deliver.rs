//! 投递：把一条合成好的唤醒文本送进派发方所在空间的会话，并让它跑完一轮。
//!
//! 本模块只回答三件事：投给哪个会话、此刻算不算忙、失败怎么归类。会话怎么建、
//! 一轮怎么跑都不在这里发明 —— 前者复用空间会话的幂等创建，后者复用
//! [`chat::run`](crate::chat::run) 的无客户端入口。

use std::sync::{Arc, Weak};

use model_provider::Content;
use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::chat::active::ChatActiveRuns;
use crate::chat::run::{RunAudience, RunDeps, RunError, spawn_run};
use crate::db::conversations::{self, ConversationRow};
use crate::error::DaemonError;
use crate::space::SpaceEnv;
use crate::space::registry::SpaceRegistry;

/// 唤醒落到哪个 agent 的会话：UI 里空间页的对话面就是它。
const SPACE_AGENT_NAME: &str = "@master";

/// 一次投递的目标；`conversation_id` 为 `None` 时投来源空间的 `@master` 空间会话。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WakeTarget {
    pub ws_id: String,
    pub conversation_id: Option<String>,
}

impl WakeTarget {
    /// 来源空间的空间会话（`scope='space'`）。
    pub fn space(ws_id: impl Into<String>) -> Self {
        Self {
            ws_id: ws_id.into(),
            conversation_id: None,
        }
    }

    /// 发起派发的那条聊天会话。
    pub fn conversation(ws_id: impl Into<String>, conversation_id: impl Into<String>) -> Self {
        Self {
            ws_id: ws_id.into(),
            conversation_id: Some(conversation_id.into()),
        }
    }
}

/// 一次投递的结论；台账的下一步（`delivered` / 回滚 / `skipped` / `failed`）由它决定。
pub(crate) enum Delivery {
    /// 已经交给运行器跑起来。
    Delivered,
    /// 目标会话有人在跑：本轮让路。
    Busy,
    /// 目标空间短暂不可用（打开与注册之间的竞态）：本轮让路，不计尝试。
    Deferred,
    /// 目标空间已不存在（回收站/空间行丢失），这条通知永远投不出去。
    Skipped,
    /// 连交给运行器都没做到。
    Failed(String),
}

/// 泵需要的投递能力。
///
/// 抽成 trait 是为了让「认领之后竞态输给用户」这条分支可测：它要求一个用户
/// 注册恰好在「查空闲」与「原子注册」之间落地，真实链路上构造不出这个窗口。
#[async_trait::async_trait]
pub(crate) trait DeliverySink: Send + Sync {
    /// 认领前的门控：目标会话忙时本轮不认领，行留在 `pending`。
    async fn is_busy(&self, target: &WakeTarget) -> bool;
    /// 投递一条合成好的文本。
    async fn deliver(&self, target: &WakeTarget, text: String) -> Delivery;
}

/// 唤醒的投递端：一条文本 → 来源空间里的某个会话的一轮对话。
pub struct WakeDelivery {
    pool: SqlitePool,
    spaces: Arc<SpaceRegistry>,
    chat_runs: Arc<ChatActiveRuns>,
    /// 只持弱引用：强引用会与观察者手里的泵构成环
    /// （delivery → env → tasks → pump → delivery）。这套图本就与进程同寿，
    /// 但测试里每建一套都会漏一份，故与 [`SpaceEnv::hub`] 同一手法。
    env: Weak<SpaceEnv>,
}

impl WakeDelivery {
    pub fn new(
        pool: SqlitePool,
        spaces: Arc<SpaceRegistry>,
        chat_runs: Arc<ChatActiveRuns>,
        env: Weak<SpaceEnv>,
    ) -> Self {
        Self {
            pool,
            spaces,
            chat_runs,
            env,
        }
    }

    async fn existing_conversation(
        &self,
        target: &WakeTarget,
    ) -> Result<Option<ConversationRow>, String> {
        find_existing_conversation(&self.pool, target).await
    }

    /// 投递要落到的那条会话：没有就建出空间会话。
    ///
    /// 建会话只发生在真正投递之前：一条注定投不出去的唤醒不该在对话列表里留下
    /// 空壳（`is_busy` 每轮都要问一次，它会先把空壳攒出来）。
    ///
    /// 来源会话被删掉时回落到空间会话：结果不能因为用户清了一条聊天记录就丢掉。
    async fn resolve_conversation(&self, target: &WakeTarget) -> Result<ConversationRow, String> {
        if let Some(row) = self.existing_conversation(target).await? {
            return Ok(row);
        }

        let ws_id = target.ws_id.as_str();
        let (id, created) =
            conversations::ensure_space_session(&self.pool, ws_id, SPACE_AGENT_NAME)
                .await
                .map_err(|e| format!("cannot create the wake conversation: {e}"))?;
        if created {
            info!(ws_id = %ws_id, conversation_id = %id, "wake conversation created");
        }

        conversations::find_by_id(&self.pool, &id)
            .await
            .map_err(|e| format!("cannot read back the wake conversation: {e}"))?
            .ok_or_else(|| "the wake conversation vanished right after ensure".to_string())
    }

    /// 目标空间不可用时的归类：短暂竞态让路，空间没了就永久跳过。
    fn space_failure(error: DaemonError) -> Delivery {
        match error {
            DaemonError::SpaceNotOpen { .. } | DaemonError::WsBusy { .. } => Delivery::Deferred,
            DaemonError::SpaceNotFound { .. }
            | DaemonError::SpaceTrashed { .. }
            | DaemonError::WorkDirMissing { .. } => Delivery::Skipped,
            other => Delivery::Failed(format!("target space is unusable: {other}")),
        }
    }
}

#[async_trait::async_trait]
impl DeliverySink for WakeDelivery {
    /// 忙 = 这一轮真的在跑，而不是「页面还开着」：注册条目随一轮结束 drop，
    /// 连接本身不再算占位（见 [`crate::chat::run`] 的 Idle 退出）。
    async fn is_busy(&self, target: &WakeTarget) -> bool {
        match self.existing_conversation(target).await {
            Ok(Some(conv)) => self.chat_runs.is_active(&conv.id),
            // 还没有会话就没有人能占着它。
            Ok(None) => false,
            // 判不出来时不算忙：让认领发生，好让这一轮把失败记进台账，
            // 而不是每轮重试却什么都看不见。
            Err(e) => {
                warn!(ws_id = %target.ws_id, error = %e, "cannot resolve the wake conversation; treating it as idle");
                false
            }
        }
    }

    async fn deliver(&self, target: &WakeTarget, text: String) -> Delivery {
        let Some(env) = self.env.upgrade() else {
            return Delivery::Failed("space env is gone".to_string());
        };

        // 来源空间可能已被用户关闭：先把实例打开再投。空间没了（回收站/行丢失）
        // 是永久不可投，交给 `space_failure` 归类。
        match self.spaces.workspace(&target.ws_id).await {
            Ok(_) => {}
            Err(DaemonError::SpaceNotOpen { .. }) => {
                if let Err(e) = self.spaces.open(&target.ws_id).await {
                    return Self::space_failure(e);
                }
            }
            Err(e) => return Self::space_failure(e),
        }

        let conv = match self.resolve_conversation(target).await {
            Ok(conv) => conv,
            Err(e) => return Delivery::Failed(e),
        };

        // 注册由 `spawn_run` 在校验之后做：查空闲与注册之间用户可能刚好打开这个
        // 会话，输掉的一方拿到 `Busy`，行回滚成待投递而不是记一次失败。顺带保证
        // 「空间没打开」这类装配失败也不会动用户正在看的那一轮。
        let deps = RunDeps {
            pool: &self.pool,
            env: &env,
            spaces: &self.spaces,
            chat_runs: &self.chat_runs,
        };
        match spawn_run(
            deps,
            &target.ws_id,
            &conv,
            text.clone(),
            Content::Text(text),
            RunAudience::Headless,
        )
        .await
        {
            // 无人看帧：这一轮的订阅端直接丢弃（正文与用量已由 feed 与落库覆盖）。
            Ok(_) => Delivery::Delivered,
            Err(e) => delivery_of(e),
        }
    }
}

/// 已有的目标会话：优先发起会话，其次本空间的 `@master` 空间会话（`scope='space'`）。
///
/// 只读 —— 问「忙不忙」没有资格建会话；建会话只发生在真正投递之前
/// （[`WakeDelivery::resolve_conversation`]）。
async fn find_existing_conversation(
    pool: &SqlitePool,
    target: &WakeTarget,
) -> Result<Option<ConversationRow>, String> {
    if let Some(conversation_id) = target.conversation_id.as_deref() {
        let found = conversations::find_by_id_and_workspace(pool, conversation_id, &target.ws_id)
            .await
            .map_err(|e| format!("cannot resolve the origin conversation: {e}"))?;
        if found.is_some() {
            return Ok(found);
        }
    }

    let Some(id) = conversations::find_space_session(pool, &target.ws_id, SPACE_AGENT_NAME)
        .await
        .map_err(|e| format!("cannot resolve the space session: {e}"))?
    else {
        return Ok(None);
    };
    conversations::find_by_id(pool, &id)
        .await
        .map_err(|e| format!("cannot read the space session: {e}"))
}

/// 把装配失败面翻成台账结论：`Busy` / `Deferred` 是让路（回滚、不计尝试），`Skipped`
/// 是空间没了，其余是真失败。
fn delivery_of(error: RunError) -> Delivery {
    match error {
        RunError::Busy => Delivery::Busy,
        RunError::Space(e) => WakeDelivery::space_failure(e),
        RunError::Agent(message) => Delivery::Failed(message),
        RunError::Provider(_, message) => Delivery::Failed(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::conversations::CreateConversationParams;

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        (pool, dir)
    }

    async fn count_conversations(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM conversations")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// 「忙不忙」每 tick 都要问一次：定位若顺手建会话，一条永远投不出去的唤醒就会
    /// 在对话列表里攒出空壳。
    #[tokio::test]
    async fn locating_the_wake_conversation_never_creates_one() {
        let (pool, _dir) = test_pool().await;

        let found = find_existing_conversation(&pool, &WakeTarget::space("ws_peco"))
            .await
            .unwrap();

        assert!(found.is_none());
        assert_eq!(count_conversations(&pool).await, 0, "只读定位不得建会话");
    }

    #[tokio::test]
    async fn the_origin_conversation_wins_over_the_space_session() {
        let (pool, _dir) = test_pool().await;
        conversations::insert(
            &pool,
            &CreateConversationParams {
                id: "c_origin".to_string(),
                workspace_id: "ws_peco".to_string(),
                agent_id: None,
                agent_name: "@master".to_string(),
                title: "t".to_string(),
            },
        )
        .await
        .unwrap();
        let (space_id, created) =
            conversations::ensure_space_session(&pool, "ws_peco", SPACE_AGENT_NAME)
                .await
                .unwrap();
        assert!(created, "夹具要先真的建出空间会话");

        let found =
            find_existing_conversation(&pool, &WakeTarget::conversation("ws_peco", "c_origin"))
                .await
                .unwrap()
                .expect("发起会话必须被定位");
        assert_eq!(found.id, "c_origin");

        let found =
            find_existing_conversation(&pool, &WakeTarget::conversation("ws_other", "c_origin"))
                .await
                .unwrap();
        assert!(found.is_none(), "别的空间没有空间会话时不得凭空给出会话");
        let fallback = find_existing_conversation(&pool, &WakeTarget::space("ws_peco"))
            .await
            .unwrap()
            .expect("回落目标是空间会话");
        assert_eq!(fallback.id, space_id, "只认 scope='space' 的那条");
    }

    #[tokio::test]
    async fn a_missing_origin_conversation_falls_back_to_the_space_session() {
        let (pool, _dir) = test_pool().await;
        let (space_id, _) = conversations::ensure_space_session(&pool, "ws_peco", SPACE_AGENT_NAME)
            .await
            .unwrap();

        let found =
            find_existing_conversation(&pool, &WakeTarget::conversation("ws_peco", "c_deleted"))
                .await
                .unwrap()
                .expect("必须回落到空间会话");

        assert_eq!(found.id, space_id);
    }
}
