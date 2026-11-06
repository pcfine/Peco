//! `/api/teams` —— 团队清单、新建与装配页所需的团队详情。
//!
//! 清单是「目录 ∪ 索引行」：目录缺失的行留着是历史（删团队没有入口），
//! 目录存在但缺行的团队照样要出现在 UI 上。

use std::collections::BTreeSet;
use std::path::Path;

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::db::documents;
use crate::db::knowledge_bases;
use crate::db::teams;
use crate::db::workspaces::{self, WorkspaceRow};
use crate::error::DaemonError;
use crate::space::index;
use crate::state::AppState;
use crate::team::{self, create_team};

#[derive(Debug, Serialize)]
pub struct TeamView {
    pub team_id: String,
    pub display_name: String,
    pub template_version: i64,
    pub agents: Vec<String>,
    pub bound_spaces: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    #[serde(alias = "team_id")]
    pub id: String,
    pub display_name: Option<String>,
    pub from_template: Option<String>,
}

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<TeamView>>, DaemonError> {
    let spaces = workspaces::list(&state.pool).await?;
    let mut views = Vec::new();
    for team_id in team_ids(&state).await? {
        views.push(team_view(&state, &team_id, &spaces).await?);
    }
    Ok(Json(views))
}

pub async fn create(
    State(state): State<AppState>,
    Json(request): Json<CreateRequest>,
) -> Result<(StatusCode, Json<TeamView>), DaemonError> {
    create_team(
        &state.env,
        &request.id,
        request.display_name.as_deref(),
        request.from_template.as_deref(),
    )
    .await?;
    let spaces = workspaces::list(&state.pool).await?;
    let view = team_view(&state, &request.id, &spaces).await?;
    Ok((StatusCode::CREATED, Json(view)))
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
    let (dir_name, dir_version) = teams::meta_from_dir(&root, team_id);
    // 行是目录的索引：缺行时按目录兜底，过期行交给下一次 rescan 修正。
    let (display_name, template_version) = match teams::get(&state.pool, team_id).await? {
        Some(row) => (row.display_name, row.template_version),
        None => (dir_name, dir_version),
    };
    Ok(TeamView {
        team_id: team_id.to_string(),
        display_name,
        template_version,
        agents: installed_agents(&root),
        bound_spaces: spaces
            .iter()
            .filter(|space| space.team_id == team_id)
            .map(|space| space.id.clone())
            .collect(),
    })
}

/// 能力根里的 agent 名（升序）。判据与详情页一致：目录下有 `agent.md` 才算
/// 装好的 agent —— 两处各定一套，同一个团队在列表页与装配页会有两个数量。
fn installed_agents(team_root: &Path) -> Vec<String> {
    index::scan_agents(&team_root.join("agents"))
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// 装配页要的团队详情。
#[derive(Debug, Serialize)]
pub struct TeamDetailView {
    pub id: String,
    pub display_name: String,
    pub template_version: i64,
    /// 能力根绝对路径：前端要拿它显示「这些能力装在哪」。
    pub root: String,
    /// 团队装好 `@master` 没有：空壳团队（只有目录）为 false，仍是 200。
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
    /// `agent.md` frontmatter 的描述；解析不了时为空串，不隐藏这条 agent。
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

/// `GET /api/teams/{team_id}` —— 装配页一次拿全。
///
/// 目录与索引行都没有才算不存在：索引可重建，目录是权威，只剩一方的团队照样
/// 要能打开装配页（否则删过库的用户就再也进不去自己的团队了）。
pub async fn detail(
    State(state): State<AppState>,
    UrlPath(team_id): UrlPath<String>,
) -> Result<Json<TeamDetailView>, DaemonError> {
    let root = state.env.team_root(&team_id);
    let row = teams::get(&state.pool, &team_id).await?;
    if !root.is_dir() && row.is_none() {
        return Err(DaemonError::TeamNotFound { team_id });
    }
    // 索引行只是目录的投影：缺行时按目录兜底，与清单接口同一顺序。
    let (display_name, template_version) = match row {
        Some(row) => (row.display_name, row.template_version),
        None => teams::meta_from_dir(&root, &team_id),
    };

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
        root: root.to_string_lossy().into_owned(),
        has_master: root.join(team::MASTER_AGENT_REL).is_file(),
        agents,
        skills: installed_skills(&root),
        mcp_servers: mcp_server_names(&root),
        knowledge_bases,
        spaces,
    }))
}

/// `skills/<name>/SKILL.md` 的目录名（升序）。
///
/// 缺 `SKILL.md` 的目录不是可加载的 skill：`SkillRegister` 扫的是这个文件，
/// 把它列出来只会让用户点进去看到空页面。
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

/// `mcpconfig.json` 里配了哪些 server（升序）。
///
/// 走 core 的 [`peco_core::config::McpConfig`] 而不是手抠 `mcpServers` 键：
/// 运行时读同一个结构体，两套解析对「哪些字段不合法」的判定不同，装配页就会
/// 显示一个实际连不上的 server。文件缺失是常态（团队可以没有 MCP），损坏只
/// 降级成空清单 —— 团队页不该因为一个配置文件打不开。
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
