use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use futures::Future;
use peco_derive::peco_tool;
use serde_json::json;
use tracing::warn;

use crate::workdir::WorkDirs;

use super::command_screen::screen_command;
use super::proc_group::{GroupGuard, configure_process_group};
use super::{Content, Tool, ToolDefinition, ToolDyn, ToolError};

/// shell 命令超时默认 20 分钟；`PECO_SHELL_TIMEOUT_SECS` 可覆盖，`0` 表示不设超时。
const DEFAULT_TIMEOUT_SECS: u64 = 20 * 60;
const TIMEOUT_ENV: &str = "PECO_SHELL_TIMEOUT_SECS";

fn shell_timeout() -> Option<Duration> {
    timeout_from(std::env::var(TIMEOUT_ENV).ok().as_deref())
}

/// 解析超时值：缺失取默认，`0` 关闭超时，非法值告警后取默认。
fn timeout_from(raw: Option<&str>) -> Option<Duration> {
    let secs = match raw {
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(v) => v,
            Err(_) => {
                warn!(
                    variable = TIMEOUT_ENV,
                    value = %raw,
                    "Invalid numeric env var; using default"
                );
                DEFAULT_TIMEOUT_SECS
            }
        },
        None => DEFAULT_TIMEOUT_SECS,
    };
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Shell command execution tool.
///
/// Executes a shell command via `sh -c` and returns the combined
/// stdout and stderr output.
#[peco_tool(
    name = "shell",
    description = "Execute a shell command and return the combined stdout and stderr output. Use this to run system commands, list directory contents, read files, or perform any other shell operation. The command is executed via `sh -c`. Set `cwd` to run the command from a specific directory.",
    params(
        command = "The shell command to execute. Examples: 'ls -la', 'cat file.txt', 'pwd', 'echo hello'",
        cwd = "Optional working directory to run the command from. Use an absolute path — e.g. a skill's directory (returned by read_skill) so relative script paths like 'python scripts/foo.py' resolve correctly."
    ),
    required = ["command"]
)]
pub async fn shell_exec(command: String, cwd: Option<String>) -> Result<String, ToolError> {
    let cwd = cwd.filter(|d| !d.is_empty());
    run_shell(&command, cwd.as_deref(), shell_timeout(), None).await
}

/// `shell_exec` 的实现主体；超时值作为参数注入以便单测复用同一路径。
///
/// 必须使用 tokio::process —— `std::process::Command::output()` 是同步阻塞调用，
/// 会占住一个 tokio worker 线程直到子进程退出。子 Agent 扇出场景下
///（RunParallelSubAgents → N 个 SimpleAgentLooper → 各自并发 shell）
/// 足以耗尽 worker 池，导致 SSE 流与其他请求全部停摆。
async fn run_shell(
    command: &str,
    cwd: Option<&str>,
    timeout: Option<Duration>,
    work: Option<&WorkDirs>,
) -> Result<String, ToolError> {
    if let Some(reason) = screen_command(command) {
        return Err(ToolError::ToolCallError(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        ))));
    }

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(command);
    // 标记后代，供下次启动时清理上一代孤儿（见 peco-server 的 orphan_reaper）。
    cmd.env("PECO_SHELL_CHILD", "1");
    // 工作层目录：产物默认落点，避免 skill 各自拼相对路径
    // （相对路径会随 cwd 漂移，且不会报错）。
    if let Some(work) = work {
        for (key, value) in work.envs() {
            cmd.env(key, value);
        }
    }
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 取消/回收工具任务时杀掉直接子进程 `sh`。
        .kill_on_drop(true);
    configure_process_group(&mut cmd);

    let child = cmd
        .spawn()
        .map_err(|e| ToolError::ToolCallError(Box::new(e)))?;
    // 超时/取消时 drop 会连同孙进程整组杀掉；正常完成前 disarm。
    let mut guard = GroupGuard::new(child.id());

    let output = match timeout {
        Some(limit) => match tokio::time::timeout(limit, child.wait_with_output()).await {
            Ok(res) => {
                let output = res.map_err(|e| ToolError::ToolCallError(Box::new(e)))?;
                guard.disarm();
                output
            }
            Err(_) => {
                return Err(ToolError::ToolCallError(Box::new(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("shell command timed out after {}s", limit.as_secs()),
                ))));
            }
        },
        None => {
            let output = child
                .wait_with_output()
                .await
                .map_err(|e| ToolError::ToolCallError(Box::new(e)))?;
            guard.disarm();
            output
        }
    };

    let mut result = String::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("STDERR:\n");
        result.push_str(&stderr);
    }
    if result.is_empty() {
        result = "(no output)".to_string();
    }

    Ok(result)
}

