// ============================================================================
// boot — 启动目录（<data_dir>/boot）启动序列
// ============================================================================
//
// peco-server 启动后，在 HTTP 服务之外异步跑一次：
//
//   <data_dir>/boot/
//     ├── user_start.sh   启动前钩子，每次启动都执行
//     └── <name>.md       「启动 <name> 用户的 agent」，内容为首条用户消息
//
// `<name>` 先按 `users.username` 精确匹配，再回退 `users.id`（见 `resolve_boot_user`）。
// 投递成功即删除文件；任何一步失败只告警，未消费的文件留待下次启动。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::process::Command;
use tracing::{info, warn};

use crate::peco::active::EnqueueOutcome;
use crate::state::AppState;

use super::handler::spawn_peco_run;

pub const BOOT_DIR_NAME: &str = "boot";
pub const PRE_START_SCRIPT: &str = "user_start.sh";
/// 启动文件名后缀。
pub const BOOT_FILE_EXT: &str = "md";

/// 钩子脚本默认超时（秒）；env `PECO_START_SCRIPT_TIMEOUT_SECS` 可覆盖，
/// 置 0 表示不限时。
const DEFAULT_SCRIPT_TIMEOUT_SECS: u64 = 300;

/// 启动消息读取上限（字符），超长按此截断。
pub const MAX_MESSAGE_CHARS: usize = 32_768;

/// `PECO_START_DISABLE` 是否置位（紧急关停启动序列）。
fn disabled() -> bool {
    matches!(
        std::env::var("PECO_START_DISABLE").ok().as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "on")
    )
}

/// 钩子脚本超时；`None` 表示不限时。
pub fn script_timeout() -> Option<Duration> {
    let secs = std::env::var("PECO_START_SCRIPT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SECS);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// 启动目录的路径集合（全部由 `data_dir` 派生）。
pub struct BootDir {
    pub root: PathBuf,
    /// 数据根目录；钩子脚本的工作目录，日志也落在这里。
    pub data_dir: PathBuf,
}

impl BootDir {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        let data_dir = data_dir.as_ref().to_path_buf();
        Self {
            root: data_dir.join(BOOT_DIR_NAME),
            data_dir,
        }
    }

    pub fn pre_start_script(&self) -> PathBuf {
        self.root.join(PRE_START_SCRIPT)
    }

    /// 钩子脚本执行日志（stdout + stderr 追加写）。
    pub fn script_log(&self) -> PathBuf {
        self.data_dir.join("logs").join("user_start.log")
    }
}

/// 钩子脚本的执行结果。
#[derive(Debug)]
pub enum ScriptOutcome {
    /// 未配置钩子。
    Missing,
    /// 已退出；被信号终止时 `code` 为 `None`。
    Exited(Option<i32>),
    TimedOut(Duration),
    /// 无法启动或等待失败。
    SpawnFailed(std::io::Error),
}

/// 在后台执行启动序列（与 HTTP 服务并行）。
///
/// spawn 收在本模块：该 future 在 bin crate 内直接 `tokio::spawn` 会撞上
/// rustc 的递归求值深度上限。
pub fn spawn_start_sequence(state: Arc<AppState>) {
    tokio::spawn(run_start_sequence(state));
}

/// 执行一次启动序列：启动前钩子 → 逐个启动文件投递。
///
/// 任何一步失败都只告警，不返回错误，不影响已在服务的进程。
pub async fn run_start_sequence(state: Arc<AppState>) {
    if disabled() {
        info!("Start sequence skipped (PECO_START_DISABLE is set)");
        return;
    }

    let dir = BootDir::new(&state.data_dir);
    if let Err(e) = tokio::fs::create_dir_all(&dir.root).await {
        warn!(
            dir = %dir.root.display(),
            error = %e,
            "Failed to create boot dir; start sequence skipped"
        );
        return;
    }

    info!(dir = %dir.root.display(), "Boot dir running");

    // ── 1. 启动前钩子 ──────────────────────────────────────────────────
    let outcome = run_pre_start_script(&dir, script_timeout()).await;
    match &outcome {
        ScriptOutcome::Missing => info!("No pre-start script, skipped"),
        ScriptOutcome::Exited(Some(0)) => info!(
            log = %dir.script_log().display(),
            "Pre-start script finished successfully"
        ),
        ScriptOutcome::Exited(code) => warn!(
            code = ?code,
            log = %dir.script_log().display(),
            "Pre-start script exited non-zero (continuing)"
        ),
        ScriptOutcome::TimedOut(after) => warn!(
            ?after,
            log = %dir.script_log().display(),
            "Pre-start script timed out and was terminated (continuing)"
        ),
        ScriptOutcome::SpawnFailed(e) => warn!(
            error = %e,
            log = %dir.script_log().display(),
            "Failed to run pre-start script (continuing)"
        ),
    }

    // ── 2. 逐个启动文件（以 user query 投递）───────────────────────────
    let files = scan_boot_files(&dir).await;
    if files.is_empty() {
        info!(dir = %dir.root.display(), "No boot files, skipped");
        return;
    }

    for path in files {
        let Some(stem) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            warn!(file = %path.display(), "Boot file has no usable name, skipped");
            continue;
        };

        let Some(user_id) = resolve_boot_user(&state, &stem).await else {
            warn!(
                stem = %stem,
                file = %path.display(),
                "Boot user unresolved; boot file left in place for the next start"
            );
            continue;
        };

        let Some(message) = read_message(&path).await else {
            warn!(
                file = %path.display(),
                "Boot file has no usable message; left in place for the next start"
            );
            continue;
        };

        match deliver(&state, &user_id, message).await {
            Ok(()) => {
                info!(
                    user_id = %user_id,
                    file = %path.display(),
                    "Boot message delivered"
                );
                consume(&path).await;
            }
            Err(e) => warn!(
                user_id = %user_id,
                file = %path.display(),
                error = %e,
                "Failed to deliver boot message; left in place for the next start"
            ),
        }
    }
}

