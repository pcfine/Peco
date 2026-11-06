//! daemon 自有配置。
//!
//! core 不读这个文件，因此字段可自由增删而不影响 `WorkSpace::open`；
//! 未知字段一律忽略（老配置配新代码不该启动失败）。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::DaemonError;

/// `$PECO_HOME/daemon.toml`。
pub const CONFIG_FILE: &str = "daemon.toml";

/// 与既有服务（9227 / 6969 / 6970 / 9233）错开的默认监听地址。
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:6971";

/// 全局退出策略 — 单空间 `inherit` 时用它（`ask` 只在服务端等价 `keep`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GlobalExitPolicy {
    Ask,
    Keep,
    Stop,
}

/// 两级配置解析后的退出策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitPolicy {
    Keep,
    Stop,
}

impl ExitPolicy {
    /// 对外字符串；`GET /api/spaces/{id}` 的 `effective_exit_policy` 用它。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Stop => "stop",
        }
    }
}

/// 单空间策略非 `inherit` 时用它，否则用全局；全局 `ask` 等价 `keep`。
///
/// 结果只描述「将来自动关闭时该怎么做」：本期 `POST /api/spaces/{id}/close`
/// 由 UI 显式触发、无条件关闭，解析值仅经 `GET /api/spaces/{id}` 的
/// `effective_exit_policy` 暴露给 UI，**不得**当作 close 的开关 —— 否则用户
/// 点关闭会毫无反馈，而配置本意是「退出 daemon 时怎么处理空间」。
pub fn resolve_exit_policy(per_space: &str, global: GlobalExitPolicy) -> ExitPolicy {
    match per_space.trim().to_ascii_lowercase().as_str() {
        "keep" => ExitPolicy::Keep,
        "stop" => ExitPolicy::Stop,
        // `inherit` 与任何未知取值都跟随全局：DB 的 CHECK 已挡住其它值，
        // 走到这里只可能是旧数据，跟随全局比 panic 更有用。
        _ => match global {
            GlobalExitPolicy::Ask | GlobalExitPolicy::Keep => ExitPolicy::Keep,
            GlobalExitPolicy::Stop => ExitPolicy::Stop,
        },
    }
}

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub peco_home: PathBuf,
    pub bind_addr: SocketAddr,
    pub exit_policy: GlobalExitPolicy,
}

impl DaemonConfig {
    /// 环境变量 → `$PECO_HOME` → `daemon.toml`。
    pub fn load() -> Result<Self, DaemonError> {
        let home = match std::env::var_os("PECO_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => default_home()?,
        };
        let mut config = Self::load_from(&home)?;

        apply_env(&mut config, "PECO_DAEMON_BIND_ADDR", |c, raw| {
            c.bind_addr = raw
                .parse()
                .map_err(|_| DaemonError::Config(format!("invalid bind_addr '{raw}'")))?;
            Ok(())
        })?;
        apply_env(&mut config, "PECO_DAEMON_EXIT_POLICY", |c, raw| {
            c.exit_policy = parse_exit_policy(raw)?;
            Ok(())
        })?;

        Ok(config)
    }

    /// 从指定的 `$PECO_HOME` 读 `daemon.toml`；文件缺失即全默认。
    pub fn load_from(home: &Path) -> Result<Self, DaemonError> {
        let file = read_config_file(home)?;
        let bind_addr = match file.daemon.bind_addr {
            Some(raw) => raw
                .parse()
                .map_err(|_| DaemonError::Config(format!("invalid bind_addr '{raw}'")))?,
            None => DEFAULT_BIND_ADDR
                .parse()
                .expect("DEFAULT_BIND_ADDR 是编译期常量"),
        };
        let exit_policy = match file.workspace.exit_policy {
            Some(raw) => parse_exit_policy(&raw)?,
            None => GlobalExitPolicy::Ask,
        };
        Ok(Self {
            // 先规范到真实绝对路径：spaces/ 与 teams/ 的子路径都从它拼出，
            // 父路径带符号链接会让同一个目录出现两种写法、两行 workspaces。
            peco_home: normalize_home(home),
            bind_addr,
            exit_policy,
        })
    }

    pub fn teams_dir(&self) -> PathBuf {
        self.peco_home.join("teams")
    }

    pub fn spaces_dir(&self) -> PathBuf {
        self.peco_home.join("spaces")
    }

    pub fn db_path(&self) -> PathBuf {
        self.peco_home.join("peco.db")
    }

