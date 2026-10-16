// ============================================================================
// orphan_reaper — 启动时清理上一代遗留的孤儿 shell 子进程
// ============================================================================
//
// shell 调用被打断（超时/取消/进程组 kill 未覆盖到）时，后代可能脱父为
// PPid=1 的孤儿并继续运行。`shell_exec` 与 `execute_shell_step` 会给子进程注入
// `PECO_SHELL_CHILD=1` 标记，启动时据此识别并清理上一代孤儿，避免其持续烧 CPU。

/// 标记：仅由 peco 的 shell 工具注入，用于区分用户脚本（boot 钩子故意长跑）。
const CHILD_MARKER: &[u8] = b"PECO_SHELL_CHILD=1";

/// 扫描 `/proc`，SIGKILL 掉所有「PPid==1 且带 shell 子进程标记」的进程，返回清理数。
///
/// 任何读取失败（权限不足、进程竞态退出、非数字目录）均静默跳过。
#[cfg(unix)]
pub fn reap_orphaned_shell_children() -> usize {
    let mut reaped = 0;
    let entries = match std::fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(_) => return 0,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<i32>().ok()) else {
            continue;
        };

        let ppid = match read_ppid(pid) {
            Some(ppid) => ppid,
            None => continue,
        };
        // 只处理无主进程；PPid!=1 说明父进程仍在，不归本次清理。
        if ppid != 1 {
            continue;
        }

        let environ = match std::fs::read(format!("/proc/{pid}/environ")) {
            Ok(environ) => environ,
            Err(_) => continue,
        };

        if is_orphan_shell_child(ppid, &environ) {
            // SAFETY: kill 是异步信号安全的系统调用；pid 来自 /proc 枚举。
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            reaped += 1;
        }
    }

    reaped
}

#[cfg(not(unix))]
pub fn reap_orphaned_shell_children() -> usize {
    0
}

/// `/proc/<pid>/status` 的 `PPid:` 字段；读不到返回 `None`。
#[cfg(unix)]
fn read_ppid(pid: i32) -> Option<i32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .and_then(|value| value.trim().parse().ok())
}

/// 判定标准：无主（PPid==1）**且** environ 含 peco 注入的 shell 标记。
fn is_orphan_shell_child(ppid: i32, environ: &[u8]) -> bool {
    ppid == 1
        && environ
            .split(|b| *b == 0)
            .any(|entry| entry == CHILD_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_with_marker_is_reaped() {
        assert!(is_orphan_shell_child(1, b"PATH=/bin\0PECO_SHELL_CHILD=1\0"));
    }

    #[test]
    fn orphan_without_marker_is_kept() {
        assert!(!is_orphan_shell_child(1, b"PATH=/bin\0HOME=/root\0"));
    }

    #[test]
    fn empty_environ_is_kept() {
        assert!(!is_orphan_shell_child(1, b""));
    }

    #[test]
    fn non_orphan_with_marker_is_kept() {
        assert!(!is_orphan_shell_child(4242, b"PECO_SHELL_CHILD=1\0"));
    }

    #[test]
    fn marker_requires_exact_value() {
        // 值不同不算命中；子串匹配也不应误伤。
        assert!(!is_orphan_shell_child(1, b"PECO_SHELL_CHILD=0\0"));
        assert!(!is_orphan_shell_child(1, b"PECO_SHELL_CHILD=12\0"));
        assert!(!is_orphan_shell_child(1, b"MY_PECO_SHELL_CHILD=1\0"));
    }
}