/// 扫描启动目录下的 `*.md` 普通文件，按文件名排序。
///
/// 目录不存在或读失败返回空（已告警）；目录项、非 `md` 后缀、非普通文件都忽略。
pub async fn scan_boot_files(dir: &BootDir) -> Vec<PathBuf> {
    let mut entries = match tokio::fs::read_dir(&dir.root).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!(dir = %dir.root.display(), "No boot dir, skipped");
            return Vec::new();
        }
        Err(e) => {
            warn!(dir = %dir.root.display(), error = %e, "Failed to read boot dir");
            return Vec::new();
        }
    };

    let mut files = Vec::new();
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some(BOOT_FILE_EXT) {
                    continue;
                }
                match tokio::fs::metadata(&path).await {
                    Ok(m) if m.is_file() => files.push(path),
                    Ok(_) => continue,
                    Err(e) => {
                        warn!(file = %path.display(), error = %e, "Failed to stat boot entry");
                        continue;
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                warn!(dir = %dir.root.display(), error = %e, "Failed to read boot dir entry");
                break;
            }
        }
    }

    files.sort();
    files
}

/// 读取启动消息。
///
/// 缺失 / 非普通文件 / 全空白 / 读取失败返回 `None`；超长按 [`MAX_MESSAGE_CHARS`]
/// 截断（按字符边界，不切坏 UTF-8）。
pub async fn read_message(path: &Path) -> Option<String> {
    let meta = match tokio::fs::metadata(path).await {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            warn!(file = %path.display(), "Boot file missing, skipped");
            return None;
        }
        Err(e) => {
            warn!(file = %path.display(), error = %e, "Failed to stat boot file");
            return None;
        }
    };
    if !meta.is_file() {
        warn!(file = %path.display(), "Boot file is not a regular file, skipped");
        return None;
    }

    let raw = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(e) => {
            warn!(file = %path.display(), error = %e, "Failed to read boot file");
            return None;
        }
    };

    let trimmed = raw.trim();
    let text = truncate_chars(trimmed, MAX_MESSAGE_CHARS);
    if text.len() < trimmed.len() {
        warn!(
            file = %path.display(),
            limit = MAX_MESSAGE_CHARS,
            "Boot message truncated at the character limit"
        );
    }
    if text.is_empty() {
        warn!(file = %path.display(), "Boot message is empty, skipped");
        return None;
    }
    Some(text.to_string())
}

