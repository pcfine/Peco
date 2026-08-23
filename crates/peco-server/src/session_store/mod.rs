// ============================================================================
// SqliteSessionPersister — SQLite 版 Session 持久化
// ============================================================================
//
// 实现 peco_core::persistence::SessionPersister trait，
// 将 SessionSnapshot 以 JSON 格式存入 session_snapshots 表，
// SessionMeta 动态字段由 SqliteSessionPersister 自行计算。

use std::path::PathBuf;

use async_trait::async_trait;
use peco_core::persistence::{PersistError, PersistResult, SessionPersister};
use peco_core::session::{SessionMeta, SessionSnapshot};
use sqlx::SqlitePool;

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
                            "会话快照反序列化失败（可能是旧格式/损坏数据），无法恢复历史会话"
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
}
