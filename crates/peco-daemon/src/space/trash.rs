//! 空间回收站：移入、恢复、永久销毁。
//!
//! 移入回收站只摘牌：置 `deleted_at`、删 `.workspace.json`，会话与记忆原样留着
//! —— 这就是「删除可以撤销」的全部含义。永久销毁才会删会话与记忆，而**工作目录
//! 里的用户文件在任何一条路径上都不删**（唯一允许删的目录内文件是记录文件本身）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use peco_core::knowledge::{KnowledgeManager, KnowledgeModuleError};
use peco_core::tools::WorkspaceHub;
use sqlx::SqliteConnection;
use tracing::{info, warn};

use crate::db::conversations;
use crate::db::workspaces::{self, WorkspaceRow};
use crate::error::DaemonError;
use crate::memory::{MEMORY_SCAN_LIMIT, config::MemoryConfig, layout};
use crate::space::SpaceEnv;
use crate::space::close::{CloseOutcome, pass_gate};
use crate::space::manifest;
use crate::space::open::map_tool_error;

/// 回收站上限（全局）。超出时同一请求内销毁最旧的一条。
pub const TRASH_CAPACITY: i64 = 100;

/// 一次「移入回收站」的结果。
#[derive(Debug, Default, Clone, Copy)]
pub struct TrashOutcome {
    /// 本次因超出上限而被销毁的空间数（被销毁的是谁只进日志）。
    pub evicted: usize,
}

/// 移入回收站：置 `deleted_at`、摘 marker，会话与记忆不动。
pub async fn trash_workspace(
    env: &SpaceEnv,
    hub: &Arc<WorkspaceHub>,
    ws_id: &str,
    force: bool,
) -> Result<TrashOutcome, DaemonError> {
    // 保留空间不能删，回收站是删除的第一步，所以这道判定在这里同样成立。
    if env.peco.is_peco_space(ws_id) {
        return Err(DaemonError::ReservedSpace {
            ws_id: ws_id.to_string(),
        });
    }
    let row = by_id(env, ws_id).await?;
    // 重复删除是同一件事：第二次不再过门槛，也不再动实例表。
    if row.is_trashed() {
        return Ok(TrashOutcome::default());
    }

    let (_inflight, _orphaned) = pass_gate(env, ws_id, force).await?;
    let dir = PathBuf::from(&row.work_dir);
    hub.begin_close(&dir)
        .await
        .map_err(|e| map_tool_error(e, ws_id))?;

    // 顺序不可换：先置 deleted_at（此后打开路径一律 409 `space_trashed`），
    // 再摘 marker。反过来会留下「marker 没了但行还是活的」窗口，此时 rescan
    // 或并发打开都能把这个空间复活成另一个身份。
    let result = async {
        workspaces::set_deleted_at(&env.pool, ws_id)
            .await
            .map_err(DaemonError::from)?;
        manifest::remove(&dir)
    }
    .await;
    hub.finish_close(&dir).await;
    result?;

    let evicted = enforce_capacity(env).await?;
    info!(ws_id, force, evicted, "space moved to trash");
    Ok(TrashOutcome { evicted })
}

/// 永久销毁：会话、记忆、marker 与空间行都删掉，工作目录内容与台账留着。
pub async fn purge_workspace(
    env: &SpaceEnv,
    hub: &Arc<WorkspaceHub>,
    ws_id: &str,
    force: bool,
) -> Result<CloseOutcome, DaemonError> {
    // 判定先于查行：保留 id 无论索引在不在都不该被删。
    if env.peco.is_peco_space(ws_id) {
        return Err(DaemonError::ReservedSpace {
            ws_id: ws_id.to_string(),
        });
    }
    let row = by_id(env, ws_id).await?;
    let (inflight, orphaned) = pass_gate(env, ws_id, force).await?;

    let dir = PathBuf::from(&row.work_dir);
    hub.begin_close(&dir)
        .await
        .map_err(|e| map_tool_error(e, ws_id))?;

    let result = destroy_space(env, &row).await;
    hub.finish_close(&dir).await;
    result?;

    info!(
        ws_id,
        force, orphaned, "space purged; ledger and work_dir files kept"
    );
    Ok(CloseOutcome {
        inflight,
        orphaned,
        purged: true,
    })
}

