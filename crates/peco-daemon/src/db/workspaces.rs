//! workspaces 表 DAO —— 空间行的 CRUD 与磁盘普查。
//!
//! 本表是可重建的索引：`ws_id` 由 `.workspace.json` 的会话 id 派生，团队以
//! 记录里的团队路径为准，运行属性（`label` / `exit_policy` / 打开关闭时间）以本表为准。

use std::path::Path;

use sqlx::SqlitePool;

use crate::db::teams::{self, NewTeam};
use crate::error::DaemonError;
use crate::space::manifest;

/// 单空间退出策略的默认值：跟随全局。
pub const EXIT_POLICY_INHERIT: &str = "inherit";

/// workspaces 表完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct WorkspaceRow {
    pub id: String,
    pub work_dir: String,
    pub team_id: String,
    pub label: Option<String>,
    pub exit_policy: String,
    pub created_at: String,
    pub last_opened_at: Option<String>,
    pub closed_at: Option<String>,
}

/// 写入一行的入参。
pub struct NewWorkspace<'a> {
    pub id: &'a str,
    pub work_dir: &'a str,
    pub team_id: &'a str,
    pub label: Option<&'a str>,
    pub exit_policy: &'a str,
}

/// `INSERT ... ON CONFLICT(work_dir) DO NOTHING`：返回 1 表示本行是这次写入的。
///
/// 并发的两次「按 work_dir 建空间」必须收敛到同一个 `ws_id` —— 这是
/// per-ws_id 门闩能生效的前提（锁键先于锁确定）。
pub async fn insert_if_absent(
    pool: &SqlitePool,
    space: &NewWorkspace<'_>,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO workspaces (id, work_dir, team_id, label, exit_policy) \
         VALUES (?, ?, ?, ?, ?) ON CONFLICT(work_dir) DO NOTHING",
    )
    .bind(space.id)
    .bind(space.work_dir)
    .bind(space.team_id)
    .bind(space.label)
    .bind(space.exit_policy)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn by_work_dir(
    pool: &SqlitePool,
    work_dir: &str,
) -> Result<Option<WorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkspaceRow>(
        "SELECT id, work_dir, team_id, label, exit_policy, created_at, last_opened_at, closed_at \
         FROM workspaces WHERE work_dir = ?",
    )
    .bind(work_dir)
    .fetch_optional(pool)
    .await
}

pub async fn by_id(pool: &SqlitePool, id: &str) -> Result<Option<WorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkspaceRow>(
        "SELECT id, work_dir, team_id, label, exit_policy, created_at, last_opened_at, closed_at \
         FROM workspaces WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// 同团队里仍未关闭的空间目录（`closed_at IS NULL`）。
///
/// 只取目录：调用方要的是「哪些实例可能还活着」，行里的其它列由实例表
/// （`WorkspaceHub`）决定，多读一遍就会有两份可能不一致的答案。
pub async fn open_work_dirs_by_team(
    pool: &SqlitePool,
    team_id: &str,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT work_dir FROM workspaces WHERE team_id = ? AND closed_at IS NULL",
    )
    .bind(team_id)
    .fetch_all(pool)
    .await
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<WorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkspaceRow>(
        "SELECT id, work_dir, team_id, label, exit_policy, created_at, last_opened_at, closed_at \
         FROM workspaces ORDER BY created_at, id",
    )
    .fetch_all(pool)
    .await
}

/// 标记一次成功打开；同时清掉 `closed_at`（重新打开即复活）。
pub async fn touch_opened(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE workspaces SET last_opened_at = datetime('now'), closed_at = NULL WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_team(pool: &SqlitePool, id: &str, team_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE workspaces SET team_id = ? WHERE id = ?")
        .bind(team_id)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_label(
    pool: &SqlitePool,
    id: &str,
    label: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE workspaces SET label = ? WHERE id = ?")
        .bind(label)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_exit_policy(
    pool: &SqlitePool,
    id: &str,
    exit_policy: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE workspaces SET exit_policy = ? WHERE id = ?")
        .bind(exit_policy)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_closed_at(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE workspaces SET closed_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// 硬删空间行（`purge`）。台账行不随之删除 —— 那是另一张表。
pub async fn delete(pool: &SqlitePool, id: &str) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM workspaces WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// 普查结果 —— CLI `ws rescan --json` 的输出形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RescanReport {
    pub teams: usize,
    pub spaces: usize,
}

/// 扫 `$PECO_HOME/teams/*` 与 `$PECO_HOME/spaces/*/work`，把磁盘现状补进索引。
///
/// 只补不删：目录消失不等于空间该被删（台账与审计的生命周期长于目录），
/// 清理入口是 `purge`。
pub async fn rescan(pool: &SqlitePool, home: &Path) -> Result<RescanReport, DaemonError> {
    let mut report = RescanReport {
        teams: 0,
        spaces: 0,
    };

    for dir in subdirs(&home.join("teams")) {
        let team_id = entry_name(&dir);
        let (display_name, template_version) = teams::meta_from_dir(&dir, &team_id);
        teams::upsert(
            pool,
            &NewTeam {
                id: &team_id,
                display_name: &display_name,
                template_version,
            },
        )
        .await?;
        report.teams += 1;
    }

    for dir in subdirs(&home.join("spaces")) {
        let work_dir = dir.join("work");
        let value = match manifest::read(&work_dir) {
            Ok(Some(value)) => value,
            Ok(None) => {
                tracing::debug!(
                    space_dir = %dir.display(),
                    "space dir has no marker; leaving it unindexed"
                );
                continue;
            }
            Err(e) => {
                tracing::warn!(
                    space_dir = %dir.display(),
                    code = e.code(),
                    error = %e,
                    "marker unusable; leaving space dir unindexed"
                );
                continue;
            }
        };
        let team_id = match manifest::team_id_of(&value) {
            Ok(team_id) => team_id,
            Err(e) => {
                tracing::warn!(
                    space_dir = %dir.display(),
                    error = %e,
                    "marker has no usable team id; leaving space dir unindexed"
                );
                continue;
            }
        };
        let ws_id = manifest::ws_id_of(&value);
        let work_dir = work_dir.to_string_lossy().into_owned();
        insert_if_absent(
            pool,
            &NewWorkspace {
                id: &ws_id,
                work_dir: &work_dir,
                team_id: &team_id,
                label: Some(&team_id),
                exit_policy: EXIT_POLICY_INHERIT,
            },
        )
        .await?;
        report.spaces += 1;
    }

    Ok(report)
}

fn subdirs(root: &Path) -> Vec<std::path::PathBuf> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(root = %root.display(), error = %e, "cannot list directory");
            return Vec::new();
        }
    };
    let mut dirs: Vec<_> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    dirs
}

fn entry_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 某团队绑定的全部空间（含已关闭），按创建序。
///
/// 团队页要列「绑定的空间」，已关闭的也要在：关闭只代表实例不再打开，
/// 台账与历史仍然指向它。
pub async fn list_by_team(
    pool: &SqlitePool,
    team_id: &str,
) -> Result<Vec<WorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkspaceRow>(
        "SELECT id, work_dir, team_id, label, exit_policy, created_at, last_opened_at, closed_at \
         FROM workspaces WHERE team_id = ? ORDER BY created_at, id",
    )
    .bind(team_id)
    .fetch_all(pool)
    .await
}
