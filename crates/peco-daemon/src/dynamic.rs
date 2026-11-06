//! 注入 Peco 空间 system prompt 的工作空间与团队清单。
//!
//! 派发按目录寻址，Peco 必须知道每个空间的可派发目录、所属团队与在飞任务数；
//! 这些值每轮都可能变（空间开关、任务进出），所以走 `dynamic_context` 而不是
//! 会话级恒定的 `environment`。

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use peco_core::agent::DynamicContext;
use peco_core::tools::WorkspaceHub;
use sqlx::SqlitePool;
use tracing::warn;

use crate::db::{dispatches, workspaces};
use crate::error::DaemonError;
use crate::space::peco::PecoIdentity;
use crate::team;

/// 整段上限。清单会进 system prompt，每轮重算，超长会挤掉真正的工作内容。
const MAX_CATALOG_CHARS: usize = 1200;

pub struct SpaceCatalogContext {
    pool: SqlitePool,
    hub: Arc<WorkspaceHub>,
    peco: Arc<PecoIdentity>,
    home: PathBuf,
}

impl SpaceCatalogContext {
    pub fn new(
        pool: SqlitePool,
        hub: Arc<WorkspaceHub>,
        peco: Arc<PecoIdentity>,
        home: PathBuf,
    ) -> Self {
        Self {
            pool,
            hub,
            peco,
            home,
        }
    }

    /// 采集 + 落格式；任何 DB/Hub 失败都上抛给 [`DynamicContext::query`] 降级。
    async fn catalog(&self) -> Result<Catalog, DaemonError> {
        let mut spaces = Vec::new();
        for row in workspaces::list(&self.pool).await? {
            // 已关闭的空间实例已销毁，列出来等于给一个必然失败的派发目标。
            if row.closed_at.is_some() {
                continue;
            }
            let dir = PathBuf::from(&row.work_dir);
            if !self.hub.is_open(&dir) {
                continue;
            }
            let inflight = dispatches::inflight_counts(&self.pool, &row.id, &self.peco.ws_id)
                .await?
                .total();
            let current = if self.peco.is_peco_space(&row.id) {
                " [当前]"
            } else {
                ""
            };
            spaces.push(SpaceEntry {
                last_opened_at: row.last_opened_at.clone(),
                text: format!(
                    "- dir={} ws_id={} team={} inflight={inflight}{current}",
                    row.work_dir, row.id, row.team_id
                ),
            });
        }
        // 最近活跃优先：截断时必须留下最可能被派发的那些。
        spaces.sort_by(|a, b| b.last_opened_at.cmp(&a.last_opened_at));

        Ok(Catalog {
            spaces,
            teams: self.team_lines(),
            current_dir: std::fs::canonicalize(&self.peco.work_dir)
                .unwrap_or_else(|_| self.peco.work_dir.clone())
                .to_string_lossy()
                .into_owned(),
            current_ws_id: self.peco.ws_id.clone(),
        })
    }

    /// 只列真正装好 `@master` 的团队目录：`create_workspace` 的 `team` 参数是绝对
    /// 路径，缺目录或空壳团队交给 Peco 只会换来一次 `team_missing`。
    fn team_lines(&self) -> Vec<String> {
        let root = self.home.join("teams");
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                warn!(root = %root.display(), error = %e, "cannot list teams; catalog lists spaces only");
                return Vec::new();
            }
        };
        let mut lines: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            // `@master` 的存在性就是「这个团队可用」的判据。
            .filter(|entry| entry.path().join(team::MASTER_AGENT_REL).is_file())
            .map(|entry| {
                let path = entry.path();
                let root = std::fs::canonicalize(&path).unwrap_or(path);
                format!(
                    "- team={} root={}",
                    entry.file_name().to_string_lossy(),
                    root.to_string_lossy()
                )
            })
            .collect();
        lines.sort();
        lines
    }
}

#[async_trait]
impl DynamicContext for SpaceCatalogContext {
    /// 清单与用户这句话无关，`query` 被忽略。
    ///
    /// 任何失败都返回 `None`：一轮对话不该因为清单拼不出来而中止。
    async fn query(&self, _query: &str) -> Option<String> {
        match self.catalog().await {
            Ok(catalog) => Some(catalog.render()),
            Err(e) => {
                warn!(
                    code = e.code(),
                    error = %e,
                    "space catalog unavailable; this turn runs without it"
                );
                None
            }
        }
    }
}

struct SpaceEntry {
    last_opened_at: Option<String>,
    text: String,
}

struct Catalog {
    /// 已按最近活跃倒序。
    spaces: Vec<SpaceEntry>,
    teams: Vec<String>,
    current_dir: String,
    current_ws_id: String,
}

impl Catalog {
    /// 落到上限内：优先丢最不活跃的空间，其次丢队尾团队。
    ///
    /// 单行本身超限时不再截断（截一半的路径比缺一行更糟），只保留头部与当前空间。
    fn render(&self) -> String {
        let mut kept_spaces = self.spaces.len();
        let mut kept_teams = self.teams.len();
        loop {
            let text = self.compose(kept_spaces, kept_teams);
            if text.chars().count() <= MAX_CATALOG_CHARS {
                return text;
            }
            if kept_spaces > 0 {
                kept_spaces -= 1;
            } else if kept_teams > 0 {
                kept_teams -= 1;
            } else {
                return text;
            }
        }
    }

    fn compose(&self, kept_spaces: usize, kept_teams: usize) -> String {
        let mut out = String::from("## 工作空间\n");
        for entry in &self.spaces[..kept_spaces] {
            out.push_str(&entry.text);
            out.push('\n');
        }
        let dropped_spaces = self.spaces.len() - kept_spaces;
        if dropped_spaces > 0 {
            out.push_str(&format!("另有 {dropped_spaces} 个空间未列出\n"));
        }
        out.push_str(&format!(
            "共 {} 个空间；当前空间 dir={} ws_id={}\n",
            self.spaces.len(),
            self.current_dir,
            self.current_ws_id
        ));

        out.push_str("## 团队\n");
        for line in &self.teams[..kept_teams] {
            out.push_str(line);
            out.push('\n');
        }
        let dropped_teams = self.teams.len() - kept_teams;
        if dropped_teams > 0 {
            out.push_str(&format!("另有 {dropped_teams} 个团队未列出\n"));
        }
        out
    }
}