/// 恢复：出回收站。恢复后仍是「已关闭」，打开是用户的下一件事。
pub async fn restore_workspace(env: &SpaceEnv, ws_id: &str) -> Result<bool, DaemonError> {
    let row = by_id(env, ws_id).await?;
    // 幂等：对活跃行调用恢复返回成功，什么都不改。
    if !row.is_trashed() {
        return Ok(false);
    }

    let work_dir = PathBuf::from(&row.work_dir);
    if !work_dir.is_dir() {
        return Err(DaemonError::WorkDirMissing {
            work_dir: row.work_dir.clone(),
        });
    }
    // 目录可能已被别的空间占用：marker 里的身份与这一行不同，恢复会让两个身份
    // 抢同一个目录，此时只能报冲突。
    match manifest::read(&work_dir)? {
        Some(found) => {
            let marker_ws_id = manifest::ws_id_of(&found);
            if marker_ws_id != row.id {
                return Err(DaemonError::WorkDirReused {
                    work_dir: row.work_dir.clone(),
                });
            }
        }
        None => {
            // 会话 id 由 ws_id 派生，重建的身份因此与删除前是同一个。
            let name = display_name(&row);
            manifest::ensure_for(&work_dir, &row.id, &env.team_root(&row.team_id), &name)?;
        }
    }

    // `closed_at` 保持原值：恢复的是索引，不是运行状态。
    workspaces::clear_deleted_at(&env.pool, ws_id).await?;
    info!(ws_id, "space restored from trash");
    Ok(true)
}

/// marker 缺失时的展示名：`label` → 目录末段 → `team_id`。
///
/// 三处都取不到（目录是根、label 为空）才退回 `team_id` —— 名字只用于展示，
/// 但回收站里一列空名字等于让用户没法判断该恢复哪一个。
pub fn display_name(row: &WorkspaceRow) -> String {
    let from_dir = Path::new(&row.work_dir)
        .file_name()
        .and_then(|segment| segment.to_str())
        .map(str::to_string);
    row.label
        .clone()
        .or(from_dir)
        .unwrap_or_else(|| row.team_id.clone())
}

async fn by_id(env: &SpaceEnv, ws_id: &str) -> Result<WorkspaceRow, DaemonError> {
    workspaces::by_id(&env.pool, ws_id)
        .await?
        .ok_or_else(|| DaemonError::SpaceNotFound {
            ws_id: ws_id.to_string(),
        })
}

/// 销毁一个空间行已经确定的全部数据。
async fn destroy_space(env: &SpaceEnv, row: &WorkspaceRow) -> Result<(), DaemonError> {
    // 会话历史走一个事务：半删会留下「消息还在、会话没了」的孤儿行。
    let mut tx = env.pool.begin().await?;
    conversations::delete_space_history(&mut tx, &row.id).await?;
    tx.commit().await?;

    // 记忆先逐条删（Helix 侧的文档只能一条条删），再整根删掉。
    cleanup_memory(&env.home, &row.id).await;
    manifest::remove(Path::new(&row.work_dir))?;
    workspaces::delete(&env.pool, &row.id).await?;
    Ok(())
}

/// 删掉一个空间的记忆：逐文档删除后整根删除。
///
/// 单条删不掉只 warn 并继续：销毁是用户明确要求的动作，卡在一条记忆上会让
/// 空间永远删不掉。本地目录随后一定被删，Helix 侧残留行留债（与 `delete_kb`
/// 对 Helix 只删本地目录同一笔）。
async fn cleanup_memory(home: &Path, ws_id: &str) {
    let root = layout::memory_root(home, ws_id);
    let kb_name = MemoryConfig::from_env().kb_name;
    let mut deleted = 0;

    // 没写过配置 = 这个空间从未建库，连模型都不必加载。
    if layout::kb_config_path(&root).is_file() {
        let km: Arc<KnowledgeManager> = layout::manager(&root, ws_id);
        match km
            .list_memory_documents_with_content(&kb_name, MEMORY_SCAN_LIMIT)
            .await
        {
            Ok(documents) => {
                for document in documents {
                    match km.delete_document(&kb_name, &document.id).await {
                        Ok(_) => deleted += 1,
                        Err(e) => warn!(
                            ws_id,
                            doc_id = %document.id,
                            error = %e,
                            "cannot delete a memory document; wiping the local library anyway"
                        ),
                    }
                }
            }
            Err(KnowledgeModuleError::NotFound(_)) => {}
            Err(e) => warn!(
                ws_id,
                kb = %kb_name,
                error = %e,
                "cannot list the memory library before deletion; wiping the local library anyway"
            ),
        }
    }

    match std::fs::remove_dir_all(&root) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(
            ws_id,
            root = %root.display(),
            error = %e,
            "cannot remove the memory directory; it is now unreachable but still on disk"
        ),
    }
    info!(ws_id, deleted, "space memory library deleted");
}

