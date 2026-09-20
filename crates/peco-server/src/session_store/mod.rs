// ============================================================================
// SqliteSessionPersister — SQLite 版 Session 持久化
// ============================================================================
//
// 实现 peco_core::persistence::SessionPersister trait，
// 将 SessionSnapshot 以 JSON 格式存入 session_snapshots 表，
// SessionMeta 动态字段由 SqliteSessionPersister 自行计算。
//
// 另承载在途轮检查点（conversation_inflight_turns）— 崩溃恢复用，
// 水化入口是本模块的 [`hydrate_inflight_turn`]。

use std::path::PathBuf;

use async_trait::async_trait;
use peco_core::persistence::{InflightCheckpoint, PersistError, PersistResult, SessionPersister};
use peco_core::session::{Session, SessionMeta, SessionSnapshot};
use sqlx::SqlitePool;
use tracing::{error, info, warn};

/// SQLite 版 SessionPersister。
///
/// 每个实例绑定一个 conversation_id，`session_id` 参数即 conversation_id。
/// 支持 save/load/delete 操作，不持状态。
#[derive(Clone)]
pub struct SqliteSessionPersister {
    /// SQLite 连接池。
    pool: SqlitePool,
}

impl SqliteSessionPersister {
    /// 创建新的 SQLite persister。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SessionPersister for SqliteSessionPersister {
    async fn save(
        &self,
        snapshot: &SessionSnapshot,
        session_id: &str,
        description: &str,
        created_at: u64,
    ) -> Result<PersistResult, PersistError> {
        let snapshot_json = serde_json::to_string(snapshot).map_err(PersistError::Serialization)?;

        let bytes_written = snapshot_json.len() as u64;

        sqlx::query(
            "INSERT INTO session_snapshots (conversation_id, session_id, description, created_at, snapshot_json) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(conversation_id) DO UPDATE SET \
             session_id = excluded.session_id, \
             description = excluded.description, \
             snapshot_json = excluded.snapshot_json, \
             updated_at = datetime('now')",
        )
        .bind(session_id)
        .bind(session_id)
        .bind(description)
        .bind(created_at as i64)
        .bind(&snapshot_json)
        .execute(&self.pool)
        .await
        .map_err(|e| PersistError::Io(std::io::Error::other(e.to_string())))?;

        Ok(PersistResult {
            bytes_written,
            path: PathBuf::from(format!("sqlite:session_snapshots/{session_id}")),
        })
    }

    async fn load(
        &self,
        session_id: &str,
    ) -> Result<Option<(SessionSnapshot, SessionMeta)>, PersistError> {
        #[derive(sqlx::FromRow)]
        struct SnapshotRow {
            description: String,
            created_at: i64,
            snapshot_json: String,
            updated_at: String,
        }

        let row = sqlx::query_as::<_, SnapshotRow>(
            "SELECT description, created_at, snapshot_json, updated_at \
             FROM session_snapshots WHERE conversation_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| PersistError::Io(std::io::Error::other(e.to_string())))?;

        match row {
            Some(r) => {
                let snapshot: SessionSnapshot =
                    serde_json::from_str(&r.snapshot_json).map_err(|e| {
                        // 历史快照可能是旧格式（Message → InputItem 迁移前的 blob），
                        // 反序列化失败即判定为不兼容，此处记录清晰日志供排查。
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "failed to deserialize session snapshot (old format or corrupt); cannot restore history"
                        );
                        PersistError::Serialization(e)
                    })?;

                // 从 snapshot 计算动态字段
                let tokens_used =
                    (snapshot.total_usage.input_tokens + snapshot.total_usage.output_tokens) as u64;
                let completed_turns = snapshot.committed_turns.len();
                let updated_at =
                    chrono::NaiveDateTime::parse_from_str(&r.updated_at, "%Y-%m-%d %H:%M:%S")
                        .map(|dt| dt.and_utc().timestamp() as u64)
                        .unwrap_or(r.created_at as u64);

                let meta = SessionMeta {
                    id: session_id.to_string(),
                    description: r.description,
                    tokens_used,
                    completed_turns,
                    created_at: r.created_at as u64,
                    updated_at,
                };

                Ok(Some((snapshot, meta)))
            }
            None => Ok(None),
        }
    }

