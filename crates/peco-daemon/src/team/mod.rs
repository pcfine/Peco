//! 团队（能力根 `teams/<team_id>/`）的创建与模板安装。

use std::path::Path;

use peco_agents::BuiltinTemplate;

use crate::db::teams::{self, NewTeam};
use crate::error::DaemonError;
use crate::space::SpaceEnv;
use crate::space::peco::PECO_TEAM_ID;

/// 目前唯一支持的内置模板。
const SUPPORTED_TEMPLATE: &str = "team_default";

/// `team_id` 规范是 `^[a-z][a-z0-9-]{1,31}$`（总长 2..=32）。
const MAX_TEAM_ID_LEN: usize = 32;

/// 新建 `teams/<id>/` 并建立索引行；`from_template` 与 `from_team` 二选一。
pub async fn create_team(
    env: &SpaceEnv,
    id: &str,
    display_name: Option<&str>,
    from_template: Option<&str>,
    from_team: Option<&str>,
) -> Result<(), DaemonError> {
    validate_team_id(id)?;
    if from_template.is_some() && from_team.is_some() {
        return Err(DaemonError::InvalidArgument {
            message: "from_template and from_team are mutually exclusive".into(),
        });
    }

    let root = env.team_root(id);
    if root.exists() || teams::get(&env.pool, id).await?.is_some() {
        return Err(DaemonError::TeamExists {
            team_id: id.to_string(),
        });
    }

    let display_name = display_name.unwrap_or(id).to_string();
    if let Err(error) =
        create_and_index(env, &root, id, &display_name, from_template, from_team).await
    {
        // 失败时清理半装目录与索引行，避免重试撞上 `team_exists`。
        if let Err(cleanup) = teams::delete_team(&env.pool, id).await {
            tracing::warn!(team_id = id, error = %cleanup, "cannot remove partial team row");
        }
        if root.exists()
            && let Err(cleanup) = std::fs::remove_dir_all(&root)
        {
            tracing::warn!(team_id = id, error = %cleanup, "cannot remove partial team directory");
        }
        return Err(error);
    }

    tracing::info!(team_id = id, "team created");
    Ok(())
}

/// 落盘并建立索引行；失败由 [`create_team`] 统一清理。
async fn create_and_index(
    env: &SpaceEnv,
    root: &Path,
    id: &str,
    display_name: &str,
    from_template: Option<&str>,
    from_team: Option<&str>,
) -> Result<(), DaemonError> {
    let (template_version, copy_icon) = if let Some(source) = from_team {
        validate_team_id(source)?;
        if source == PECO_TEAM_ID {
            return Err(DaemonError::InvalidArgument {
                message: format!("team id '{PECO_TEAM_ID}' is reserved and cannot be copied"),
            });
        }
        let source_root = env.team_root(source);
        if !source_root.is_dir() {
            return Err(DaemonError::TeamNotFound {
                team_id: source.to_string(),
            });
        }
        let source_row = teams::get(&env.pool, source).await?;
        let icon = source_row
            .as_ref()
            .map(|row| row.icon.clone())
            .unwrap_or_default();
        let (_, version) = teams::resolve_meta(source_row, &source_root, source);
        copy_tree(&source_root, root)?;
        (version, Some(icon))
    } else if let Some(name) = from_template {
        (install_template(root, name)?, None)
    } else {
        std::fs::create_dir_all(root)?;
        (0, None)
    };

    write_team_json(root, id, display_name, template_version)?;
    teams::upsert(
        &env.pool,
        &NewTeam {
            id,
            display_name,
            template_version,
        },
    )
    .await?;
    if let Some(icon) = copy_icon.as_deref().filter(|icon| !icon.is_empty()) {
        teams::set_icon(&env.pool, id, icon, display_name, template_version).await?;
    }
    Ok(())
}

/// 递归复制目录，跳过符号链接。
fn copy_tree(src: &Path, dest: &Path) -> Result<(), DaemonError> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree(&from, &to)?;
        } else if file_type.is_file() {
            std::fs::copy(&from, &to)?;
        } else {
            tracing::warn!(path = %from.display(), "skipping non-regular file while copying team");
        }
    }
    Ok(())
}

pub(crate) fn validate_team_id(id: &str) -> Result<(), DaemonError> {
    let mut chars = id.chars();
    let head_ok = matches!(chars.next(), Some(c) if c.is_ascii_lowercase());
    let valid = head_ok
        && (2..=MAX_TEAM_ID_LEN).contains(&id.len())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(DaemonError::InvalidArgument {
            message: format!("team id '{id}' does not match ^[a-z][a-z0-9-]{{1,31}}$"),
        })
    }
}

/// 将内置模板铺进能力根，返回模板版本。
fn install_template(root: &Path, name: &str) -> Result<i64, DaemonError> {
    if name != SUPPORTED_TEMPLATE {
        return Err(DaemonError::InvalidArgument {
            message: format!("unsupported template '{name}'"),
        });
    }
    let template = BuiltinTemplate::by_name(name).ok_or_else(|| DaemonError::InvalidArgument {
        message: format!("unknown template '{name}'"),
    })?;

    std::fs::create_dir_all(root)?;
    for (rel_path, content) in template.files {
        let dest = root.join(rel_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, content)?;
    }
    Ok(template_version_of(template.files))
}

/// `@master` 的 `agent.md` 相对路径。
pub(crate) const MASTER_AGENT_REL: &str = "agents/@master/agent.md";

/// 取模板内的版本号。
fn template_version_of(files: &[(&str, &[u8])]) -> i64 {
    files
        .iter()
        .find(|(rel_path, _)| *rel_path == MASTER_AGENT_REL)
        .and_then(|(_, bytes)| std::str::from_utf8(bytes).ok())
        .and_then(template_version_from_agent_md)
        .map(i64::from)
        .unwrap_or(0)
}

/// 读取磁盘上的模板版本。
pub(crate) fn template_version_on_disk(team_root: &Path) -> Option<i64> {
    std::fs::read_to_string(team_root.join(MASTER_AGENT_REL))
        .ok()
        .and_then(|raw| template_version_from_agent_md(&raw))
        .map(i64::from)
}

fn template_version_from_agent_md(raw: &str) -> Option<u32> {
    peco_core::agent::agent_config::parse_agent_md(raw)
        .ok()
        .and_then(|(profile, _)| profile.template_version)
}

/// agent.md 的模板版本；缺失或解析失败为 0。
pub(crate) fn agent_md_template_version(agent_md: &Path) -> u32 {
    std::fs::read_to_string(agent_md)
        .ok()
        .and_then(|raw| template_version_from_agent_md(&raw))
        .unwrap_or(0)
}

/// 写入 `team.json`。
pub(crate) fn write_team_json(
    root: &Path,
    id: &str,
    display_name: &str,
    template_version: i64,
) -> Result<(), DaemonError> {
    let body = serde_json::json!({
        "team_id": id,
        "display_name": display_name,
        "template_version": template_version,
        "created_at": peco_core::workspace::manifest::now_rfc3339(),
    });
    let raw = serde_json::to_vec_pretty(&body).map_err(|e| DaemonError::Internal {
        message: format!("cannot serialize team.json: {e}"),
    })?;
    std::fs::write(root.join("team.json"), raw)?;
    Ok(())
}
