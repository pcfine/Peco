// ============================================================================
// FileSessionPersister — 基于 JSON 文件的会话持久化实现
// ============================================================================
//
// 每个会话存储为 `{base_dir}/{session_id}.json` 下的独立 JSON 文件。
// 使用 v4 格式（仅 committed turns + 计数器 + pending，不含 state/staging）。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::format::SessionFile;
use super::traits::{PersistError, PersistResult, SessionPersister};
use crate::session::{SessionMeta, SessionSnapshot};

/// 基于 JSON 文件的 [`SessionPersister`] 实现。
///
/// # 文件格式 (v4)
///
/// ```text
/// {base_dir}/
///   550e8400-e29b-41d4-a716-446655440000.json
///   6ba7b810-9dad-11d1-80b4-00c04fd430c8.json
///   ...
/// ```
///
/// # 线程安全
///
/// 使用 [`tokio::sync::RwLock`] 允许并发读，写操作序列化。
pub struct FileSessionPersister {
    base_dir: PathBuf,
    write_lock: RwLock<()>,
}

impl FileSessionPersister {
    /// 在 `base_dir` 下创建一个新的文件会话持久化器。
    ///
    /// 如果目录（及其父目录）不存在则自动创建。
    pub async fn new(base_dir: PathBuf) -> Result<Self, std::io::Error> {
        tokio::fs::create_dir_all(&base_dir).await?;
        Ok(Self {
            base_dir,
            write_lock: RwLock::new(()),
        })
    }

    /// 从环境变量创建持久化器：
    /// - `PECO_SESSIONS_DIR` — 自定义存储目录
    /// - 未设置时默认使用 `$HOME/.peco/sessions/`
    pub async fn from_env() -> Result<Self, anyhow::Error> {
        let dir = std::env::var("PECO_SESSIONS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".peco").join("sessions")
            });
        Ok(Self::new(dir).await?)
    }

    /// 返回存储目录的引用。
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// 计算给定 session_id 的磁盘文件路径。
    fn file_path(&self, session_id: &str) -> PathBuf {
        let filename = format!("{}.json", sanitize_session_id(session_id));
        self.base_dir.join(filename)
    }
}

#[async_trait]
impl SessionPersister for FileSessionPersister {
    async fn save(
        &self,
        snapshot: &SessionSnapshot,
        session_id: &str,
        description: &str,
        created_at: u64,
    ) -> Result<PersistResult, PersistError> {
        let _guard = self.write_lock.write().await;

        let path = self.file_path(session_id);

        // ── 增量持久化：尝试加载已有文件，仅追加新 turn ────────────────
        // 注意：compaction 会物理修剪 committed_turns 且不回退 turn_index 计数器
        // （计数器是"历史轮数"语义），因此：
        //   1. 追加边界必须用「位置」而非 turn_index —— 否则压缩后的首次追加
        //      会以越界切片 panic；
        //   2. 检测到收缩（文件轮数 > 快照轮数）时必须全量重写，
        //      否则增量追加会复活被驱逐的轮次。
        // 前提：committed 内 turn_index 单调连续且从 0 起（由 Session::compact
        // 重编号保证），文件内容是快照 committed_turns 的前缀。
        let existing = if path.exists() {
            read_session_file(&path).await.ok().flatten()
        } else {
            None
        };
        let existing_len = existing
            .as_ref()
            .map_or(0, |(e, _)| e.committed_turns.len());
        let shrank = existing_len > snapshot.committed_turns.len();
        let committed_turns = match existing {
            Some((mut existing_snapshot, _existing_meta)) if !shrank => {
                if existing_len < snapshot.committed_turns.len() {
                    // 追加新 turn：保留已有前缀，从新 snapshot 中按位置取增量
                    let new_turns = &snapshot.committed_turns[existing_len..];
                    tracing::debug!(
                        session_id = %session_id,
                        existing = existing_len,
                        appended = new_turns.len(),
                        "Incremental persistence: appending turns"
                    );
                    existing_snapshot
                        .committed_turns
                        .extend(new_turns.iter().cloned());
                    existing_snapshot.committed_turns
                } else {
                    // 轮数未变化 — 保留已有（幂等）
                    existing_snapshot.committed_turns
                }
            }
            _ => {
                // 文件不存在/损坏，或 compaction 收缩 — 全量写入
                snapshot.committed_turns.clone()
            }
        };

        // 构造 SessionMeta 的动态字段
        let meta = SessionMeta {
            id: session_id.to_string(),
            description: description.to_string(),
            created_at,
            updated_at: unix_timestamp_now(),
            tokens_used: snapshot.total_usage.total_tokens as u64,
            completed_turns: committed_turns.len(),
        };

        let file = SessionFile {
            format_version: 4,
            meta,
            committed_turns,
            turn_index: snapshot.turn_index,
            total_usage: snapshot.total_usage.clone(),
            next_message_id: snapshot.next_message_id,
            pending_inputs: snapshot.pending_inputs.clone(),
            pinned_summary: snapshot.pinned_summary.clone(),
        };

        let json = serde_json::to_string_pretty(&file)?;
        let bytes = json.len() as u64;
        tokio::fs::write(&path, json).await?;

        Ok(PersistResult {
            bytes_written: bytes,
            path,
        })
    }

