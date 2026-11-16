//! `/api/teams` —— 团队清单、新建、删除与详情。

use std::collections::BTreeSet;
use std::path::Path;

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::db::documents;
use crate::db::knowledge_bases;
use crate::db::teams;
use crate::db::workspaces::{self, WorkspaceRow};
use crate::error::DaemonError;
use crate::space::index;
use crate::space::peco::PECO_TEAM_ID;
use crate::state::AppState;
use crate::team::{self, create_team};

#[derive(Debug, Serialize)]
pub struct TeamView {
    pub team_id: String,
    pub display_name: String,
    pub template_version: i64,
    /// `/uploads/...` URL；空串表示未设置。
    pub icon: String,
    pub agent_count: i64,
    pub skill_count: i64,
    pub mcp_count: i64,
    pub space_count: i64,
    pub agents: Vec<String>,
    pub bound_spaces: Vec<String>,
    /// 保留团队（目前只有 Peco）：不能创建也不能删除，默认不进清单。
    pub reserved: bool,
}

/// `GET /api/teams` 的查询参数。
#[derive(Debug, Deserialize)]
pub struct ListParams {
    /// 逗号分隔的附加集合，目前只认 `peco`（保留团队）。逗号分隔是为了
    /// 以后追加其它保留集合时不必换参数格式；不认识的取值忽略。
    #[serde(default)]
    include: Option<String>,
}