// ============================================================================
// ShellTool — 带 workspace 默认 cwd 的 shell 工具
// ============================================================================
//
// `ShellExec` 由 #[peco_tool] 宏生成，是零尺寸结构体，无法持有状态。
// ShellTool 手写实现 ToolDyn：持有可选的工作空间根目录作为默认 cwd，
// definition 从 ShellExec 克隆（单一真相源，避免描述文本两处漂移），
// 仅在提供默认 cwd 时修改 cwd 参数描述；执行逻辑直接复用
// 宏展开保留的原始 `shell_exec` 函数。

pub struct ShellTool {
    default_cwd: Option<PathBuf>,
    /// 工作层目录。None 时不注入（历史行为逐字节一致）。
    work: Option<WorkDirs>,
}

impl ShellTool {
    pub fn new(default_cwd: Option<PathBuf>, work: Option<WorkDirs>) -> Self {
        Self { default_cwd, work }
    }
}

/// 解析有效 cwd：显式参数优先（空串视为未提供），其次默认 cwd。
fn resolve_cwd(args_cwd: Option<String>, default_cwd: Option<&Path>) -> Option<String> {
    args_cwd
        .filter(|d| !d.is_empty())
        .or_else(|| default_cwd.map(|p| p.display().to_string()))
}

impl ToolDyn for ShellTool {
    fn name(&self) -> String {
        "shell".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        let mut def = <ShellExec as Tool>::definition(&ShellExec);
        // 仅在提供默认 cwd 时补充参数描述；None 时与 ShellExec 逐字段一致
        // （向后兼容锚点：非 WorkSpace 路径的行为不因包装而改变）
        if self.default_cwd.is_some() {
            // miss = 宏生成的 schema 形状漂移
            // 必须响亮失败而不是静默跳过
            let desc = def
                .parameters
                .pointer_mut("/properties/cwd/description")
                .expect("shell schema must contain /properties/cwd/description");
            *desc = json!(
                "Optional working directory to run the command from. \
                 If omitted, the command runs in the workspace root directory. \
                 Use an absolute path — e.g. a skill's directory (returned by \
                 read_skill) so relative script paths like 'python \
                 scripts/foo.py' resolve correctly."
            );
        }
        def
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            // 直接复用宏生成的参数结构 — 避免手写副本与 schema 漂移
            let params: <ShellExec as Tool>::Args =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let cwd = resolve_cwd(params.cwd, self.default_cwd.as_deref());
            run_shell(
                &params.command,
                cwd.as_deref(),
                shell_timeout(),
                self.work.as_ref(),
            )
            .await
            .map(Content::Text)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_cwd() {
        let default = Path::new("/ws");
        // 显式参数优先
        assert_eq!(
            resolve_cwd(Some("/a".into()), Some(default)),
            Some("/a".to_string())
        );
        // 空串视为未提供 → 落到默认
        assert_eq!(
            resolve_cwd(Some(String::new()), Some(default)),
            Some("/ws".to_string())
        );
        // 未提供 → 默认
        assert_eq!(resolve_cwd(None, Some(default)), Some("/ws".to_string()));
        // 双缺 → None（行为与旧 ShellExec 完全一致）
        assert_eq!(resolve_cwd(None, None), None);
    }

    #[test]
    fn test_timeout_from() {
        assert_eq!(timeout_from(None), Some(Duration::from_secs(20 * 60)));
        assert_eq!(timeout_from(Some("90")), Some(Duration::from_secs(90)));
        assert_eq!(timeout_from(Some("0")), None);
        assert_eq!(
            timeout_from(Some("abc")),
            Some(Duration::from_secs(20 * 60))
        );
        assert_eq!(timeout_from(Some(" 30 ")), Some(Duration::from_secs(30)));
    }

    #[test]
    fn test_shell_tool_definition_patched() {
        let tool = ShellTool::new(Some(PathBuf::from("/ws")), None);
        let def = tool.definition();
        assert_eq!(def.name, "shell");

        let exec_def = <ShellExec as Tool>::definition(&ShellExec);
        // cwd 描述补充了默认行为
        assert!(
            def.parameters["properties"]["cwd"]["description"]
                .as_str()
                .unwrap()
                .contains("workspace root directory")
        );
        // 其余部分与 ShellExec 一致
        assert_eq!(
            def.parameters["properties"]["command"]["description"],
            exec_def.parameters["properties"]["command"]["description"]
        );
        assert_eq!(def.description, exec_def.description);
        assert_eq!(def.parameters["required"], json!(["command"]));
    }

    #[test]
    fn test_shell_tool_definition_none_identical() {
        // 无默认 cwd 时，definition 必须与 ShellExec 逐字段相同
        let tool = ShellTool::new(None, None);
        let def = tool.definition();
        let exec_def = <ShellExec as Tool>::definition(&ShellExec);
        assert_eq!(def.name, exec_def.name);
        assert_eq!(def.description, exec_def.description);
        assert_eq!(def.parameters, exec_def.parameters);
    }

    #[tokio::test]
    async fn test_shell_tool_default_cwd_used() {
        let dir = std::env::temp_dir().join(format!("peco-shell-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = ShellTool::new(Some(dir.clone()), None);
        let out = tool
            .call(r#"{"command": "pwd"}"#.to_string())
            .await
            .unwrap()
            .text_view()
            .into_owned();
        let prefix = dir.display().to_string();
        assert!(
            out.trim().starts_with(&prefix),
            "pwd output `{out}` should start with `{prefix}`"
        );
        let _ = std::fs::remove_dir(&dir);
    }

    // ── 进程组加固 ─────────────────────────────────────────────────────
    //
    // 用 `sleep ... & ... wait` 制造一个真实孙进程；验证超时 / 取消两条路径
    // 都能把整组杀掉，而非只杀 `sh`。

    #[cfg(unix)]
    fn is_running(pid: i32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            // "pid (comm) STATE ..."：comm 可含空格/括号，取最后一个 ')' 之后。
            Ok(stat) => {
                let state = stat
                    .rsplit(')')
                    .next()
                    .unwrap_or("")
                    .trim_start()
                    .chars()
                    .next();
                !matches!(state, Some('Z') | None)
            }
            Err(_) => false,
        }
    }

    #[cfg(unix)]
    fn read_pidfile(path: &Path) -> Option<i32> {
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    #[cfg(unix)]
    async fn wait_for_pidfile(path: &Path) -> Option<i32> {
        for _ in 0..100 {
            if let Some(pid) = read_pidfile(path) {
                return Some(pid);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        None
    }

    #[cfg(unix)]
    async fn wait_until_dead(pid: i32, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if !is_running(pid) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(unix)]
    fn grandchild_script(pidfile: &Path) -> String {
        format!("sleep 30 & echo $! > '{}'; wait", pidfile.display())
    }

    #[cfg(unix)]
    fn temp_pidfile(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("peco-shell-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("gc.pid");
        (dir, pidfile)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_timeout_kills_process_group() {
        let (dir, pidfile) = temp_pidfile("timeout");
        let err = run_shell(
            &grandchild_script(&pidfile),
            None,
            Some(Duration::from_millis(300)),
            None,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("timed out"),
            "expected timeout error, got: {err}"
        );

        let grandchild = read_pidfile(&pidfile).expect("grandchild pid file missing");
        assert!(
            wait_until_dead(grandchild, Duration::from_secs(3)).await,
            "grandchild {grandchild} survived timeout"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_shell_kills_process_group() {
        let (dir, pidfile) = temp_pidfile("cancel");
        let script = grandchild_script(&pidfile);
        let handle = tokio::spawn(async move { run_shell(&script, None, None, None).await });

        let grandchild = wait_for_pidfile(&pidfile)
            .await
            .expect("grandchild pid file missing");
        assert!(is_running(grandchild), "grandchild should start running");

        handle.abort();
        assert!(
            wait_until_dead(grandchild, Duration::from_secs(3)).await,
            "grandchild {grandchild} survived cancellation"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