    pub fn db_url(&self) -> String {
        format!("sqlite:{}?mode=rwc", self.db_path().display())
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileConfig {
    daemon: DaemonSection,
    workspace: WorkspaceSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DaemonSection {
    bind_addr: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WorkspaceSection {
    exit_policy: Option<String>,
}

fn read_config_file(home: &Path) -> Result<FileConfig, DaemonError> {
    let path = home.join(CONFIG_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileConfig::default()),
        Err(e) => {
            return Err(DaemonError::Config(format!(
                "cannot read '{}': {e}",
                path.display()
            )));
        }
    };
    toml::from_str(&raw)
        .map_err(|e| DaemonError::Config(format!("cannot parse '{}': {e}", path.display())))
}

fn apply_env(
    config: &mut DaemonConfig,
    key: &str,
    apply: impl FnOnce(&mut DaemonConfig, &str) -> Result<(), DaemonError>,
) -> Result<(), DaemonError> {
    match std::env::var(key) {
        Ok(raw) => apply(config, &raw),
        Err(std::env::VarError::NotPresent) => Ok(()),
        Err(e) => Err(DaemonError::Config(format!("{key}: {e}"))),
    }
}

fn parse_exit_policy(raw: &str) -> Result<GlobalExitPolicy, DaemonError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "ask" => Ok(GlobalExitPolicy::Ask),
        "keep" => Ok(GlobalExitPolicy::Keep),
        "stop" => Ok(GlobalExitPolicy::Stop),
        other => Err(DaemonError::Config(format!(
            "invalid exit_policy '{other}'"
        ))),
    }
}

fn normalize_home(home: &Path) -> PathBuf {
    std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf())
}

fn default_home() -> Result<PathBuf, DaemonError> {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| DaemonError::Config("HOME is unset and PECO_HOME is not given".into()))?;
    Ok(base.join(".peco"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_when_config_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let config = DaemonConfig::load_from(dir.path()).unwrap();
        assert_eq!(config.exit_policy, GlobalExitPolicy::Ask);
        assert_eq!(
            config.bind_addr,
            DEFAULT_BIND_ADDR.parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn file_values_win_over_defaults_and_unknown_keys_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "[daemon]\nbind_addr = \"127.0.0.1:6999\"\nfuture_flag = true\n\
             [workspace]\nexit_policy = \"stop\"\nunknown_key = \"x\"\n\
             [nobody_knows]\nfoo = 1\n",
        )
        .unwrap();

        let config = DaemonConfig::load_from(dir.path()).unwrap();
        assert_eq!(
            config.bind_addr,
            "127.0.0.1:6999".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(config.exit_policy, GlobalExitPolicy::Stop);
    }

    /// 旧配置里残留已删除的字段既不该报错，也不该影响任何配置项。
    #[test]
    fn legacy_monitor_keys_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);

        std::fs::write(&path, "[daemon]\nbind_addr = \"127.0.0.1:6971\"\n").unwrap();
        let without = DaemonConfig::load_from(dir.path()).unwrap();

        std::fs::write(
            &path,
            "[daemon]\nbind_addr = \"127.0.0.1:6971\"\n\
             monitor_interval_secs = 7\ncensus_interval_secs = 70\n",
        )
        .unwrap();
        let with = DaemonConfig::load_from(dir.path()).unwrap();

        assert_eq!(
            format!("{with:?}"),
            format!("{without:?}"),
            "残留字段不得影响任何配置项"
        );
    }

    /// 单空间非 `inherit` 覆盖全局，`inherit` 跟随全局，全局 `ask` 等价 `keep`。
    #[test]
    fn per_space_exit_policy_wins_until_it_inherits() {
        use ExitPolicy::{Keep, Stop};

        assert_eq!(resolve_exit_policy("inherit", GlobalExitPolicy::Stop), Stop);
        assert_eq!(resolve_exit_policy("keep", GlobalExitPolicy::Stop), Keep);
        assert_eq!(resolve_exit_policy("stop", GlobalExitPolicy::Ask), Stop);
        assert_eq!(resolve_exit_policy("inherit", GlobalExitPolicy::Ask), Keep);
        assert_eq!(resolve_exit_policy("inherit", GlobalExitPolicy::Keep), Keep);
        assert_eq!(
            resolve_exit_policy("nonsense", GlobalExitPolicy::Stop),
            Stop,
            "未知单空间取值跟随全局"
        );
        assert_eq!(Keep.as_str(), "keep");
        assert_eq!(Stop.as_str(), "stop");
    }

    #[test]
    fn invalid_exit_policy_is_reported_as_config_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "[workspace]\nexit_policy = \"nuke\"\n",
        )
        .unwrap();

        let err = DaemonConfig::load_from(dir.path()).unwrap_err();
        assert_eq!(err.code(), "config_error");
        assert!(err.to_string().contains("nuke"), "{err}");
    }

    #[test]
    fn home_paths_are_built_from_peco_home() {
        let dir = tempfile::tempdir().unwrap();
        let config = DaemonConfig::load_from(dir.path()).unwrap();
        assert_eq!(config.teams_dir(), config.peco_home.join("teams"));
        assert_eq!(config.spaces_dir(), config.peco_home.join("spaces"));
        assert!(config.db_url().ends_with("peco.db?mode=rwc"));
    }
}