/// 回收站上限：超出时销毁最旧的若干条，返回被销毁的 `ws_id`。
///
/// 计数与销毁必须在同一把 `BEGIN IMMEDIATE` 里：两个并发删除各自读到 99 就会
/// 各插一条，上限变成 102 而没有人再来看第二眼。
async fn enforce_capacity(env: &SpaceEnv) -> Result<usize, DaemonError> {
    let mut conn = env.pool.acquire().await?;
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
    let doomed = match trim_to_capacity(&mut conn).await {
        Ok(doomed) => {
            sqlx::query("COMMIT").execute(&mut *conn).await?;
            doomed
        }
        Err(e) => {
            // 回滚失败只 warn：真正的错误是上面那个，别让它被覆盖掉。
            if let Err(rollback) = sqlx::query("ROLLBACK").execute(&mut *conn).await {
                warn!(error = %rollback, "cannot roll back the trash trim");
            }
            return Err(DaemonError::from(e));
        }
    };

    // 文件清理放在事务之后：删记忆要走网络与磁盘，占了事务会把整张表锁住。
    for (ws_id, work_dir) in &doomed {
        cleanup_memory(&env.home, ws_id).await;
        if let Err(e) = manifest::remove(Path::new(work_dir)) {
            warn!(ws_id, error = %e, "cannot remove the marker of an evicted space");
        }
        warn!(ws_id, "the trash is full; the oldest entry was destroyed");
    }
    Ok(doomed.len())
}

/// 在事务内把回收站裁到上限，返回被销毁的 `(ws_id, work_dir)`。
async fn trim_to_capacity(
    conn: &mut SqliteConnection,
) -> Result<Vec<(String, String)>, sqlx::Error> {
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workspaces WHERE deleted_at IS NOT NULL")
            .fetch_one(&mut *conn)
            .await?;
    if count <= TRASH_CAPACITY {
        return Ok(Vec::new());
    }
    let excess = count - TRASH_CAPACITY;
    let victims: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, work_dir FROM workspaces WHERE deleted_at IS NOT NULL \
         ORDER BY deleted_at, id LIMIT ?",
    )
    .bind(excess)
    .fetch_all(&mut *conn)
    .await?;

    for (ws_id, _) in &victims {
        conversations::delete_space_history(&mut *conn, ws_id).await?;
        sqlx::query("DELETE FROM workspaces WHERE id = ?")
            .bind(ws_id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(victims)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(label: Option<&str>, work_dir: &str, team: &str) -> WorkspaceRow {
        WorkspaceRow {
            id: "ws_a".into(),
            work_dir: work_dir.into(),
            team_id: team.into(),
            label: label.map(str::to_string),
            exit_policy: "inherit".into(),
            created_at: "2026-01-01 00:00:00".into(),
            last_opened_at: None,
            closed_at: None,
            deleted_at: None,
        }
    }

    /// 名字的三级回落：`label` 优先于目录末段（`/home/x/` 这种尾斜杠不该把名字
    /// 变成空串），全都取不到才退团队 id。
    #[test]
    fn display_name_falls_back_to_the_directory_then_the_team() {
        assert_eq!(display_name(&row(Some("研发"), "/w/a", "ops")), "研发");
        assert_eq!(display_name(&row(None, "/w/a", "ops")), "a");
        assert_eq!(display_name(&row(None, "/", "ops")), "ops");
    }

    /// 上限常量就是设计里的 100：改它是产品决定，不该悄悄漂移。
    #[test]
    fn capacity_is_the_documented_hundred() {
        assert_eq!(TRASH_CAPACITY, 100);
    }
}
