//! `ws_id` → 已打开实例的唯一适配层。
//!
//! 实例表只有一张（[`WorkspaceHub`]）：本结构体只做「查库取目录 → 问 Hub 要
//! 实例」两段，不缓存第二份映射 —— 两份表一旦各自过期，就会出现「库里说打开、
//! Hub 里没有」这类无法收敛的状态。

use std::path::PathBuf;
use std::sync::Arc;

use peco_core::tools::{WorkspaceHandle, WorkspaceHub};
use peco_core::workspace::WorkSpace;
use sqlx::SqlitePool;

use crate::db::workspaces;
use crate::error::DaemonError;

pub struct SpaceRegistry {
    hub: Arc<WorkspaceHub>,
    pool: SqlitePool,
}

impl SpaceRegistry {
    pub fn new(hub: Arc<WorkspaceHub>, pool: SqlitePool) -> Self {
        Self { hub, pool }
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

    async fn index_row(&self, ws_id: &str) -> Result<workspaces::WorkspaceRow, DaemonError> {
        workspaces::by_id(&self.pool, ws_id)
            .await?
            .ok_or_else(|| DaemonError::SpaceNotFound {
                ws_id: ws_id.to_string(),
            })
    }
}
