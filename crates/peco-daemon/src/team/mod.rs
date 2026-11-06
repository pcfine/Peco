//! 团队（能力根 `teams/<team_id>/`）的创建与模板安装。
//!
//! 目录是唯一真源：先把模板文件落盘，再补 `teams` 索引行。反过来做会在
//! 「行已插、目录没建」时留下一个指向空气的索引，`open` 只会报 `team_missing`。

use std::path::Path;

use peco_agents::BuiltinTemplate;

use crate::db::teams::{self, NewTeam};
use crate::error::DaemonError;
use crate::space::SpaceEnv;

/// `from_template` 本期唯一支持的内置模板。
///
/// `peco` 模板属于 Peco 自己的能力根（`teams/peco/`），不由此接口创建。
const SUPPORTED_TEMPLATE: &str = "team_default";

/// `team_id` 规范是 `^[a-z][a-z0-9-]{1,31}$`（总长 2..=32）。
const MAX_TEAM_ID_LEN: usize = 32;

/// 新建 `teams/<id>/` 并建立索引行。
pub async fn create_team(
    env: &SpaceEnv,
    id: &str,
    display_name: Option<&str>,
    from_template: Option<&str>,
) -> Result<(), DaemonError> {
    validate_team_id(id)?;

    let root = env.team_root(id);
    // 目录已存在也算「已存在」：删团队没有入口，重建同名团队会覆盖别人的能力文件。
    if root.exists() || teams::get(&env.pool, id).await?.is_some() {
        return Err(DaemonError::TeamExists {
            team_id: id.to_string(),
        });
    }

    let template_version = match from_template {
        Some(name) => install_template(&root, name)?,
        None => {
            std::fs::create_dir_all(&root)?;
            0
        }
    };

    let display_name = display_name.unwrap_or(id).to_string();
    write_team_json(&root, id, &display_name, template_version)?;
    teams::upsert(
        &env.pool,
        &NewTeam {
            id,
            display_name: &display_name,
            template_version,
        },
    )
    .await?;
    tracing::info!(team_id = id, template_version, "team created");
    Ok(())
}

fn validate_team_id(id: &str) -> Result<(), DaemonError> {
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

/// 把内置模板的文件按相对路径铺进能力根，返回模板版本。
///
/// 只铺文件、不实例化 `WorkSpace`：知识库真正的建库发生在该团队的某个空间
/// 打开时（`KnowledgeBaseManager` 扫 `knowledge/*/kb_config.json`），而
/// `init_from_template` 会在此刻连知识库后端 —— 建团队这一步不需要它。
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

/// `@master` 的 `agent.md` 相对路径 —— 版本字段与「团队是否装好」都看它。
pub(crate) const MASTER_AGENT_REL: &str = "agents/@master/agent.md";

/// 模板版本取 `@master` 的 `agent.md` frontmatter —— 与 `init_from_template`
/// 的升级闸门读同一个字段，写死常量会与模板漂移。
fn template_version_of(files: &[(&str, &[u8])]) -> i64 {
    files
        .iter()
        .find(|(rel_path, _)| *rel_path == MASTER_AGENT_REL)
        .and_then(|(_, bytes)| std::str::from_utf8(bytes).ok())
        .and_then(template_version_from_agent_md)
        .unwrap_or(0)
}

/// 从磁盘上的 `@master/agent.md` 解析模板版本；读不到或解不出返回 `None`。
pub(crate) fn template_version_on_disk(team_root: &Path) -> Option<i64> {
    std::fs::read_to_string(team_root.join(MASTER_AGENT_REL))
        .ok()
        .and_then(|raw| template_version_from_agent_md(&raw))
}

fn template_version_from_agent_md(raw: &str) -> Option<i64> {
    peco_core::agent::agent_config::parse_agent_md(raw)
        .ok()
        .and_then(|(profile, _)| profile.template_version)
        .map(i64::from)
}

/// `team.json` 是 `rescan`/`meta_from_dir` 的读入面：不写它，展示名会在下一次
/// 普查里被目录名顶掉。
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
