//! teams 表 DAO —— 能力根 `teams/<team_id>/` 的 DB 索引。
//!
//! 真源是目录：本表只在 `ws rescan` 与首次写入时补行，删团队没有入口。

use std::path::Path;

use sqlx::SqlitePool;

/// teams 表完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct TeamRow {
    pub id: String,
    pub display_name: String,
    pub template_version: i64,
    pub created_at: String,
}

/// 写入一行的入参。
pub struct NewTeam<'a> {
    pub id: &'a str,
    pub display_name: &'a str,
    pub template_version: i64,
}

/// 幂等写入：同 id 覆盖展示名与模板版本（模板升级后 rescan 才能反映真值）。
pub async fn upsert(pool: &SqlitePool, team: &NewTeam<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO teams (id, display_name, template_version) VALUES (?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET display_name = excluded.display_name, \
         template_version = excluded.template_version",
    )
    .bind(team.id)
    .bind(team.display_name)
    .bind(team.template_version)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<TeamRow>, sqlx::Error> {
    sqlx::query_as::<_, TeamRow>(
        "SELECT id, display_name, template_version, created_at FROM teams WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<TeamRow>, sqlx::Error> {
    sqlx::query_as::<_, TeamRow>(
        "SELECT id, display_name, template_version, created_at FROM teams ORDER BY id",
    )
    .fetch_all(pool)
    .await
}

pub async fn list_ids(pool: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT id FROM teams ORDER BY id")
        .fetch_all(pool)
        .await
}

/// 目录名 + 可选 `team.json` → 索引字段。
///
/// `team.json` 缺失或损坏时按目录名兜底：rescan 的职责是「磁盘有就索引」，
/// 不因一个坏文件让整个团队消失在 UI 上。
pub fn meta_from_dir(dir: &Path, team_id: &str) -> (String, i64) {
    let raw = match std::fs::read_to_string(dir.join("team.json")) {
        Ok(raw) => raw,
        Err(_) => return (team_id.to_string(), 0),
    };
    let parsed: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(e) => {
            tracing::warn!(team_id, error = %e, "team.json unparsable; indexing by dir name");
            return (team_id.to_string(), 0);
        }
    };
    let display_name = parsed
        .get("display_name")
        .and_then(|v| v.as_str())
        .unwrap_or(team_id)
        .to_string();
    let template_version = parsed
        .get("template_version")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    (display_name, template_version)
}