/// 删掉已投递的启动文件；失败只告警。
pub async fn consume(path: &Path) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => info!(file = %path.display(), "Boot file removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(
            file = %path.display(),
            error = %e,
            "Failed to remove boot file"
        ),
    }
}

/// 按字符数上限截断字符串（保留完整 UTF-8 边界）。
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// 执行启动前钩子脚本。
///
/// cwd = `data_dir`，stdout/stderr 追加到 `<data_dir>/logs/user_start.log`。
/// 有可执行位则直接 exec，否则回退 `sh <path>`；Unix 下置于独立进程组，超时按组终止。
pub async fn run_pre_start_script(dir: &BootDir, timeout: Option<Duration>) -> ScriptOutcome {
    let path = dir.pre_start_script();
    match tokio::fs::metadata(&path).await {
        Ok(m) if m.is_file() => {}
        Ok(_) => {
            return ScriptOutcome::SpawnFailed(std::io::Error::other(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ScriptOutcome::Missing,
        Err(e) => return ScriptOutcome::SpawnFailed(e),
    }

    let log_path = dir.script_log();
    if let Some(parent) = log_path.parent()
        && let Err(e) = tokio::fs::create_dir_all(parent).await
    {
        return ScriptOutcome::SpawnFailed(e);
    }

    let mut log = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(e) => return ScriptOutcome::SpawnFailed(e),
    };
    {
        use std::io::Write as _;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(
            log,
            "\n===== start sequence @ {stamp} :: {} =====",
            path.display()
        );
        let _ = log.flush();
    }
    let stderr_file = match log.try_clone() {
        Ok(f) => f,
        Err(e) => return ScriptOutcome::SpawnFailed(e),
    };

    let mut command = build_command(&path);
    command
        .current_dir(&dir.data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr_file));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // 独立进程组，超时可按组终止。
        command.as_std_mut().process_group(0);
    }

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return ScriptOutcome::SpawnFailed(e),
    };
    let pid = child.id();

    let waited = match timeout {
        None => child.wait().await,
        Some(limit) => match tokio::time::timeout(limit, child.wait()).await {
            Ok(r) => r,
            Err(_) => {
                // 超时：按组 TERM，兜底 KILL。
                if let Some(pid) = pid {
                    let _ = Command::new("sh")
                        .arg("-c")
                        .arg(format!("kill -TERM -- -{pid} 2>/dev/null"))
                        .status()
                        .await;
                }
                let _ = child.kill().await;
                let _ = child.wait().await;
                return ScriptOutcome::TimedOut(limit);
            }
        },
    };

    match waited {
        Ok(status) => ScriptOutcome::Exited(status.code()),
        Err(e) => ScriptOutcome::SpawnFailed(e),
    }
}

/// 有可执行位则直接 exec，否则交给 `sh`。
fn build_command(path: &Path) -> Command {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let executable = std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
        if executable {
            return Command::new(path);
        }
    }

    let mut command = Command::new("sh");
    command.arg(path);
    command
}

/// 启动文件名 stem 到用户 id 的匹配结果。
#[derive(Debug, PartialEq, Eq)]
enum UserMatch {
    Unique(String),
    /// 同名 username 有多个。
    Ambiguous(usize),
    /// 无同名 username，也无同 id。
    NotFound,
}

/// 解析启动文件的用户名：`users.username` 优先，`users.id` 兜底。
pub async fn resolve_boot_user(state: &AppState, stem: &str) -> Option<String> {
    let users = match list_users(state).await {
        Ok(u) => u,
        Err(e) => {
            warn!(error = %e, "Failed to list users for boot user resolution");
            return None;
        }
    };

    match match_user(stem, &users) {
        UserMatch::Unique(id) => {
            info!(stem, user_id = %id, "Boot user resolved");
            Some(id)
        }
        UserMatch::Ambiguous(n) => {
            warn!(
                stem,
                matches = n,
                "Boot user ambiguous (duplicate usernames), boot file left in place"
            );
            None
        }
        UserMatch::NotFound => {
            warn!(
                stem,
                candidates = users.len(),
                "Boot user unresolved (expects username or user_id)"
            );
            None
        }
    }
}

