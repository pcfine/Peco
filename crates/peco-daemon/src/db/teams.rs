//! teams 表 DAO。

use std::path::Path;

use sqlx::SqlitePool;

/// teams 表完整行。
#[derive(Debug, sqlx::FromRow)]
pub struct TeamRow {
    pub id: String,
    pub display_name: String,
    pub template_version: i64,
    /// `/uploads/...` URL；空串表示未设置。
    pub icon: String,
    pub created_at: String,
}

/// 写入一行的入参。
pub struct NewTeam<'a> {
    pub id: &'a str,
    pub display_name: &'a str,
    pub template_version: i64,
}

/// 幂等写入；不覆盖 `icon`。
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
        "SELECT id, display_name, template_version, icon, created_at FROM teams WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<TeamRow>, sqlx::Error> {
    sqlx::query_as::<_, TeamRow>(
        "SELECT id, display_name, template_version, icon, created_at FROM teams ORDER BY id",
    )
    .fetch_all(pool)
    .await
}

/// 写入图标；缺行时补行。
pub async fn set_icon(
    pool: &SqlitePool,
    id: &str,
    icon: &str,
    display_name: &str,
    template_version: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO teams (id, display_name, template_version, icon) VALUES (?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET icon = excluded.icon",
    )
    .bind(id)
    .bind(display_name)
    .bind(template_version)
    .bind(icon)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_ids(pool: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT id FROM teams ORDER BY id")
        .fetch_all(pool)
        .await
}

/// 删除团队及其索引与绑定空间行，返回删除的空间数。
pub async fn delete_team(pool: &SqlitePool, id: &str) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "DELETE FROM documents WHERE kb_id IN \
         (SELECT id FROM knowledge_bases WHERE team_id = ?)",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM knowledge_bases WHERE team_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM agents WHERE team_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let spaces = sqlx::query("DELETE FROM workspaces WHERE team_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM teams WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(spaces.rows_affected())
}

/// 目录名 + `team.json` → 展示名与模板版本。
pub fn meta_from_dir(dir: &Path, team_id: &str) -> (String, i64) {
    let (display_name, recorded) = team_json_meta(dir, team_id);
    let version = match crate::team::template_version_on_disk(dir) {
        Some(on_disk) if on_disk > recorded => on_disk,
        _ => recorded,
    };
    (display_name, version)
}

/// 合并索引行与目录元数据；版本取较大者。
pub fn resolve_meta(row: Option<TeamRow>, dir: &Path, team_id: &str) -> (String, i64) {
    let (dir_name, dir_version) = meta_from_dir(dir, team_id);
    match row {
        Some(row) => (row.display_name, row.template_version.max(dir_version)),
        None => (dir_name, dir_version),
    }
}

/// 读取 `team.json` 的展示名与版本。
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

    /// 写入带模板版本的 `@master/agent.md`。
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

    /// 磁盘版本高于 team.json 时以磁盘为准。
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

    /// team.json 缺失时用磁盘版本。
    #[test]
    fn a_missing_team_json_falls_back_to_the_version_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        write_master_agent(dir.path(), 13);

        assert_eq!(
            meta_from_dir(dir.path(), "research"),
            ("research".to_string(), 13)
        );
    }

    /// 坏 team.json 不影响磁盘版本。
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

    /// 空团队版本为 0。
    #[test]
    fn an_empty_team_has_no_template_version() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            meta_from_dir(dir.path(), "research"),
            ("research".to_string(), 0)
        );
    }

    /// 索引行版本不低报磁盘版本。
    #[test]
    fn a_row_never_reports_an_older_version_than_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        write_master_agent(dir.path(), 13);
        write_team_json(dir.path(), "研究组", 12);

        let stale_row = TeamRow {
            id: "research".to_string(),
            display_name: "研究组".to_string(),
            template_version: 12,
            icon: String::new(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
        };

        assert_eq!(
            resolve_meta(Some(stale_row), dir.path(), "research"),
            ("研究组".to_string(), 13),
            "过期索引行不能盖住磁盘上的实际版本"
        );
    }

    /// 索引行版本更高时保留。
    #[test]
    fn a_row_ahead_of_the_disk_keeps_its_version() {
        let dir = tempfile::tempdir().unwrap();
        write_team_json(dir.path(), "研究组", 13);

        let row = TeamRow {
            id: "research".to_string(),
            display_name: "研究组".to_string(),
            template_version: 13,
            icon: String::new(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
        };

        assert_eq!(
            resolve_meta(Some(row), dir.path(), "research"),
            ("研究组".to_string(), 13)
        );
    }

    /// 缺行时按目录兜底。
    #[test]
    fn a_missing_row_falls_back_to_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        write_master_agent(dir.path(), 13);

        assert_eq!(
            resolve_meta(None, dir.path(), "research"),
            ("research".to_string(), 13)
        );
    }

    async fn test_pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        (pool, dir)
    }

    /// 目录-only 团队首次改图标补行。
    #[tokio::test]
    async fn set_icon_creates_row_with_fallback_meta() {
        let (pool, _dir) = test_pool().await;

        set_icon(&pool, "research", "/uploads/teams/a.png", "research", 0)
            .await
            .unwrap();

        let row = get(&pool, "research").await.unwrap().unwrap();
        assert_eq!(row.display_name, "research");
        assert_eq!(row.template_version, 0);
        assert_eq!(row.icon, "/uploads/teams/a.png");
    }

    /// upsert 不覆盖图标。
    #[tokio::test]
    async fn upsert_does_not_clobber_icon() {
        let (pool, _dir) = test_pool().await;
        set_icon(&pool, "research", "/uploads/teams/a.png", "研究组", 1)
            .await
            .unwrap();

        upsert(
            &pool,
            &NewTeam {
                id: "research",
                display_name: "研究组",
                template_version: 2,
            },
        )
        .await
        .unwrap();

        let row = get(&pool, "research").await.unwrap().unwrap();
        assert_eq!(row.icon, "/uploads/teams/a.png");
        assert_eq!(row.template_version, 2);
    }
}
