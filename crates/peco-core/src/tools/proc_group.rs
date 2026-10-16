// ============================================================================
// proc_group — 让 shell 子进程可连同其后代一起被终止
// ============================================================================
//
// `sh -c "cmd"` 常 fork 出孙进程；仅 `kill_on_drop` 只杀直接子进程 `sh`，
// 孙进程会脱父为 PPid=1 的孤儿继续运行。把子进程放进独立进程组后，
// 取消/超时可按组一次性 SIGKILL。

use tokio::process::Command;

/// 将即将 spawn 的子进程放入独立进程组（组 id = 子进程 pid）。
///
/// 必须在 `spawn` 之前调用；非 unix 平台为 no-op。
pub fn configure_process_group(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.as_std_mut().process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = cmd;
    }
}

/// 子进程进程组的 RAII 守卫。
///
/// 仍武装时在 `Drop` 中对整组发 SIGKILL —— 正常完成后须 [`disarm`](Self::disarm)，
/// 否则退出路径会误杀同组其他进程。组已消失时 `kill` 返回 ESRCH，无害。
pub struct GroupGuard {
    pgid: Option<i32>,
}

impl GroupGuard {
    /// 以子进程 pid 作为组 id 武装守卫；`None`（spawn 后拿不到 pid）时退化为 no-op。
    pub fn new(pid: Option<u32>) -> Self {
        Self {
            pgid: pid.and_then(|p| i32::try_from(p).ok()),
        }
    }

    /// 子进程已正常退出，撤销后续的组终止。
    pub fn disarm(&mut self) {
        self.pgid = None;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            // SAFETY: kill 是异步信号安全的系统调用；负 pid 表示按进程组投递。
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }
}