    async fn delete(&self, session_id: &str) -> Result<(), PersistError> {
        sqlx::query("DELETE FROM session_snapshots WHERE conversation_id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| PersistError::Io(std::io::Error::other(e.to_string())))?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<SessionMeta>, PersistError> {
        #[derive(sqlx::FromRow)]
        struct ListRow {
            conversation_id: String,
            description: String,
            created_at: i64,
            snapshot_json: String,
            updated_at: String,
        }

        let rows = sqlx::query_as::<_, ListRow>(
            "SELECT conversation_id, description, created_at, snapshot_json, updated_at \
             FROM session_snapshots ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| PersistError::Io(std::io::Error::other(e.to_string())))?;

        let mut metas = Vec::with_capacity(rows.len());
        for r in rows {
            let snapshot: SessionSnapshot =
                serde_json::from_str(&r.snapshot_json).map_err(PersistError::Serialization)?;

            let tokens_used =
                (snapshot.total_usage.input_tokens + snapshot.total_usage.output_tokens) as u64;
            let completed_turns = snapshot.committed_turns.len();
            let updated_at =
                chrono::NaiveDateTime::parse_from_str(&r.updated_at, "%Y-%m-%d %H:%M:%S")
                    .map(|dt| dt.and_utc().timestamp() as u64)
                    .unwrap_or(r.created_at as u64);

            metas.push(SessionMeta {
                id: r.conversation_id,
                description: r.description,
                tokens_used,
                completed_turns,
                created_at: r.created_at as u64,
                updated_at,
            });
        }

        Ok(metas)
    }

    // ── 在途轮检查点（崩溃恢复）──────────────────────────────────────────

    async fn save_inflight(&self, checkpoint: &InflightCheckpoint) -> Result<(), PersistError> {
        let payload_json =
            serde_json::to_string(&checkpoint.staged).map_err(PersistError::Serialization)?;

        sqlx::query(
            "INSERT INTO conversation_inflight_turns \
             (conversation_id, session_id, turn_index, reason, payload_json) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(conversation_id) DO UPDATE SET \
             session_id = excluded.session_id, \
             turn_index = excluded.turn_index, \
             reason = excluded.reason, \
             payload_json = excluded.payload_json, \
             updated_at = datetime('now')",
        )
        .bind(&checkpoint.session_id)
        .bind(&checkpoint.session_id)
        .bind(checkpoint.turn_index as i64)
        .bind(&checkpoint.reason)
        .bind(&payload_json)
        .execute(&self.pool)
        .await
        .map_err(|e| PersistError::Io(std::io::Error::other(e.to_string())))?;

        Ok(())
    }

    async fn load_inflight(
        &self,
        session_id: &str,
    ) -> Result<Option<InflightCheckpoint>, PersistError> {
        #[derive(sqlx::FromRow)]
        struct InflightRow {
            turn_index: i64,
            reason: String,
            payload_json: String,
        }

        let row = sqlx::query_as::<_, InflightRow>(
            "SELECT turn_index, reason, payload_json \
             FROM conversation_inflight_turns WHERE conversation_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| PersistError::Io(std::io::Error::other(e.to_string())))?;

        match row {
            Some(r) => {
                // 检查点写入与读取都在同一进程内闭环，格式不兼容只可能是
                // 磁盘损坏；按「没有检查点」处理，不阻塞会话启动。
                let staged =
                    serde_json::from_str(&r.payload_json).map_err(PersistError::Serialization)?;
                Ok(Some(InflightCheckpoint {
                    session_id: session_id.to_string(),
                    turn_index: r.turn_index as usize,
                    reason: r.reason,
                    staged,
                }))
            }
            None => Ok(None),
        }
    }

    async fn delete_inflight(&self, session_id: &str) -> Result<(), PersistError> {
        sqlx::query("DELETE FROM conversation_inflight_turns WHERE conversation_id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| PersistError::Io(std::io::Error::other(e.to_string())))?;
        Ok(())
    }
}

// ============================================================================
// 冷启动水化
// ============================================================================

/// 把上次进程崩溃留下的在途轮检查点冻结入史。
///
/// 检查点内容（一批已落地的工具结果）由 [`Session::hydrate_inflight`] 灌回
/// staging，再走 [`Session::interrupt_turn`] 完成补齐与冻结 —— 与进程内中断
/// 路径收敛到同一个动作，不需要 looper 从 `ExecutingTools` 冷启动。
///
/// 三种情况直接丢弃检查点：轮次编号对不上（陈旧行）、内容为空、灌入失败。
/// **任何失败都只记日志** —— 恢复失败不该挡住用户发新消息。
pub async fn hydrate_inflight_turn(persister: &SqliteSessionPersister, session: &mut Session) {
    let checkpoint = match persister.load_inflight(session.id()).await {
        Ok(Some(checkpoint)) => checkpoint,
        Ok(None) => return,
        Err(e) => {
            warn!(
                session_id = %session.id(),
                error = %e,
                "Failed to load inflight turn checkpoint"
            );
            return;
        }
    };

    // 陈旧行：收尾已跑完但删除失败。committed 里可能已经有这一轮，
    // 灌进当前会话会与既有历史错位。
    if checkpoint.turn_index != session.turn_index() {
        warn!(
            session_id = %session.id(),
            checkpoint_turn = checkpoint.turn_index,
            session_turn = session.turn_index(),
            "Discarding stale inflight turn checkpoint"
        );
        let _ = persister.delete_inflight(session.id()).await;
        return;
    }

    if checkpoint.staged.is_empty() {
        let _ = persister.delete_inflight(session.id()).await;
        return;
    }

    let reason = checkpoint.reason;
    match session.hydrate_inflight(checkpoint.staged) {
        Ok(hydrated) => match session.interrupt_turn(&reason) {
            Ok(token) => {
                // 冻结必须落盘：用户可能只是打开页面看一眼，不会再发消息。
                let snapshot = session.snapshot(&token);
                if let Err(e) = persister
                    .save(
                        &snapshot,
                        session.id(),
                        session.description(),
                        session.created_at(),
                    )
                    .await
                {
                    error!(error = %e, "Failed to persist hydrated inflight turn");
                }
                info!(
                    session_id = %session.id(),
                    hydrated,
                    reason = %reason,
                    "Inflight turn hydrated and frozen into history"
                );
            }
            Err(e) => error!(error = %e, "Failed to freeze hydrated inflight turn"),
        },
        Err(e) => error!(error = %e, "Failed to hydrate inflight turn"),
    }

    let _ = persister.delete_inflight(session.id()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use model_provider::{Content, InputItem, Role, Usage};
    use peco_core::session::{AnnotatedMessage, MessageId, MessageSource, SessionSnapshot};

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = db::connect(&url).await.unwrap();
        db::run_migrations(&pool).await.unwrap();
        (pool, dir)
    }

    fn user(text: &str) -> InputItem {
        InputItem::Message {
            role: Role::User,
            content: Content::Text(text.to_string()),
        }
    }

    fn function_call(call_id: &str) -> InputItem {
        InputItem::FunctionCall {
            call_id: call_id.to_string(),
            name: "shell".to_string(),
            arguments: "{}".to_string(),
        }
    }

    fn staged() -> Vec<AnnotatedMessage> {
        vec![
            AnnotatedMessage::new(
                MessageId(0),
                0,
                user("跑个长任务"),
                MessageSource::UserInput,
            ),
            AnnotatedMessage::new(
                MessageId(1),
                0,
                function_call("c1"),
                MessageSource::ModelGeneration,
            ),
        ]
    }

    fn snapshot_with(turns: usize) -> SessionSnapshot {
        SessionSnapshot {
            committed_turns: Vec::new(),
            turn_index: turns,
            total_usage: Usage::default(),
            next_message_id: 0,
            pending_inputs: Vec::new(),
            pinned_summary: None,
        }
    }

    #[tokio::test]
    async fn inflight_checkpoint_roundtrips_and_is_overwritten() {
        let (pool, _dir) = test_pool().await;
        let persister = SqliteSessionPersister::new(pool);

        assert!(persister.load_inflight("s1").await.unwrap().is_none());

        let checkpoint = InflightCheckpoint {
            session_id: "s1".to_string(),
            turn_index: 3,
            reason: "crashed".to_string(),
            staged: staged(),
        };
        persister.save_inflight(&checkpoint).await.unwrap();

        let back = persister.load_inflight("s1").await.unwrap().unwrap();
        assert_eq!(back.turn_index, 3);
        assert_eq!(back.reason, "crashed");
        assert_eq!(back.staged.len(), 2);
        assert_eq!(*back.staged[0].message, user("跑个长任务"));

        // 覆盖式：同一会话只保留最新一份（一批工具写一次，不能堆积）
        let newer = InflightCheckpoint {
            reason: "crashed".to_string(),
            turn_index: 4,
            staged: staged()[..1].to_vec(),
            ..checkpoint
        };
        persister.save_inflight(&newer).await.unwrap();
        let back = persister.load_inflight("s1").await.unwrap().unwrap();
        assert_eq!(back.turn_index, 4);
        assert_eq!(back.staged.len(), 1);

        persister.delete_inflight("s1").await.unwrap();
        assert!(persister.load_inflight("s1").await.unwrap().is_none());
        // 幂等：无对应行同样成功
        persister.delete_inflight("s1").await.unwrap();
    }

    #[tokio::test]
    async fn hydrate_frozen_turn_survives_reload() {
        let (pool, _dir) = test_pool().await;
        let persister = SqliteSessionPersister::new(pool);
        persister
            .save_inflight(&InflightCheckpoint {
                session_id: "s1".to_string(),
                turn_index: 0,
                reason: "crashed".to_string(),
                staged: staged(),
            })
            .await
            .unwrap();

        let mut session = Session::new("s1".to_string(), "d".to_string());
        hydrate_inflight_turn(&persister, &mut session).await;

        // 冻结入史：内容 + 补齐的悬空 tool_call + 中断说明
        let turn = &session.committed_turns()[0];
        assert_eq!(turn.len(), 4);
        assert_eq!(session.turn_index(), 1);
        // 检查点已消费 —— 再水化一次不得重复冻结
        assert!(persister.load_inflight("s1").await.unwrap().is_none());

        // 落盘：重载后这一轮仍在（用户可能只看一眼不发消息）
        let (reloaded, _meta) = persister.load("s1").await.unwrap().unwrap();
        assert_eq!(reloaded.committed_turns.len(), 1);
        assert!(matches!(
            reloaded.committed_turns[0].last().unwrap().source,
            MessageSource::InterruptedTurn { .. }
        ));

        let mut again = Session::from_snapshot("s1".to_string(), "d".to_string(), 1, reloaded);
        hydrate_inflight_turn(&persister, &mut again).await;
        assert_eq!(again.committed_turns().len(), 1, "重复水化不得再冻结一轮");
    }

    #[tokio::test]
    async fn stale_checkpoint_is_discarded() {
        // 收尾已跑完但删除失败的残留行：轮次编号小于会话当前值
        let (pool, _dir) = test_pool().await;
        let persister = SqliteSessionPersister::new(pool);
        persister
            .save_inflight(&InflightCheckpoint {
                session_id: "s1".to_string(),
                turn_index: 0,
                reason: "crashed".to_string(),
                staged: staged(),
            })
            .await
            .unwrap();

        let mut session =
            Session::from_snapshot("s1".to_string(), "d".to_string(), 1, snapshot_with(5));
        hydrate_inflight_turn(&persister, &mut session).await;

        assert!(session.committed_turns().is_empty(), "陈旧检查点不得入史");
        assert!(persister.load_inflight("s1").await.unwrap().is_none());
    }
}
