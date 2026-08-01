//! 工作空间文件监控器。
//!
//! 为每个活跃用户的 workspace 目录创建后台 tokio 任务，
//! 监听文件变更并触发对应的管理器重载。
//!
//! ## 生命周期
//!
//! - [`FileWatcher::start`] 创建监控任务，持有 `Weak<WorkSpace>` 引用
//! - 当 LRU 驱逐 WorkSpace 后，`Weak::upgrade()` 返回 `None`，watcher 自动退出
//! - Drop [`FileWatcher`] 时发送关闭信号，后台 task 优雅退出

mod watcher;

use std::path::PathBuf;
use std::sync::Weak;

use peco_core::workspace::WorkSpace;
use tokio::sync::oneshot;

/// 工作空间文件监控句柄。
///
/// 持有后台 watcher task 的控制通道。Drop 时自动发送关闭信号。
pub struct FileWatcher {
    /// 发送关闭信号。`None` 表示已发送。
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// 后台任务句柄。watcher 退出时 JoinHandle 完成。
    _task: tokio::task::JoinHandle<()>,
}

impl FileWatcher {
    /// 启动对指定 workspace 目录的文件监控。
    ///
    /// 后台 task 持有 `Weak<WorkSpace>`，当 WorkSpace 被 LRU 驱逐后自动退出。
    /// watcher 创建失败时记录 error 日志并返回 `None`。
    pub fn start(workspace_root: PathBuf, ws: Weak<WorkSpace>) -> Option<Self> {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(watcher::run(workspace_root, ws, shutdown_rx));
        Some(Self {
            shutdown_tx: Some(shutdown_tx),
            _task: task,
        })
    }

    /// 优雅停止文件监控（发送关闭信号，不等待 task 完成）。
    pub fn stop(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
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
