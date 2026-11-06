//! 单实例检查（`$PECO_HOME/daemon.pid`）。
//!
//! 只拦「手滑起两个 daemon」，不防恶意，因此不做目录锁：目录锁会留下
//! 清理不掉的锁目录，而 pid 文件 + `/proc/<pid>/cmdline` 的误判代价是重启一次。

use std::path::{Path, PathBuf};

use crate::error::DaemonError;

pub const PID_FILE: &str = "daemon.pid";

/// `/proc/<pid>/cmdline` 里出现它才认作「另一个 daemon」。
const DAEMON_NAME: &str = "peco-daemon";

/// 持有期间 pid 文件存在，drop 时删除。
pub struct SingleInstanceGuard {
    path: PathBuf,
    pid: u32,
}

impl SingleInstanceGuard {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 取单实例锁：已有 daemon 在跑就拒绝，否则写入自己的 pid。
pub fn check_single_instance(home: &Path) -> Result<SingleInstanceGuard, DaemonError> {
    let path = home.join(PID_FILE);
    let pid = std::process::id();

    if let Some(other) = running_daemon_pid(&path)
        && other != pid
    {
        return Err(DaemonError::AlreadyRunning {
            pid: other,
            pid_file: path.display().to_string(),
        });
    }

    std::fs::write(&path, pid.to_string())?;
    Ok(SingleInstanceGuard { path, pid })
}

fn running_daemon_pid(pid_file: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(pid_file).ok()?;
    let pid: u32 = raw.trim().parse().ok()?;
    let cmdline = PathBuf::from(format!("/proc/{pid}/cmdline"));
    cmdline_contains(&cmdline, DAEMON_NAME).then_some(pid)
}

/// 进程存活且命令行含 `needle`。
pub fn cmdline_contains(cmdline: &Path, needle: &str) -> bool {
    std::fs::read(cmdline)
        .map(|raw| String::from_utf8_lossy(&raw).contains(needle))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_writes_pid_file_and_removes_it_on_drop() {
        let home = tempfile::tempdir().unwrap();
        let guard = {
            let guard = check_single_instance(home.path()).unwrap();
            assert_eq!(guard.pid(), std::process::id());
            assert_eq!(
                std::fs::read_to_string(home.path().join(PID_FILE)).unwrap(),
                std::process::id().to_string()
            );
            guard
        };
        drop(guard);
        assert!(
            !home.path().join(PID_FILE).exists(),
            "退出后应删除 pid 文件"
        );
    }

    #[test]
    fn stale_pid_file_does_not_block_startup() {
        let home = tempfile::tempdir().unwrap();
        // 该 pid 不存在 ⇒ /proc 下没有 cmdline ⇒ 视为无 daemon 在跑
        std::fs::write(home.path().join(PID_FILE), "3939393").unwrap();
        let guard = check_single_instance(home.path()).unwrap();
        assert_eq!(guard.pid(), std::process::id());
    }

    #[test]
    fn cmdline_scan_matches_only_the_daemon_name() {
        let dir = tempfile::tempdir().unwrap();
        let cmdline = dir.path().join("cmdline");
        std::fs::write(&cmdline, "peco-daemon\0--config\0x").unwrap();
        assert!(cmdline_contains(&cmdline, DAEMON_NAME));

        std::fs::write(&cmdline, "peco-server\0--port\09227").unwrap();
        assert!(!cmdline_contains(&cmdline, DAEMON_NAME));

        assert!(!cmdline_contains(&dir.path().join("missing"), DAEMON_NAME));
    }
}