/// 列出 `(user_id, username)`，按 id 排序。
async fn list_users(state: &AppState) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as::<_, (String, String)>("SELECT id, username FROM users ORDER BY id")
        .fetch_all(&state.db)
        .await
}

/// 先按 username 精确匹配（命中多个视为歧义），再回退按 user_id 匹配。
fn match_user(stem: &str, users: &[(String, String)]) -> UserMatch {
    let by_name: Vec<&(String, String)> = users.iter().filter(|(_, name)| name == stem).collect();
    match by_name.len() {
        0 => {}
        1 => return UserMatch::Unique(by_name[0].0.clone()),
        n => return UserMatch::Ambiguous(n),
    }

    match users.iter().find(|(id, _)| id == stem) {
        Some((id, _)) => UserMatch::Unique(id.clone()),
        None => UserMatch::NotFound,
    }
}

/// 投递启动消息：已有 run 就排队，否则抢注并新建 run。
async fn deliver(state: &Arc<AppState>, user_id: &str, text: String) -> Result<(), String> {
    match state.peco_runs.enqueue_query(user_id, text.clone()) {
        EnqueueOutcome::Enqueued => Ok(()),
        EnqueueOutcome::Backpressure => Err("peco control queue full".to_string()),
        EnqueueOutcome::NoRun => {
            let Some(registration) = state.peco_runs.try_register(user_id) else {
                return Err("peco run registry busy (concurrent registration)".to_string());
            };
            spawn_peco_run(state, user_id, text, registration)
                .await
                .map_err(|e| e.message().to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn users() -> Vec<(String, String)> {
        vec![
            ("id-a".to_string(), "alice".to_string()),
            ("id-b".to_string(), "bob".to_string()),
        ]
    }

    #[test]
    fn match_user_prefers_username_then_falls_back_to_id() {
        let u = users();
        assert_eq!(match_user("bob", &u), UserMatch::Unique("id-b".to_string()));
        assert_eq!(
            match_user("id-b", &u),
            UserMatch::Unique("id-b".to_string())
        );
        assert_eq!(match_user("bobby", &u), UserMatch::NotFound);
        assert_eq!(match_user("", &u), UserMatch::NotFound);
    }

    #[test]
    fn match_user_reports_duplicate_usernames_as_ambiguous() {
        let u = vec![
            ("id-a".to_string(), "same".to_string()),
            ("id-b".to_string(), "same".to_string()),
        ];
        assert_eq!(match_user("same", &u), UserMatch::Ambiguous(2));
        // 名字有歧义不影响 id 兜底：id 仍然唯一。
        assert_eq!(
            match_user("id-b", &u),
            UserMatch::Unique("id-b".to_string())
        );
    }

    #[test]
    fn match_user_username_wins_over_another_users_id() {
        let u = vec![
            ("id-a".to_string(), "bob".to_string()),
            ("bob".to_string(), "zoe".to_string()),
        ];
        // "bob" 既是 user id-a 的 username，又是 user bob 的 id；username 优先。
        assert_eq!(match_user("bob", &u), UserMatch::Unique("id-a".to_string()));
        // 不含歧义的名字走 id 兜底。
        assert_eq!(
            match_user("bob", &u[1..]),
            UserMatch::Unique("bob".to_string())
        );
    }

    #[test]
    fn truncate_chars_keeps_utf8_boundaries() {
        let s = "中文abc";
        assert_eq!(truncate_chars(s, 2), "中文");
        assert_eq!(truncate_chars(s, 99), s);
        assert_eq!(truncate_chars(s, 0), "");
    }

    #[test]
    fn boot_dir_paths_derive_from_data_dir() {
        let dir = BootDir::new("/tmp/data");
        assert_eq!(dir.root, PathBuf::from("/tmp/data/boot"));
        assert_eq!(
            dir.pre_start_script(),
            PathBuf::from("/tmp/data/boot/user_start.sh")
        );
        assert_eq!(
            dir.script_log(),
            PathBuf::from("/tmp/data/logs/user_start.log")
        );
    }
}
