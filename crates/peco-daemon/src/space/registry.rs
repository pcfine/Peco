//! `ws_id` → 已打开实例的唯一适配层，外加无空间团队的 console 实例。
//!
//! 实例表只有一张（[`WorkspaceHub`]）：本结构体只做「查库取目录 → 问 Hub 要
//! 实例」两段，不缓存第二份映射 —— 两份表一旦各自过期，就会出现「库里说打开、
//! Hub 里没有」这类无法收敛的状态。
//!
//! 例外是 console：它没有空间身份（无 `ws_id`、无 marker、不写 `workspaces`
//! 表），Hub 的键是 work_dir，放进去等于凭空造一个「空间」。它只服务「读团队
//! 能力根」这一件事，因此由本结构体自己持有，并随团队删除或团队出现已打开空间
//! 而退场。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use peco_core::tools::{WorkspaceHandle, WorkspaceHub};
use peco_core::workspace::{WorkDir, WorkSpace};
use tokio::sync::Mutex as AsyncMutex;

use crate::db::{teams, workspaces};
use crate::error::DaemonError;
use crate::provider::{layer_roots, layers_for};
use crate::space::SpaceEnv;
use crate::team;

/// console 的构建闸门。
///
/// 构建是低频事件（一个团队一次），但并发两个请求会各建一份实例，把同一份
/// `knowledge/` 目录打开两次；串行之后「同一团队最多一个 console」成立。
static CONSOLE_BUILD: AsyncMutex<()> = AsyncMutex::const_new(());

fn team_not_found(team_id: &str) -> DaemonError {
    DaemonError::TeamNotFound {
        team_id: team_id.to_string(),
    }
}

pub struct SpaceRegistry {
    hub: Arc<WorkspaceHub>,
    env: Arc<SpaceEnv>,
    consoles: Mutex<HashMap<String, Arc<WorkSpace>>>,
}

impl SpaceRegistry {
    pub fn new(hub: Arc<WorkspaceHub>, env: Arc<SpaceEnv>) -> Self {
        Self {
            hub,
            env,
            consoles: Mutex::new(HashMap::new()),
        }
    }

    /// 已打开实例。
    ///
    /// 未索引 → `space_not_found`；已索引但未打开或正在关闭 → `space_not_open`：
    /// 后者是「先打开再用」的调用顺序问题，不是资源不存在。
    pub async fn handle(&self, ws_id: &str) -> Result<Arc<dyn WorkspaceHandle>, DaemonError> {
        self.opened(ws_id)
            .await?
            .ok_or_else(|| DaemonError::SpaceNotOpen {
                ws_id: ws_id.to_string(),
            })
    }

    /// 已打开实例，未打开时为 `None`。
    ///
    /// 看板与列表类接口要能在空间关闭时照常回答（计数退化成 0），因此不能
    /// 要求它们把 `space_not_open` 当成正常分支再吞掉。
    pub async fn opened(
        &self,
        ws_id: &str,
    ) -> Result<Option<Arc<dyn WorkspaceHandle>>, DaemonError> {
        let work_dir = self.work_dir(ws_id).await?;
        // Hub 自己串行同一目录的打开/关闭；这里不持任何本地锁跨 await。
        Ok(self.hub.get(&work_dir))
    }

    pub async fn workspace(&self, ws_id: &str) -> Result<Arc<WorkSpace>, DaemonError> {
        Ok(self.handle(ws_id).await?.workspace())
    }

    /// 空间的工作目录（已索引即可得，不需要实例）。
    pub async fn work_dir(&self, ws_id: &str) -> Result<PathBuf, DaemonError> {
        Ok(PathBuf::from(self.index_row(ws_id).await?.work_dir))
    }

    /// 空间所属团队 —— 投影表（agents / knowledge_bases）的资源边界。
    ///
    /// 只读库不取实例：列表类接口要能在空间未打开时照常回答。
    pub async fn team_id(&self, ws_id: &str) -> Result<String, DaemonError> {
        Ok(self.index_row(ws_id).await?.team_id)
    }

    /// 团队能力根的实例：没有空间也拿得到。
    ///
    /// 有已打开空间时直接复用那个实例 —— 同一份 `knowledge/`、`agents/` 目录被
    /// 两棵管理器同时打开，缓存与文件锁会各说各话。没有空间才建 console。
    pub async fn team_workspace(&self, team_id: &str) -> Result<Arc<WorkSpace>, DaemonError> {
        self.ensure_owned_team_root(team_id)?;
        if !self.indexed_or_present(team_id).await? {
            return Err(team_not_found(team_id));
        }

        if let Some(ws) = self.reusable_workspace(team_id).await? {
            return Ok(ws);
        }

        let _building = CONSOLE_BUILD.lock().await;
        // 等锁期间团队可能刚打开空间，或别的请求已经建好了 console。
        if let Some(ws) = self.reusable_workspace(team_id).await? {
            return Ok(ws);
        }

        let console = self.open_console(team_id).await?;
        // 构建期间团队可能已被删：缓存一棵指向已删目录的管理器树，下一个同名
        // 团队就会拿到它。删除与构建没有互斥，只能在落缓存前复核一次。
        if !self.indexed_or_present(team_id).await? {
            return Err(team_not_found(team_id));
        }
        self.consoles()
            .insert(team_id.to_string(), Arc::clone(&console));
        tracing::info!(team = team_id, "team console workspace opened");
        Ok(console)
    }