    async fn load(
        &self,
        session_id: &str,
    ) -> Result<Option<(SessionSnapshot, SessionMeta)>, PersistError> {
        validate_session_id(session_id)?;

        let _guard = self.write_lock.read().await;

        read_session_file(&self.file_path(session_id)).await
    }

    async fn delete(&self, session_id: &str) -> Result<(), PersistError> {
        validate_session_id(session_id)?;

        let _guard = self.write_lock.write().await;
        let path = self.file_path(session_id);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PersistError::Io(e)),
        }
    }

    async fn list(&self) -> Result<Vec<SessionMeta>, PersistError> {
        let _guard = self.write_lock.read().await;

        let mut entries = tokio::fs::read_dir(&self.base_dir).await?;
        let mut metas = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json")
                && let Ok(content) = tokio::fs::read_to_string(&path).await
                && let Ok(file) = serde_json::from_str::<SessionFile>(&content)
                && file.format_version == 4
            {
                metas.push(file.meta);
            }
        }

        // 按 updated_at 降序排列
        metas.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(metas)
    }
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 从路径读取并解析 v4 SessionFile（无锁、无校验）。
async fn read_session_file(
    path: &Path,
) -> Result<Option<(SessionSnapshot, SessionMeta)>, PersistError> {
    let raw = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(PersistError::Io(e)),
    };

    let file: SessionFile = serde_json::from_str(&raw).map_err(|_| PersistError::UnknownFormat)?;

    if file.format_version != 4 {
        return Err(PersistError::UnsupportedFormatVersion(file.format_version));
    }

    let snapshot = SessionSnapshot {
        committed_turns: file.committed_turns,
        turn_index: file.turn_index,
        total_usage: file.total_usage,
        next_message_id: file.next_message_id,
        pending_inputs: file.pending_inputs,
        pinned_summary: file.pinned_summary,
    };

    Ok(Some((snapshot, file.meta)))
}

/// 获取当前 Unix 时间戳（秒）。
fn unix_timestamp_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 验证会话 ID 合法性。
fn validate_session_id(id: &str) -> Result<(), PersistError> {
    if id.is_empty() {
        return Err(PersistError::InvalidId(
            "session ID must not be empty".into(),
        ));
    }
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err(PersistError::InvalidId(format!(
            "session ID contains invalid characters: {id}"
        )));
    }
    Ok(())
}

