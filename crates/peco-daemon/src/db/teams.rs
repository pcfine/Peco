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
///
/// 模板版本取 `team.json` 与 `@master/agent.md` 中的较大者：模板升级只覆盖
/// `agent.md`，`team.json` 停在首装时的版本，只读它会让已升过级的空间一直
/// 报着旧版本。
pub fn meta_from_dir(dir: &Path, team_id: &str) -> (String, i64) {
    let (display_name, recorded) = team_json_meta(dir, team_id);
    let version = match crate::team::template_version_on_disk(dir) {
        Some(on_disk) if on_disk > recorded => on_disk,
        _ => recorded,
    };
    (display_name, version)
}

/// 索引行 ∪ 目录 → 对外回答的展示名与模板版本。
///
/// 展示名信索引行（用户改过的名字要留住），模板版本取索引行与磁盘中的较大者：
/// 索引行只是上一次普查时抄下的快照，落后的那次抄写不该盖住磁盘上的实际版本。
pub fn resolve_meta(row: Option<TeamRow>, dir: &Path, team_id: &str) -> (String, i64) {
    let (dir_name, dir_version) = meta_from_dir(dir, team_id);
    match row {
        Some(row) => (row.display_name, row.template_version.max(dir_version)),
        None => (dir_name, dir_version),
    }
}

/// `team.json` 里的展示名与记录的模板版本；读不到或解不出时按目录名与 0 兜底。
fn team_json_meta(dir: &Path, team_id: &str) -> (String, i64) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份能解出模板版本的 `@master/agent.md`；`version` 就是它实际装着的模板。
    fn write_master_agent(dir: &Path, version: u32) {
        let path = dir.join(crate::team::MASTER_AGENT_REL);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "---\nagent:\n  name: \"@master\"\n  description: \"总管\"\n\
                 template_version: {version}\n---\n\n# prompt\n"
            ),
        )
        .unwrap();
    }

    fn write_team_json(dir: &Path, display_name: &str, version: i64) {
        let body = serde_json::json!({
            "team_id": "research",
            "display_name": display_name,
            "template_version": version,
            "created_at": "2024-01-01T00:00:00Z",
        });
        std::fs::write(dir.join("team.json"), body.to_string()).unwrap();
    }

    /// 模板升级只改 `agent.md`，`team.json` 停在首装版本 —— 索引必须报磁盘上的版本。
    #[test]
    fn a_stale_team_json_loses_to_the_version_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        write_master_agent(dir.path(), 13);
        write_team_json(dir.path(), "研究组", 12);

        assert_eq!(
            meta_from_dir(dir.path(), "research"),
            ("研究组".to_string(), 13),
            "team.json 落后时必须以磁盘上的模板版本为准"
        );
    }

    /// `team.json` 缺失仍要能报出磁盘版本：模板文件才是版本的真源。
    #[test]
    fn a_missing_team_json_falls_back_to_the_version_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        write_master_agent(dir.path(), 13);

        assert_eq!(
            meta_from_dir(dir.path(), "research"),
            ("research".to_string(), 13)
        );
    }

    /// 坏 `team.json` 只让展示名退回目录名，不该顺手把模板版本也清零。
    #[test]
    fn a_broken_team_json_still_reports_the_version_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        write_master_agent(dir.path(), 13);
        std::fs::write(dir.path().join("team.json"), b"{ not json").unwrap();

        assert_eq!(
            meta_from_dir(dir.path(), "research"),
            ("research".to_string(), 13)
        );
    }

    /// 空壳团队（只有目录）没有版本可报：0 是「未装模板」的既有语义。
    #[test]
    fn an_empty_team_has_no_template_version() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            meta_from_dir(dir.path(), "research"),
            ("research".to_string(), 0)
        );
    }

    /// 索引行只贡献展示名：模板版本永远不低报磁盘上的实际版本。
    #[test]
    fn a_row_never_reports_an_older_version_than_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        write_master_agent(dir.path(), 13);
        write_team_json(dir.path(), "研究组", 12);

        let stale_row = TeamRow {
            id: "research".to_string(),
            display_name: "研究组".to_string(),
            template_version: 12,
            created_at: "2024-01-01T00:00:00Z".to_string(),
        };

        assert_eq!(
            resolve_meta(Some(stale_row), dir.path(), "research"),
            ("研究组".to_string(), 13),
            "过期索引行不能盖住磁盘上的实际版本"
        );
    }

    /// 索引行版本更高时保留它：磁盘读不到版本（手写 agent.md）不代表版本归零。
    #[test]
    fn a_row_ahead_of_the_disk_keeps_its_version() {
        let dir = tempfile::tempdir().unwrap();
        write_team_json(dir.path(), "研究组", 13);

        let row = TeamRow {
            id: "research".to_string(),
            display_name: "研究组".to_string(),
            template_version: 13,
            created_at: "2024-01-01T00:00:00Z".to_string(),
        };

        assert_eq!(
            resolve_meta(Some(row), dir.path(), "research"),
            ("研究组".to_string(), 13)
        );
    }

    /// 缺行时展示名与版本都按目录兜底。
    #[test]
    fn a_missing_row_falls_back_to_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        write_master_agent(dir.path(), 13);

        assert_eq!(
            resolve_meta(None, dir.path(), "research"),
            ("research".to_string(), 13)
        );
    }
}