impl ListParams {
    fn includes(&self, source: &str) -> bool {
        self.include
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .any(|token| token.trim() == source)
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    #[serde(alias = "team_id")]
    pub id: String,
    pub display_name: Option<String>,
    /// 内置模板名（目前只有 `team_default`）。
    pub from_template: Option<String>,
    /// 复制来源团队 id；与 `from_template` 二选一。
    pub from_team: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRequest {
    /// 空串表示清空图标。
    pub icon: String,
}

fn validate_icon(icon: &str) -> Result<(), DaemonError> {
    let ok = icon.is_empty()
        || (icon.len() <= 1024
            && icon.starts_with("/uploads/")
            && !icon.contains("..")
            && !icon.chars().any(char::is_control));
    if ok {
        Ok(())
    } else {
        Err(DaemonError::InvalidArgument {
            message: "icon must be an uploaded /uploads/... URL".into(),
        })
    }
}

pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<TeamView>>, DaemonError> {
    // 默认把 Peco 挡在清单外：团队列表页是「用户自己的团队」，配置界面要
    // 显式要它（`?include=peco`）才拿得到。
    let include_reserved_peco = params.includes(PECO_TEAM_ID);
    let spaces = workspaces::list(&state.pool).await?;
    let mut views = Vec::new();
    for team_id in team_ids(&state).await? {
        if team_id == PECO_TEAM_ID && !include_reserved_peco {
            continue;
        }
        views.push(team_view(&state, &team_id, &spaces).await?);
    }
    Ok(Json(views))
}

pub async fn create(
    State(state): State<AppState>,
    Json(request): Json<CreateRequest>,
) -> Result<(StatusCode, Json<TeamView>), DaemonError> {
    // Peco 团队由启动引导安装，不由用户创建。
    if request.id == PECO_TEAM_ID {
        return Err(DaemonError::InvalidArgument {
            message: format!("team id '{PECO_TEAM_ID}' is reserved"),
        });
    }
    create_team(
        &state.env,
        &request.id,
        request.display_name.as_deref(),
        request.from_template.as_deref(),
        request.from_team.as_deref(),
    )
    .await?;
    let spaces = workspaces::list(&state.pool).await?;
    let view = team_view(&state, &request.id, &spaces).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// `PATCH /api/teams/{team_id}` —— 更新图标。
pub async fn update(
    State(state): State<AppState>,
    UrlPath(team_id): UrlPath<String>,
    Json(request): Json<UpdateRequest>,
) -> Result<Json<TeamView>, DaemonError> {
    let root = state.env.team_root(&team_id);
    let row = teams::get(&state.pool, &team_id).await?;
    if !root.is_dir() && row.is_none() {
        return Err(DaemonError::TeamNotFound { team_id });
    }
    validate_icon(&request.icon)?;

    let (display_name, template_version) = teams::resolve_meta(row, &root, &team_id);
    teams::set_icon(
        &state.pool,
        &team_id,
        &request.icon,
        &display_name,
        template_version,
    )
    .await?;

    let spaces = workspaces::list(&state.pool).await?;
    Ok(Json(team_view(&state, &team_id, &spaces).await?))
}

/// `DELETE /api/teams/{team_id}` —— 删除团队目录、索引与绑定空间行。
pub async fn delete(
    State(state): State<AppState>,
    UrlPath(team_id): UrlPath<String>,
) -> Result<StatusCode, DaemonError> {
    // Peco 是保留团队，不能删。
    if team_id == PECO_TEAM_ID {
        return Err(DaemonError::InvalidArgument {
            message: format!("team id '{PECO_TEAM_ID}' is reserved"),
        });
    }
    team::validate_team_id(&team_id)?;

    let root = state.env.team_root(&team_id);
    let row = teams::get(&state.pool, &team_id).await?;
    if !root.is_dir() && row.is_none() {
        return Err(DaemonError::TeamNotFound { team_id });
    }

    let deleted_spaces = teams::delete_team(&state.pool, &team_id).await?;
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    // 摘掉本进程里为这个团队建过的 console。它只覆盖「删除时已经在缓存里」的
    // 那棵树：与在途的 console 构建没有互斥，构建完成后由 `team_workspace`
    // 的团队存在性复核兜住，不会把已删团队的目录重新缓存下来。
    state.spaces.drop_console(&team_id);
    tracing::info!(
        team_id = %team_id,
        deleted_spaces,
        "team deleted; work dirs and markers kept"
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn team_ids(state: &AppState) -> Result<Vec<String>, DaemonError> {
    let mut ids: BTreeSet<String> = state.env.available_teams().into_iter().collect();
    for id in teams::list_ids(&state.pool).await? {
        ids.insert(id);
    }
    Ok(ids.into_iter().collect())
}

async fn team_view(
    state: &AppState,
    team_id: &str,
    spaces: &[WorkspaceRow],
) -> Result<TeamView, DaemonError> {
    let env = state.env.as_ref();
    let root = env.team_root(team_id);
    let row = teams::get(&state.pool, team_id).await?;
    let icon = row.as_ref().map(|r| r.icon.clone()).unwrap_or_default();
    let (display_name, template_version) = teams::resolve_meta(row, &root, team_id);
    let agents = installed_agents(&root);
    let skills = installed_skills(&root);
    let mcp_servers = mcp_server_names(&root);
    let bound_spaces: Vec<String> = spaces
        .iter()
        .filter(|space| space.team_id == team_id)
        .map(|space| space.id.clone())
        .collect();
    Ok(TeamView {
        team_id: team_id.to_string(),
        display_name,
        template_version,
        icon,
        agent_count: agents.len() as i64,
        skill_count: skills.len() as i64,
        mcp_count: mcp_servers.len() as i64,
        space_count: bound_spaces.len() as i64,
        agents,
        bound_spaces,
        reserved: team_id == PECO_TEAM_ID,
    })
}

/// 能力根里的 agent 名。
fn installed_agents(team_root: &Path) -> Vec<String> {
    index::scan_agents(&team_root.join("agents"))
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// 团队详情。
#[derive(Debug, Serialize)]
pub struct TeamDetailView {
    pub id: String,
    pub display_name: String,
    pub template_version: i64,
    /// `/uploads/...` URL；空串表示未设置。
    pub icon: String,
    /// 能力根绝对路径。
    pub root: String,
    /// 是否已安装 `@master`。
    pub has_master: bool,
    pub agents: Vec<TeamAgentView>,
    pub skills: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub knowledge_bases: Vec<TeamKnowledgeBaseView>,
    pub spaces: Vec<TeamSpaceView>,
}

#[derive(Debug, Serialize)]
pub struct TeamAgentView {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct TeamKnowledgeBaseView {
    pub name: String,
    pub document_count: i64,
}

#[derive(Debug, Serialize)]
pub struct TeamSpaceView {
    pub ws_id: String,
    pub work_dir: String,
    pub closed_at: Option<String>,
}

/// `GET /api/teams/{team_id}` —— 团队详情。
pub async fn detail(
    State(state): State<AppState>,
    UrlPath(team_id): UrlPath<String>,
) -> Result<Json<TeamDetailView>, DaemonError> {
    let root = state.env.team_root(&team_id);
    let row = teams::get(&state.pool, &team_id).await?;
    if !root.is_dir() && row.is_none() {
        return Err(DaemonError::TeamNotFound { team_id });
    }
    let icon = row.as_ref().map(|r| r.icon.clone()).unwrap_or_default();
    let (display_name, template_version) = teams::resolve_meta(row, &root, &team_id);

    let agents: Vec<TeamAgentView> = index::scan_agents(&root.join("agents"))
        .into_iter()
        .map(|(name, description)| TeamAgentView { name, description })
        .collect();
    let mut knowledge_bases = Vec::new();
    for kb in knowledge_bases::list_by_team(&state.pool, &team_id).await? {
        let document_count = documents::count_by_kb(&state.pool, &kb.id).await?;
        knowledge_bases.push(TeamKnowledgeBaseView {
            name: kb.name,
            document_count,
        });
    }
    let spaces = workspaces::list_by_team(&state.pool, &team_id)
        .await?
        .into_iter()
        .map(|row| TeamSpaceView {
            ws_id: row.id,
            work_dir: row.work_dir,
            closed_at: row.closed_at,
        })
        .collect();

    Ok(Json(TeamDetailView {
        id: team_id,
        display_name,
        template_version,
        icon,
        root: root.to_string_lossy().into_owned(),
        has_master: root.join(team::MASTER_AGENT_REL).is_file(),
        agents,
        skills: installed_skills(&root),
        mcp_servers: mcp_server_names(&root),
        knowledge_bases,
        spaces,
    }))
}

/// 带 `SKILL.md` 的 skill 名。
fn installed_skills(team_root: &Path) -> Vec<String> {
    let dir = team_root.join("skills");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut skills: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().join("SKILL.md").is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    skills.sort();
    skills
}

/// `mcpconfig.json` 里的 server 名。
fn mcp_server_names(team_root: &Path) -> Vec<String> {
    let path = team_root.join("mcpconfig.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            warn!(path = %path.display(), error = %e, "cannot read mcpconfig.json");
            return Vec::new();
        }
    };
    match serde_json::from_str::<peco_core::config::McpConfig>(&raw) {
        Ok(config) => {
            let mut names: Vec<String> = config.mcp_servers.into_keys().collect();
            names.sort();
            names
        }
        Err(e) => {
            warn!(path = %path.display(), error = %e, "mcpconfig.json unparsable; reporting no servers");
            Vec::new()
        }
    }
}