    /// 已建好的 console 实例；没有时为 `None`。
    pub fn console(&self, team_id: &str) -> Option<Arc<WorkSpace>> {
        self.consoles().get(team_id).cloned()
    }

    /// 摘掉 console：团队删除、或团队出现已打开空间时调用。
    pub fn drop_console(&self, team_id: &str) {
        self.consoles().remove(team_id);
    }

    /// 团队 id 的路径归属：能力根必须落在 `$PECO_HOME/teams` 之下。
    ///
    /// `X-Peco-Team` 是请求方给的任意字符串，`..`、`foo/..` 或指向 teams 之外
    /// 的符号链接都会让 `team_root` 跳出去 —— 那条路径随后被当成能力根交给
    /// `open_layered`（它会在其中建目录、写文件），等于把 `$PECO_HOME` 本身交出去。
    ///
    /// 目录尚不存在（只有索引行的团队）时退回 id 语法校验：合法 id 里没有路径
    /// 分隔符，也就不可能越界。语法只在这一支上要求 —— 历史团队目录里存在
    /// `peco-dev.bak-v1` 这类不合语法的名字，它们照样要能打开。
    fn ensure_owned_team_root(&self, team_id: &str) -> Result<(), DaemonError> {
        let teams = self.env.home.join("teams");
        let root = self.env.team_root(team_id);
        let Ok(existing) = root.canonicalize() else {
            return team::validate_team_id(team_id).map_err(|_| team_not_found(team_id));
        };
        let owned = teams
            .canonicalize()
            .is_ok_and(|teams| existing.parent() == Some(teams.as_path()));
        if owned {
            Ok(())
        } else {
            Err(team_not_found(team_id))
        }
    }

    /// 团队还在不在：`teams` 表有行，或能力根目录还在。
    async fn indexed_or_present(&self, team_id: &str) -> Result<bool, DaemonError> {
        Ok(teams::get(&self.env.pool, team_id).await?.is_some()
            || self.env.team_root(team_id).is_dir())
    }

    /// 该团队此刻可直接用的实例：已打开空间优先，其次已建好的 console。
    ///
    /// 复用空间时顺手摘掉 console：团队后来打开了空间，两棵管理器不该并存。
    async fn reusable_workspace(
        &self,
        team_id: &str,
    ) -> Result<Option<Arc<WorkSpace>>, DaemonError> {
        for dir in workspaces::open_work_dirs_by_team(&self.env.pool, team_id).await? {
            // 库里有行不代表实例还在（可能已关闭）。
            if let Some(handle) = self.hub.get(Path::new(&dir)) {
                self.drop_console(team_id);
                return Ok(Some(handle.workspace()));
            }
        }
        Ok(self.console(team_id))
    }

    /// 按团队能力根建 console：root 是团队根，落盘目录独立于任何空间。
    ///
    /// 不装派发 / 文件监听 hook：那些都绑 `ws_id`，console 没有空间身份；
    /// 它自己的 manager 写盘后即时可见，无需外部变更通知。
    async fn open_console(&self, team_id: &str) -> Result<Arc<WorkSpace>, DaemonError> {
        let root = self.env.team_root(team_id);
        let system_config = self.env.system_config.clone();
        // provider 继承链与空间打开共用一处推导：团队页看到的与打开的空间一致。
        let provider_layers = layer_roots(&layers_for(self.env.as_ref(), team_id));
        let work_dir = WorkDir::new(
            self.env
                .home
                .join("work")
                .join("team-console")
                .join(team_id),
        );
        // kb_scope 与空间打开一致传 team_id：同团队共用一套知识库命名空间。
        let scope = team_id.to_string();
        // `open_layered` 是同步阻塞的（读盘、开管理器），交给 blocking 池。
        let opened = tokio::task::spawn_blocking(move || {
            WorkSpace::open_layered(root, scope, &system_config, work_dir, provider_layers)
        })
        .await
        .map_err(|e| DaemonError::Internal {
            message: format!("team console open task aborted: {e}"),
        })?;
        let ws = Arc::new(opened.map_err(DaemonError::Workspace)?);
        ws.inject_deps();
        Ok(ws)
    }

    fn consoles(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<WorkSpace>>> {
        // 锁只保护一张表：别的线程 panic 过不该让团队资源永远打不开。
        self.consoles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    async fn index_row(&self, ws_id: &str) -> Result<workspaces::WorkspaceRow, DaemonError> {
        workspaces::by_id(&self.env.pool, ws_id)
            .await?
            .ok_or_else(|| DaemonError::SpaceNotFound {
                ws_id: ws_id.to_string(),
            })
    }
}
