// ============================================================================
// session_map — Session-Agent 映射 + Agent 感知的持久化器
// ============================================================================
//
// 两层职责：
// 1. SessionAgentMap：独立文件维护 session_id → agent_name 映射，用于
//    按 Agent 过滤会话列表（不依赖 Session.description 字段）。
// 2. AgentAwareSessionPersister：包装 FileSessionPersister，拦截 save()
//    从 SessionSnapshot 中提取第一条 User query 作为 description 写入。
//    这样 description 始终是会话的内容描述，而非 agent name。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use model_provider::Message;
use peco_core::persistence::{
    FileSessionPersister, PersistError, PersistResult, SessionPersister,
};
use peco_core::session::{SessionMeta, SessionSnapshot};
use tokio::sync::RwLock;

// ============================================================================
// SessionAgentMap
// ============================================================================

/// session_id → agent_name 映射，存储在独立的 JSON 文件中。
///
/// 与 Session 持久化文件解耦 —— Session 自身不感知 agent_name，
/// 映射关系由 CLI 层独立维护。
pub struct SessionAgentMap {
    path: PathBuf,
    map: RwLock<HashMap<String, String>>,
}

impl SessionAgentMap {
    /// 从 workspace 的 `.peco/session_agent_map.json` 加载映射。
    pub async fn load(workspace_root: &Path) -> anyhow::Result<Self> {
        let path = workspace_root
            .join(".peco")
            .join("session_agent_map.json");

        let map: HashMap<String, String> = if path.exists() {
            let content = tokio::fs::read_to_string(&path).await?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            HashMap::new()
        };

        Ok(Self {
            path,
            map: RwLock::new(map),
        })
    }

    /// 插入 session_id → agent_name 映射并持久化到磁盘。
    pub async fn insert(&self, session_id: &str, agent_name: &str) -> anyhow::Result<()> {
        {
            let mut map = self.map.write().await;
            map.insert(session_id.to_string(), agent_name.to_string());
            self.save_locked(&map).await?;
        }
        Ok(())
    }

    /// 返回指定 agent 拥有的全部 session ID。
    pub async fn sessions_for_agent(&self, agent_name: &str) -> HashSet<String> {
        let map = self.map.read().await;
        map.iter()
            .filter(|(_, name)| *name == agent_name)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// 持久化映射到 JSON 文件。
    async fn save_locked(&self, map: &HashMap<String, String>) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string_pretty(map)?;
        tokio::fs::write(&self.path, json).await?;
        Ok(())
    }
}

// ============================================================================
// AgentAwareSessionPersister
// ============================================================================

/// 包装 FileSessionPersister，改造 description 语义。
///
/// - `save()`：忽略传入的 description，从 Snapshot 提取首条 User query
/// - `list_by_agent()`：结合 SessionAgentMap 按 agent 过滤 session 列表
/// - `register_session()`：将新 session 写入映射文件
pub struct AgentAwareSessionPersister {
    inner: FileSessionPersister,
    session_map: SessionAgentMap,
}

impl AgentAwareSessionPersister {
    /// 创建实例。
    pub async fn new(
        sessions_dir: PathBuf,
        workspace_root: &Path,
    ) -> anyhow::Result<Self> {
        let inner = FileSessionPersister::new(sessions_dir).await?;
        let session_map = SessionAgentMap::load(workspace_root).await?;
        Ok(Self {
            inner,
            session_map,
        })
    }

    /// 注册新 session 的 agent 归属关系。
    pub async fn register_session(
        &self,
        session_id: &str,
        agent_name: &str,
    ) -> anyhow::Result<()> {
        self.session_map.insert(session_id, agent_name).await
    }

    /// 按 agent 名称过滤已保存的会话列表。
    pub async fn list_by_agent(
        &self,
        agent_name: &str,
    ) -> Result<Vec<SessionMeta>, PersistError> {
        let agent_sessions = self.session_map.sessions_for_agent(agent_name).await;
        let all = self.inner.list().await?;
        Ok(all
            .into_iter()
            .filter(|m| agent_sessions.contains(&m.id))
            .collect())
    }
}

#[async_trait]
impl SessionPersister for AgentAwareSessionPersister {
    async fn save(
        &self,
        snapshot: &SessionSnapshot,
        session_id: &str,
        description: &str,
        created_at: u64,
    ) -> Result<PersistResult, PersistError> {
        // 从 snapshot 提取首条 User query 作为真实 description
        let real_desc = extract_first_query(snapshot).unwrap_or(description);
        self.inner
            .save(snapshot, session_id, real_desc, created_at)
            .await
    }

    async fn load(
        &self,
        session_id: &str,
    ) -> Result<Option<(SessionSnapshot, SessionMeta)>, PersistError> {
        self.inner.load(session_id).await
    }

    async fn delete(&self, session_id: &str) -> Result<(), PersistError> {
        self.inner.delete(session_id).await
    }

    async fn list(&self) -> Result<Vec<SessionMeta>, PersistError> {
        self.inner.list().await
    }
}

// ============================================================================
// 辅助
// ============================================================================

/// 从 SessionSnapshot 中提取第一条 User 消息文本。
///
/// 会话的首条消息总是用户 query，位于 `committed_turns[0][0]`。
fn extract_first_query(snapshot: &SessionSnapshot) -> Option<&str> {
    snapshot
        .committed_turns
        .first()?
        .first()
        .and_then(|am| match am.message.as_ref() {
            Message::User { content } => Some(content.as_str()),
            _ => None,
        })
}
