// ============================================================================
// orphan_reaper — 启动时清理上一代遗留的孤儿 shell 子进程
// ============================================================================
//
// shell 调用被打断（超时/取消/进程组 kill 未覆盖到）时，后代可能脱父为
// PPid=1 的孤儿并继续运行。`shell_exec` 与 `execute_shell_step` 会给子进程注入
// `PECO_SHELL_CHILD=1` 标记，启动时据此识别并清理上一代孤儿，避免其持续烧 CPU。

/// 标记：仅由 peco 的 shell 工具注入，用于区分用户脚本（boot 钩子故意长跑）。
const CHILD_MARKER: &[u8] = b"PECO_SHELL_CHILD=1";

/// 单进程的判定事实。把 /proc 读取与判定分开，判定部分才能单测。
struct ProcFacts<'a> {
    pid: i32,
    ppid: i32,
    sid: i32,
    /// 可执行文件与本进程相同（即另一个 peco-server）。
    exe_is_self: bool,
    environ: &'a [u8],
}

/// 是否应当回收。
///
/// 光有「PPid==1 + 标记」不够：`( setsid ... & )` 拉起的进程父 shell 立即退出，
/// PPid 天生就是 1。若不排除，新 server 会在启动时把自己和重启 worker 一起杀掉
/// （2026-10-18 重启失败即此）。
fn should_reap(facts: &ProcFacts<'_>, self_pid: i32) -> bool {
    // 绝不杀自己，也不杀同类可执行文件（另一个 peco-server 实例）。
    if facts.pid == self_pid || facts.exe_is_self {
        return false;
    }
    // 自己开新会话（sid == pid）的是刻意常驻：restart worker、vite、手动 setsid
    // 的守护进程。它们的 ppid==1 是设计使然。意外泄漏的 shell 后代仍留在原会话里。
    if facts.sid == facts.pid {
        return false;
    }
    is_orphan_shell_child(facts.ppid, facts.environ)
}

/// 扫描 `/proc`，SIGKILL 掉上一代泄漏的孤儿 shell 子进程，返回清理数。
///
/// 任何读取失败（权限不足、进程竞态退出、非数字目录）均静默跳过。
#[cfg(unix)]
pub fn reap_orphaned_shell_children() -> usize {
    let self_pid = std::process::id() as i32;
    let self_exe = std::fs::metadata("/proc/self/exe").ok();
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
        let Some(sid) = read_sid(pid) else {
            continue;
        };

        let environ = match std::fs::read(format!("/proc/{pid}/environ")) {
            Ok(environ) => environ,
            Err(_) => continue,
        };

        let facts = ProcFacts {
            pid,
            ppid,
            sid,
            exe_is_self: same_exe(pid, self_exe.as_ref()),
            environ: &environ,
        };
        if should_reap(&facts, self_pid) {
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

/// `/proc/<pid>/stat` 的第 6 字段（session id）。comm 可能含空格与括号，
/// 因此从最后一个 `)` 之后开始切分。
#[cfg(unix)]
fn read_sid(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    let _state = fields.next()?;
    let _ppid = fields.next()?;
    let _pgrp = fields.next()?;
    fields.next()?.parse().ok()
}

/// 该 pid 的可执行文件与本进程是否为同一文件（比 dev+ino，二进制被替换也能判定）。
#[cfg(unix)]
fn same_exe(pid: i32, self_exe: Option<&std::fs::Metadata>) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(self_meta) = self_exe else {
        return false;
    };
    let Ok(meta) = std::fs::metadata(format!("/proc/{pid}/exe")) else {
        return false;
    };
    meta.dev() == self_meta.dev() && meta.ino() == self_meta.ino()
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

    const MARKER: &[u8] = b"PATH=/bin\0PECO_SHELL_CHILD=1\0";

    fn leaked_child(pid: i32, sid: i32, marker: bool) -> ProcFacts<'static> {
        ProcFacts {
            pid,
            ppid: 1,
            sid,
            exe_is_self: false,
            environ: if marker { MARKER } else { b"PATH=/bin\0" },
        }
    }

    #[test]
    fn leaked_shell_child_is_reaped() {
        // 意外泄漏：留原会话（sid != pid），父进程已死，带标记。
        assert!(should_reap(&leaked_child(4242, 999, true), 100));
    }

    #[test]
    fn self_is_never_reaped() {
        // 新 server 由 ( setsid ... & ) 拉起，ppid 天生为 1。
        assert!(!should_reap(&leaked_child(100, 100, true), 100));
    }

    #[test]
    fn same_executable_is_never_reaped() {
        let mut facts = leaked_child(4242, 999, true);
        facts.exe_is_self = true;
        assert!(!should_reap(&facts, 100));
    }

    #[test]
    fn session_leader_is_kept() {
        // setsid 出去的常驻进程（restart worker / vite）：sid == pid。
        assert!(!should_reap(&leaked_child(4242, 4242, true), 100));
    }

    #[test]
    fn orphan_without_marker_is_kept() {
        assert!(!should_reap(&leaked_child(4242, 999, false), 100));
    }

    #[test]
    fn non_orphan_with_marker_is_kept() {
        let mut facts = leaked_child(4242, 999, true);
        facts.ppid = 1234;
        assert!(!should_reap(&facts, 100));
    }

    #[test]
    fn marker_requires_exact_value() {
        // 值不同不算命中；子串匹配也不应误伤。
        assert!(!is_orphan_shell_child(1, b"PECO_SHELL_CHILD=0\0"));
        assert!(!is_orphan_shell_child(1, b"PECO_SHELL_CHILD=12\0"));
        assert!(!is_orphan_shell_child(1, b"MY_PECO_SHELL_CHILD=1\0"));
    }

    #[test]
    fn empty_environ_is_kept() {
        assert!(!should_reap(&leaked_child(4242, 999, false), 100));
    }
}
