//! 投递：把一条合成好的唤醒文本送进 Peco 空间的 `@master` 会话，并让它跑完一轮。
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
use crate::space::SpaceEnv;
use crate::space::peco::PecoIdentity;
use crate::space::registry::SpaceRegistry;

/// 唤醒落到哪个 agent 的会话：UI 里 Peco 的对话面就是它。
const PECO_AGENT_NAME: &str = "@master";

/// 一次投递的结论；台账的下一步（`delivered` / 回滚 / `failed`）由它决定。
pub(crate) enum Delivery {
    /// 已经交给运行器跑起来。
    Delivered,
    /// 目标会话有人在跑：本轮让路。
    Busy,
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
    async fn is_busy(&self) -> bool;
    /// 投递一条合成好的文本。
    async fn deliver(&self, text: String) -> Delivery;
}

/// 唤醒的投递端：一条文本 → Peco 空间 `@master` 会话里的一轮对话。
pub struct PecoDelivery {
    pool: SqlitePool,
    spaces: Arc<SpaceRegistry>,
    chat_runs: Arc<ChatActiveRuns>,
    peco: Arc<PecoIdentity>,
    /// 只持弱引用：强引用会与观察者手里的泵构成环
    /// （delivery → env → tasks → pump → delivery）。这套图本就与进程同寿，
    /// 但测试里每建一套都会漏一份，故与 [`SpaceEnv::hub`] 同一手法。
    env: Weak<SpaceEnv>,
}

impl PecoDelivery {
    pub fn new(
        pool: SqlitePool,
        spaces: Arc<SpaceRegistry>,
        chat_runs: Arc<ChatActiveRuns>,
        peco: Arc<PecoIdentity>,
        env: Weak<SpaceEnv>,
    ) -> Self {
        Self {
            pool,
            spaces,
            chat_runs,
            peco,
            env,
        }
    }

    /// 已有的目标会话；只读，见 [`find_wake_conversation`]。
    async fn existing_conversation(&self) -> Result<Option<ConversationRow>, String> {
        find_wake_conversation(&self.pool, self.peco.ws_id.as_str()).await
    }

    /// 投递要落到的那条会话：没有就建出空间会话。
    ///
    /// 建会话只发生在真正投递之前：一条注定投不出去的唤醒不该在对话列表里留下
    /// 空壳（`is_busy` 每轮都要问一次，它会先把空壳攒出来）。
    ///
    /// 落点是空间会话而不是另开一条：唤醒的内容属于用户在空间页看的那段对话，
    /// 另起一条只会把消息藏进一个用户没打开过的会话。
    async fn resolve_conversation(&self) -> Result<ConversationRow, String> {
        if let Some(row) = self.existing_conversation().await? {
            return Ok(row);
        }

        let ws_id = self.peco.ws_id.as_str();
        let (id, created) = conversations::ensure_space_session(&self.pool, ws_id, PECO_AGENT_NAME)
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
}

#[async_trait::async_trait]
impl DeliverySink for PecoDelivery {
    /// 忙 = 这一轮真的在跑，而不是「页面还开着」：注册条目随一轮结束 drop，
    /// 连接本身不再算占位（见 [`crate::chat::run`] 的 Idle 退出）。
    async fn is_busy(&self) -> bool {
        match self.existing_conversation().await {
            Ok(Some(conv)) => self.chat_runs.is_active(&conv.id),
            // 还没有会话就没有人能占着它。
            Ok(None) => false,
            // 判不出来时不算忙：让认领发生，好让这一轮把失败记进台账，
            // 而不是每轮重试却什么都看不见。
            Err(e) => {
                warn!(error = %e, "cannot resolve the peco conversation; treating it as idle");
                false
            }
        }
    }

    async fn deliver(&self, text: String) -> Delivery {
        let conv = match self.resolve_conversation().await {
            Ok(conv) => conv,
            Err(e) => return Delivery::Failed(e),
        };
        let Some(env) = self.env.upgrade() else {
            return Delivery::Failed("space env is gone".to_string());
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
            &self.peco.ws_id,
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

/// 已有的目标会话：Peco 空间里 `@master` 的空间会话（`scope='space'`）。
///
/// 只认空间会话：用户在空间页看的、`/conversation/ensure` 建的都是同一条，投进
/// 别处等于「消息存在但用户看不见」—— 唤醒轮的消息要落在他正在看的那段历史里。
///
/// 只读 —— 问「忙不忙」没有资格建会话；建会话只发生在真正投递之前
/// （[`PecoDelivery::resolve_conversation`]）。
async fn find_wake_conversation(
    pool: &SqlitePool,
    ws_id: &str,
) -> Result<Option<ConversationRow>, String> {
    let Some(id) = conversations::find_space_session(pool, ws_id, PECO_AGENT_NAME)
        .await
        .map_err(|e| format!("cannot resolve the space session: {e}"))?
    else {
        return Ok(None);
    };
    conversations::find_by_id(pool, &id)
        .await
        .map_err(|e| format!("cannot read the space session: {e}"))
}

/// 把装配失败面翻成台账结论：`Busy` 是让路（回滚、不计尝试），其余是真失败。
fn delivery_of(error: RunError) -> Delivery {
    match error {
        RunError::Busy => Delivery::Busy,
        RunError::Space(e) => Delivery::Failed(format!("target space is unusable: {e}")),
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

        let found = find_wake_conversation(&pool, "ws_peco").await.unwrap();

        assert!(found.is_none());
        assert_eq!(count_conversations(&pool).await, 0, "只读定位不得建会话");
    }

    /// 命中范围只到「本空间 + `@master` + `scope='space'`」：别的空间、别的 agent
    /// 的会话投进去等于把唤醒发给不相干的人，而 `scope='agent'` 的会话虽然也属于
    /// 本空间本 agent，却不是用户在空间页看的那条 —— 投进去就是「消息存在但看不
    /// 见」。
    ///
    /// 变异：把定位改回 `list_by_workspace_and_agent(...).first()` ⇒ 命中的是
    /// 更早插入的那条 `scope='agent'` 会话，第一条断言变红。
    #[tokio::test]
    async fn only_the_space_session_of_the_peco_master_is_matched() {
        let (pool, _dir) = test_pool().await;
        for (id, ws_id, agent_name) in [
            ("c_other_ws", "ws_other", PECO_AGENT_NAME),
            ("c_other_agent", "ws_peco", "@memory"),
            ("c_agent_scope", "ws_peco", PECO_AGENT_NAME),
        ] {
            conversations::insert(
                &pool,
                &CreateConversationParams {
                    id: id.to_string(),
                    workspace_id: ws_id.to_string(),
                    agent_id: None,
                    agent_name: agent_name.to_string(),
                    title: "t".to_string(),
                },
            )
            .await
            .unwrap();
        }
        let (space_id, created) =
            conversations::ensure_space_session(&pool, "ws_peco", PECO_AGENT_NAME)
                .await
                .unwrap();
        assert!(created, "夹具要先真的建出空间会话");

        let found = find_wake_conversation(&pool, "ws_peco")
            .await
            .unwrap()
            .expect("本空间 `@master` 的空间会话要能被定位");

        assert_eq!(found.id, space_id, "只认 scope='space' 的那条");
        assert_ne!(found.id, "c_agent_scope", "普通 agent 会话不是投递目标");

        // 别的空间还没有空间会话：不得跨空间命中。
        assert!(
            find_wake_conversation(&pool, "ws_ghost")
                .await
                .unwrap()
                .is_none(),
            "别的空间没有会话时不得给出一条"
        );
    }
}