/// 将会话 ID 清理为安全的文件名组件。
fn sanitize_session_id(id: &str) -> String {
    let sanitized: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(128)
        .collect();

    if sanitized.is_empty() || sanitized.chars().all(|c| c == '_') {
        "unnamed".to_string()
    } else {
        sanitized
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{AnnotatedMessage, MessageId, MessageSource};
    use model_provider::{InputItem, Role, Usage};

    fn user(text: impl Into<String>) -> InputItem {
        InputItem::Message {
            role: Role::User,
            content: text.into(),
        }
    }

    fn temp_dir(test_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "peco-persist-test-{}-{}",
            std::process::id(),
            test_name
        ))
    }

    async fn setup(test_name: &str) -> FileSessionPersister {
        let dir = temp_dir(test_name);
        let _ = tokio::fs::remove_dir_all(&dir).await;
        FileSessionPersister::new(dir)
            .await
            .expect("create temp dir")
    }

    async fn teardown(persister: &FileSessionPersister) {
        let _ = tokio::fs::remove_dir_all(persister.base_dir()).await;
    }

    fn make_snapshot() -> SessionSnapshot {
        SessionSnapshot {
            committed_turns: vec![vec![crate::session::AnnotatedMessage::new(
                crate::session::MessageId(0),
                0,
                user("hello"),
                crate::session::MessageSource::UserInput,
            )]],
            turn_index: 1,
            total_usage: Usage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
            },
            next_message_id: 2,
            pending_inputs: Vec::new(),
            pinned_summary: None,
        }
    }

    #[tokio::test]
    async fn test_save_and_load_roundtrip() {
        let p = setup("roundtrip").await;
        let snap = make_snapshot();
        let id = "test-session-1";

        // Save
        let result = p.save(&snap, id, "test desc", 1000).await.unwrap();
        assert!(result.bytes_written > 0);
        assert!(result.path.exists());

        // Load
        let (loaded_snap, meta) = p.load(id).await.unwrap().expect("should exist");
        assert_eq!(loaded_snap.turn_index, 1);
        assert_eq!(loaded_snap.committed_turns.len(), 1);
        assert_eq!(loaded_snap.total_usage.total_tokens, 30);
        assert_eq!(loaded_snap.next_message_id, 2);

        // Meta computed correctly
        assert_eq!(meta.id, id);
        assert_eq!(meta.description, "test desc");
        assert_eq!(meta.created_at, 1000);
        assert_eq!(meta.tokens_used, 30);
        assert_eq!(meta.completed_turns, 1);
        assert!(meta.updated_at > 0);

        teardown(&p).await;
    }

    #[tokio::test]
    async fn test_load_missing_returns_none() {
        let p = setup("missing").await;
        let result = p.load("nonexistent").await.unwrap();
        assert!(result.is_none());
        teardown(&p).await;
    }

    #[tokio::test]
    async fn test_delete_removes_file() {
        let p = setup("delete").await;
        let snap = make_snapshot();
        let id = "to-delete";
        p.save(&snap, id, "desc", 1000).await.unwrap();

        p.delete(id).await.unwrap();
        let result = p.load(id).await.unwrap();
        assert!(result.is_none());

        // Delete non-existent is ok
        p.delete(id).await.unwrap();

        teardown(&p).await;
    }

    #[tokio::test]
    async fn test_list_returns_sorted() {
        let p = setup("list").await;
        let snap = make_snapshot();

        // Save two sessions
        p.save(&snap, "first", "first desc", 1000).await.unwrap();
        // Small delay so timestamps differ
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        p.save(&snap, "second", "second desc", 2000).await.unwrap();

        let list = p.list().await.unwrap();
        assert_eq!(list.len(), 2);
        // Most recently updated should be first
        assert!(list[0].updated_at >= list[1].updated_at);

        teardown(&p).await;
    }

    #[tokio::test]
    async fn test_incremental_save_appends_turns() {
        let p = setup("incremental").await;
        let id = "incr-session";

        // Save turn 0
        let snap0 = SessionSnapshot {
            committed_turns: vec![vec![AnnotatedMessage::new(
                MessageId(0),
                0,
                user("turn0"),
                MessageSource::UserInput,
            )]],
            turn_index: 1,
            total_usage: Usage {
                input_tokens: 5,
                output_tokens: 10,
                total_tokens: 15,
            },
            next_message_id: 1,
            pending_inputs: Vec::new(),
            pinned_summary: None,
        };
        p.save(&snap0, id, "desc", 1000).await.unwrap();

        // Save turn 1 (should append incrementally)
        let snap1 = SessionSnapshot {
            committed_turns: vec![
                vec![AnnotatedMessage::new(
                    MessageId(0),
                    0,
                    user("turn0"),
                    MessageSource::UserInput,
                )],
                vec![AnnotatedMessage::new(
                    MessageId(1),
                    1,
                    user("turn1"),
                    MessageSource::UserInput,
                )],
            ],
            turn_index: 2,
            total_usage: Usage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
            },
            next_message_id: 2,
            pending_inputs: Vec::new(),
            pinned_summary: None,
        };
        p.save(&snap1, id, "desc", 1000).await.unwrap();

        // Load and verify both turns are present
        let (loaded, meta) = p.load(id).await.unwrap().expect("should exist");
        assert_eq!(loaded.committed_turns.len(), 2);
        assert_eq!(loaded.turn_index, 2);
        assert_eq!(meta.completed_turns, 2);
        assert_eq!(meta.tokens_used, 30);

        teardown(&p).await;
    }

    #[tokio::test]
    async fn test_save_full_rewrite_on_compaction_shrink() {
        // compaction 后 turn_index 不变而 committed_turns 收缩 —
        // 增量追加逻辑必须检测到收缩并全量重写，否则被驱逐轮次会复活。
        let p = setup("shrink").await;
        let id = "shrink-session";

        // 先保存 2 轮
        let snap_full = SessionSnapshot {
            committed_turns: vec![
                vec![AnnotatedMessage::new(
                    MessageId(0),
                    0,
                    user("turn0"),
                    MessageSource::UserInput,
                )],
                vec![AnnotatedMessage::new(
                    MessageId(1),
                    1,
                    user("turn1"),
                    MessageSource::UserInput,
                )],
            ],
            turn_index: 2,
            total_usage: Usage::default(),
            next_message_id: 2,
            pending_inputs: Vec::new(),
            pinned_summary: None,
        };
        p.save(&snap_full, id, "desc", 1000).await.unwrap();

        // 模拟 compaction：驱逐 turn0，钉扎摘要（turn_index 仍为 2）
        let snap_compacted = SessionSnapshot {
            committed_turns: vec![vec![AnnotatedMessage::new(
                MessageId(1),
                0,
                user("turn1"),
                MessageSource::UserInput,
            )]],
            turn_index: 2,
            total_usage: Usage::default(),
            next_message_id: 3,
            pending_inputs: Vec::new(),
            pinned_summary: Some(AnnotatedMessage::new(
                MessageId(2),
                0,
                InputItem::Message {
                    role: Role::System,
                    content: "summary".to_string(),
                },
                MessageSource::SystemInjection {
                    reason: "compaction".to_string(),
                },
            )),
        };
        p.save(&snap_compacted, id, "desc", 1000).await.unwrap();

        let (loaded, meta) = p.load(id).await.unwrap().expect("should exist");
        assert_eq!(loaded.committed_turns.len(), 1, "被驱逐轮次不得复活");
        assert_eq!(loaded.committed_turns[0][0].turn_index, 0);
        assert!(loaded.pinned_summary.is_some());
        assert_eq!(meta.completed_turns, 1);

        teardown(&p).await;
    }

    #[tokio::test]
    async fn test_incremental_append_after_compaction() {
        // compaction 后 turn_index 计数器不再等于 committed_turns.len()，
        // 增量追加必须按「位置」取边界 —— 旧实现用 turn_index 切片，
        // 压缩后的首次追加会以越界切片 panic（committed_turns[2..3]，len=2）。
        let p = setup("append-after-compact").await;
        let id = "append-after-compact-session";

        // 1. 保存 2 轮（turn_index == 2 == 轮数，正常状态）
        let snap_full = SessionSnapshot {
            committed_turns: vec![
                vec![AnnotatedMessage::new(
                    MessageId(0),
                    0,
                    user("turn0"),
                    MessageSource::UserInput,
                )],
                vec![AnnotatedMessage::new(
                    MessageId(1),
                    1,
                    user("turn1"),
                    MessageSource::UserInput,
                )],
            ],
            turn_index: 2,
            total_usage: Usage::default(),
            next_message_id: 2,
            pending_inputs: Vec::new(),
            pinned_summary: None,
        };
        p.save(&snap_full, id, "desc", 1000).await.unwrap();

        // 2. compaction：驱逐 turn0（turn_index 仍为 2，轮数变为 1）
        let snap_compacted = SessionSnapshot {
            committed_turns: vec![vec![AnnotatedMessage::new(
                MessageId(1),
                0,
                user("turn1"),
                MessageSource::UserInput,
            )]],
            turn_index: 2,
            total_usage: Usage::default(),
            next_message_id: 3,
            pending_inputs: Vec::new(),
            pinned_summary: Some(AnnotatedMessage::new(
                MessageId(2),
                0,
                InputItem::Message {
                    role: Role::System,
                    content: "summary".to_string(),
                },
                MessageSource::SystemInjection {
                    reason: "compaction".to_string(),
                },
            )),
        };
        p.save(&snap_compacted, id, "desc", 1000).await.unwrap();

        // 3. 压缩后新增一轮：轮数 2，但 turn_index 计数器为 3（历史轮数语义）
        //    新 turn 的消息 turn_index == 2（计数器），与其 committed 位置（1）不等
        let snap_new_turn = SessionSnapshot {
            committed_turns: vec![
                vec![AnnotatedMessage::new(
                    MessageId(1),
                    0,
                    user("turn1"),
                    MessageSource::UserInput,
                )],
                vec![AnnotatedMessage::new(
                    MessageId(3),
                    2,
                    user("turn2"),
                    MessageSource::UserInput,
                )],
            ],
            turn_index: 3,
            total_usage: Usage::default(),
            next_message_id: 4,
            pending_inputs: Vec::new(),
            pinned_summary: snap_compacted.pinned_summary.clone(),
        };
        p.save(&snap_new_turn, id, "desc", 1000).await.unwrap();

        let (loaded, meta) = p.load(id).await.unwrap().expect("should exist");
        assert_eq!(
            loaded.committed_turns.len(),
            2,
            "压缩后的新 turn 必须被追加（且不 panic）"
        );
        assert_eq!(
            loaded.committed_turns[0][0].message.as_ref(),
            &InputItem::Message {
                role: Role::User,
                content: "turn1".to_string()
            }
        );
        assert_eq!(
            loaded.committed_turns[1][0].message.as_ref(),
            &InputItem::Message {
                role: Role::User,
                content: "turn2".to_string()
            }
        );
        assert!(loaded.pinned_summary.is_some(), "pinned 摘要不得丢失");
        assert_eq!(meta.completed_turns, 2);

        teardown(&p).await;
    }

    #[tokio::test]
    async fn test_incremental_save_idempotent() {
        let p = setup("idempotent").await;
        let id = "idem-session";

        let snap = make_snapshot();
        // Save same snapshot twice — should not duplicate turns
        p.save(&snap, id, "desc", 1000).await.unwrap();
        p.save(&snap, id, "desc", 1000).await.unwrap();

        let (loaded, _meta) = p.load(id).await.unwrap().expect("should exist");
        assert_eq!(loaded.committed_turns.len(), 1); // Still 1, not duplicated

        teardown(&p).await;
    }

    #[tokio::test]
    async fn test_validate_rejects_bad_ids() {
        let p = setup("validate").await;
        assert!(p.load("../bad").await.is_err());
        assert!(p.load("").await.is_err());
        teardown(&p).await;
    }

    #[test]
    fn test_sanitize_session_id() {
        assert_eq!(sanitize_session_id("hello-world_123"), "hello-world_123");
        assert!(!sanitize_session_id("../../etc/passwd").contains('/'));
        assert_eq!(sanitize_session_id(""), "unnamed");
        assert_eq!(sanitize_session_id("!!!@@@"), "unnamed");
        let long = "a".repeat(200);
        assert_eq!(sanitize_session_id(&long).len(), 128);
    }
}
