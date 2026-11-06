//! 空间生命周期：`.workspace.json` 读写、打开决策表、关闭与删除门槛。

pub mod bootstrap;
pub mod close;
pub mod fanout;
pub mod index;
pub mod manifest;
pub mod memory_audit;
pub mod open;
pub mod peco;
pub mod reconcile;
pub mod registry;
pub mod tasks;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};

use peco_core::agent::hooks::{DispatchOrigin, DispatchWatchHook, TaskObservationSink};
use peco_core::config::SystemConfig;
use peco_core::tools::WorkspaceHub;
use sqlx::SqlitePool;

use crate::config::DaemonConfig;
use crate::db::workspaces::{self, WorkspaceRow};
use crate::error::DaemonError;
use crate::space::peco::PecoIdentity;
use crate::space::tasks::TaskObserver;

/// 打开/关闭一个空间所需的全部外部依赖。
///
/// 打包成一个结构体而不是散着传：打开决策表、关闭门槛与 HTTP handler 必须看
/// 同一份连接池与同一个能力根，否则「按目录判定团队存在」会各说各话。
///
/// 已打开实例表不在这里 —— 它仍是进程级 `peco_core::tools::WorkspaceHub`。
/// 这里只留一个弱引用，让「打开实例之后」的动作（文件监听扇出）能拿到同一张
/// 表；本结构体不承担缓存职责。
pub struct SpaceEnv {
    pub home: PathBuf,
    pub pool: SqlitePool,
    pub system_config: SystemConfig,
    /// Peco 默认空间的身份；台账分组计数与 HTTP 视图共用这一份。
    pub peco: Arc<PecoIdentity>,
    /// 台账写入端：观察 hook 是它的生产者，打开空间的用例需要拿到同一份
    /// （另建一个 `TaskObserver` 会让广播事件落进没人订阅的第二个通道）。
    pub tasks: Arc<TaskObserver>,
    /// 已打开实例表，晚绑定：Hub 需要 opener，opener 又需要 Hub 才能把文件
    /// 变更扇出到同团队的其它空间，两者只能有一方事后注入。持 `Weak` 是为了
    /// 不在 Hub 之外给它续命。
    hub: OnceLock<Weak<WorkspaceHub>>,
}

impl SpaceEnv {
    pub fn new(
        config: &DaemonConfig,
        pool: SqlitePool,
        system_config: SystemConfig,
        peco: Arc<PecoIdentity>,
        tasks: Arc<TaskObserver>,
    ) -> Self {
        Self {
            home: config.peco_home.clone(),
            pool,
            system_config,
            peco,
            tasks,
            hub: OnceLock::new(),
        }
    }

    /// 派发观察 hook 的唯一构造函数。
    ///
    /// 两条注入路径（chat 的一轮 looper、空间打开时的 machine 执行体）必须
    /// 得到同一个形状的 hook：各写一份会让 `origin_dir` / `principal` 在两条
    /// 路径上取值不同，台账从此对不上。hook 只持 `Weak<WorkspaceHub>`，
    /// 不会把已关闭的空间钉在内存里。
    pub fn dispatch_hook(
        &self,
        origin_ws_id: String,
        principal: String,
        origin_dir: PathBuf,
    ) -> Arc<DispatchWatchHook> {
        let hub = self
            .hub()
            .map_or_else(Weak::new, |hub| Arc::downgrade(&hub));
        Arc::new(DispatchWatchHook::new(
            hub,
            DispatchOrigin {
                ws_id: origin_ws_id,
                principal,
                origin_dir,
            },
            Arc::clone(&self.tasks) as Arc<dyn TaskObservationSink>,
        ))
    }

    /// 注入进程内唯一的实例表。装配顺序被破坏（重复注入）只 warn：
    /// 先到的那一份才是真正在用的 Hub，第二次注入不会让它变成另一个。
    pub fn attach_hub(&self, hub: &Arc<WorkspaceHub>) {
        if self.hub.set(Arc::downgrade(hub)).is_err() {
            tracing::warn!("space env already has a hub; keeping the first one");
        }
    }

    /// 已打开实例表；未注入时为 `None`。
    pub fn hub(&self) -> Option<Arc<WorkspaceHub>> {
        self.hub.get().and_then(Weak::upgrade)
    }

    pub fn team_root(&self, team_id: &str) -> PathBuf {
        self.home.join("teams").join(team_id)
    }

    pub fn team_exists(&self, team_id: &str) -> bool {
        self.team_root(team_id).is_dir()
    }

    /// 已安装的团队（升序），用于 `team_missing` 的可选动作列表。
    pub fn available_teams(&self) -> Vec<String> {
        let mut teams: Vec<String> = std::fs::read_dir(self.home.join("teams"))
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.path().is_dir())
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        teams.sort();
        teams
    }
}

/// 改绑空间的团队：marker 是身份真源，先写它，DB 只是索引。
///
/// 调用方必须先确认目标团队的能力根存在 —— 这里不做校验，避免「校验一次、
/// 写入一次」之间目录被删掉时静默改成悬空绑定。
pub async fn rebind_team(
    env: &SpaceEnv,
    row: &WorkspaceRow,
    team_id: &str,
) -> Result<(), DaemonError> {
    let work_dir = PathBuf::from(&row.work_dir);
    if let Some(mut existing) = manifest::read(&work_dir)? {
        existing.team = env.team_root(team_id).to_string_lossy().into_owned();
        manifest::write(&work_dir, &existing)?;
    }
    workspaces::set_team(&env.pool, &row.id, team_id).await?;
    Ok(())
}
