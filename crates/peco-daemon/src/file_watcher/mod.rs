//! 团队目录文件监听：外部编辑器改 `agents/` 后把新内容送进已打开的空间。
//!
//! ## 为什么监听团队根
//!
//! `teams/<team_id>/` 是同一团队所有空间共享的能力根：`agents/<name>/agent.md`
//! 对每个空间都是同一个文件。变更天然是团队级的，扇出交给
//! [`crate::space::fanout`]，不在 watcher 里判断「哪个空间该刷新」。
//!
//! ## 为什么同一团队会起 N 个 watcher
//!
//! 同一团队的 N 个已打开空间各起一个。共享一个 watcher 需要一张
//! `team_id → watcher` 的注册表与引用计数来决定「谁负责停」，而本层的设计前提
//! 是实例表只有一张（`WorkspaceHub`）；多一层表反而多一处可能与实例表不一致的
//! 状态。代价是同一目录多个 inotify watch：内核按 watch 分发，一次写入触发 N
//! 次回调，N 是同时打开的该团队空间数 —— 换来的是 watcher 的生命周期完全由实例
//! 生命周期决定。
//!
//! ## 生命周期
//!
//! 句柄挂在 daemon 的空间句柄上，实例被 Hub drop 时 [`FileWatcher`] 随之 drop 并
//! 发送关闭信号；watcher 另持 `Weak<WorkSpace>` 兜底 —— 即便句柄被别处短暂持有，
//! 空间实例消失后 watcher 也会自行退出。

mod watcher;

use std::path::PathBuf;
use std::sync::Weak;

use peco_core::tools::WorkspaceHub;
use peco_core::workspace::WorkSpace;
use sqlx::SqlitePool;
use tokio::sync::oneshot;

/// 团队目录监控句柄。Drop 时发送关闭信号。
pub struct FileWatcher {
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// 后台任务句柄。watcher 退出时完成。
    _task: tokio::task::JoinHandle<()>,
}

impl FileWatcher {
    /// 监听一个团队根目录。
    ///
    /// `team_id` 只用于扇出查询，`team_root` 是监听目标。`hub` 用 `Weak`：
    /// watcher 不该给实例表续命，Hub 没了就无事可做。
    pub fn start(
        team_root: PathBuf,
        team_id: String,
        ws: Weak<WorkSpace>,
        pool: SqlitePool,
        hub: Weak<WorkspaceHub>,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(watcher::run(team_root, team_id, ws, pool, hub, shutdown_rx));
        Self {
            shutdown_tx: Some(shutdown_tx),
            _task: task,
        }
    }
}

impl Drop for FileWatcher {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}
