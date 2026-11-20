//! daemon 自有配置。
//!
//! core 不读这个文件，因此字段可自由增删而不影响 `WorkSpace::open`；
//! 未知字段一律忽略（老配置配新代码不该启动失败）。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use peco_core::config::{McpConfig, ProvidersConfig, SystemConfig};
use serde::Deserialize;
use tracing::warn;

use crate::error::DaemonError;

/// `$PECO_HOME/daemon.toml`。
pub const CONFIG_FILE: &str = "daemon.toml";

/// 每一层能力根下的 provider 配置文件。
///
/// 定义在这里而不是 `provider` 模块：解析系统层路径要读环境变量，而读环境变量的
/// 唯一位置是本文件（`PECO_HOME` / `PECO_DAEMON_*` 都在这里），让 `provider`
/// 自己读 env 会造出第二个环境边界。团队层继续用同一个常量。
pub const PROVIDERS_FILE: &str = "providers.toml";

/// daemon 数据根在 `$HOME` 下的目录名。
///
/// 这是 daemon 数据根的唯一定义：部署脚本按行从这行常量解析目录名，解析到零处
/// 或两处都直接失败，不在别处再写一份字面量。
///
/// `~/.peco` 归 peco-server 所有，daemon 不得复用它 —— 两个进程写同一份
/// `peco.db` 会把数据搅在一起。改动这里的值之前，先确认部署脚本最后那道
/// 「`work_dir` 前缀必须等于数据根」的校验点仍然成立。
///
/// peco-core 里还有两处 `.peco` 字面量（知识库配置与会话文件持久化的默认路径），
/// 全仓零调用；不要为了「统一」去改它们 —— core 不该认识 daemon 的目录名，
/// 改这两处只有耦合，没有运行时收益。
pub const HOME_DIR_NAME: &str = ".peco-daemon";

/// 与既有服务（9227 / 6969 / 6970 / 9233）错开的默认监听地址。
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:6971";

/// setup token 的默认文件名（`$PECO_HOME/token`）。
///
/// 它是「谁有权把本地账号绑到密码上」的唯一凭据，也是自动化的 Bearer 令牌：
/// 落在 daemon 自己的数据根里，与 `daemon.toml` 同级。
pub const TOKEN_FILE: &str = "token";

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
    /// 系统层 provider 文件所在的目录，`PROVIDERS_FILE` 拼在它后面。
    ///
    /// 缺省是 `peco_home`：系统层属于 daemon 自己的数据根，不是 core 的 XDG
    /// 默认位置（后者还掺了一条依赖 CWD 的候选）。
    providers_root: PathBuf,
    pub bind_addr: SocketAddr,
    pub exit_policy: GlobalExitPolicy,
    /// HTTPS 素材。两个都配齐才开 TLS，缺任一维持 HTTP。
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
    /// setup token 文件；缺省是 `$PECO_HOME/token`。
    token_file: Option<PathBuf>,
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
        apply_env(&mut config, "PECO_PROVIDERS_CONFIG", |c, raw| {
            c.providers_root = providers_root_from_env(raw)?;
            Ok(())
        })?;

        Ok(config)
    }

    /// 从指定的 `$PECO_HOME` 读 `daemon.toml`；文件缺失即全默认。
    pub fn load_from(home: &Path) -> Result<Self, DaemonError> {
        let FileConfig { daemon, workspace } = read_config_file(home)?;
        let bind_addr = match daemon.bind_addr {
            Some(raw) => raw
                .parse()
                .map_err(|_| DaemonError::Config(format!("invalid bind_addr '{raw}'")))?,
            None => DEFAULT_BIND_ADDR
                .parse()
                .expect("DEFAULT_BIND_ADDR 是编译期常量"),
        };
        let exit_policy = match workspace.exit_policy {
            Some(raw) => parse_exit_policy(&raw)?,
            None => GlobalExitPolicy::Ask,
        };
        // 先规范到真实绝对路径：spaces/ 与 teams/ 的子路径都从它拼出，
        // 父路径带符号链接会让同一个目录出现两种写法、两行 workspaces。
        let peco_home = normalize_home(home);
        Ok(Self {
            providers_root: peco_home.clone(),
            token_file: daemon
                .token_file
                .map(|raw| resolve_against(&peco_home, raw)),
            tls_cert: daemon.tls_cert.map(|raw| resolve_against(&peco_home, raw)),
            tls_key: daemon.tls_key.map(|raw| resolve_against(&peco_home, raw)),
            peco_home,
            bind_addr,
            exit_policy,
        })
    }

    /// HTTPS 素材：证书与私钥都配齐才返回。
    pub fn tls(&self) -> Option<(&Path, &Path)> {
        match (&self.tls_cert, &self.tls_key) {
            (Some(cert), Some(key)) => Some((cert, key)),
            _ => None,
        }
    }

    /// 只配了一侧。启动期据此 warn：静默跑 HTTP 会让人以为在跑 HTTPS。
    pub fn has_partial_tls(&self) -> bool {
        self.tls_cert.is_some() != self.tls_key.is_some()
    }

    pub fn token_path(&self) -> PathBuf {
        self.token_file
            .clone()
            .unwrap_or_else(|| self.peco_home.join(TOKEN_FILE))
    }

    /// 系统层 provider 文件的目录（低优先层，所有团队共用）。
    pub fn system_providers_root(&self) -> &Path {
        &self.providers_root
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
    tls_cert: Option<String>,
    tls_key: Option<String>,
    token_file: Option<String>,
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

/// 相对路径按数据根解析。
///
/// 含义取决于 CWD 的路径会随启动方式漂移（systemd 的 `WorkingDirectory` 与手动
/// 启动下的 CWD 不是同一个目录），证书与 token 文件因此只认数据根。
fn resolve_against(home: &Path, raw: String) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        home.join(path)
    }
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

/// `PECO_PROVIDERS_CONFIG` 的值 = 系统层文件的**绝对**路径，且必须叫 `providers.toml`。
///
/// 三条校验都是防「静默不生效」：层是按**目录**拼文件名的（core 的
/// `UserConfig::load_layered` 只吃目录，core 只读），所以只要文件名或父目录
/// 有一处打错，系统层就会静默为空 —— 与「我根本没设这个变量」表现完全一样。
/// 相对路径同理：它的含义取决于 CWD，而 systemd 的 `WorkingDirectory` 与手动启动
/// 下的 CWD 不是同一个目录。违反即启动失败，不降级。
fn providers_root_from_env(raw: &str) -> Result<PathBuf, DaemonError> {
    let path = Path::new(raw);
    if path.file_name() != Some(std::ffi::OsStr::new(PROVIDERS_FILE)) {
        return Err(DaemonError::Config(format!(
            "PECO_PROVIDERS_CONFIG must point at a file named '{PROVIDERS_FILE}', got '{raw}'"
        )));
    }
    if !path.is_absolute() {
        return Err(DaemonError::Config(format!(
            "PECO_PROVIDERS_CONFIG must be an absolute path, got '{raw}'"
        )));
    }
    match path.parent() {
        Some(parent) if parent.is_dir() => Ok(parent.to_path_buf()),
        _ => Err(DaemonError::Config(format!(
            "PECO_PROVIDERS_CONFIG parent directory does not exist: '{raw}'"
        ))),
    }
}

/// daemon 的进程级配置基座：providers 基座为空，MCP 仍走 core 的加载。
///
/// providers 基座为空是有意的：系统层是 `$PECO_HOME/providers.toml` 这一最低
/// 优先级文件层后，删除条目必须能经 `reload_providers()` 生效。留一份启动快照当
/// 基座，删掉的条目会被快照复活；读 core 的默认位置则会让系统层取决于 CWD。
///
/// 代价是 `SystemConfig` 在本 crate 里退化成「MCP 载体」—— 读 `system_config()`
/// 的代码不要指望从这里拿 provider 兜底。
pub fn base_system_config() -> SystemConfig {
    let mcp = McpConfig::load().unwrap_or_else(|e| {
        warn!(error = %e, "cannot load MCP config; using empty config");
        McpConfig::empty()
    });
    SystemConfig {
        providers: ProvidersConfig {
            default_provider: String::new(),
            providers: HashMap::new(),
            web_search: None,
        },
        mcp,
    }
}

/// 启动时只校验、不修复系统层文件。
///
/// 损坏的系统层文件会让**所有团队的空间都打不开**（`WorkSpace::open_layered` →
/// `UserConfig::load_layered` → `ConfigError::TomlParse`），而那个错误不带文件
/// 路径、也不说是哪一层。入口这一条把它变成一条带绝对路径与解析位置的错误。
///
/// 解析口径与路由级读取完全一致（复用 [`crate::provider::load_user_providers`]），
/// 免得门禁在这里放过一个路由那边会 400 的文件。文件缺失是合法状态。
pub fn check_system_providers_file(path: &Path) -> Result<(), DaemonError> {
    crate::provider::load_user_providers(path).map(|_| ())
}

fn normalize_home(home: &Path) -> PathBuf {
    std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf())
}

fn default_home() -> Result<PathBuf, DaemonError> {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| DaemonError::Config("HOME is unset and PECO_HOME is not given".into()))?;
    Ok(base.join(HOME_DIR_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_env::{ENV_LOCK, EnvVar};

    #[test]
    fn default_home_uses_the_home_dir_name_constant() {
        assert_eq!(HOME_DIR_NAME, ".peco-daemon");
        let home = default_home().unwrap();
        assert!(
            home.ends_with(HOME_DIR_NAME),
            "默认 home 应以常量结尾：{}",
            home.display()
        );
    }

    /// `PECO_HOME` 仍压过 `$HOME/<HOME_DIR_NAME>`：临时实例与隔离实例全靠它。
    #[test]
    fn peco_home_env_wins_over_the_default_home() {
        let _serial = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let probe = std::fs::canonicalize(dir.path()).unwrap();

        let config = {
            let _env = EnvVar::set("PECO_HOME", &probe);
            DaemonConfig::load().unwrap()
        };

        assert_eq!(config.peco_home, probe);
        assert!(
            !config.peco_home.ends_with(HOME_DIR_NAME),
            "被覆盖时不该再落到默认 home：{}",
            config.peco_home.display()
        );
    }

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

    /// 系统层文件必须落在 daemon 自己的数据根，与 CWD 无关。
    ///
    /// 纯函数级：切 CWD 的黑盒手法会把「路径解析」和「进程状态」搅在一起，而
    /// CWD 无关性由「相对路径必须报错」那条用例判死。
    #[test]
    fn base_system_config_is_empty_and_its_root_is_peco_home() {
        let _serial = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(dir.path()).unwrap();

        let config = {
            let _env = EnvVar::set("PECO_HOME", &home);
            DaemonConfig::load().unwrap()
        };

        assert_eq!(config.system_providers_root(), home.as_path());
        assert!(
            config
                .system_providers_root()
                .join(PROVIDERS_FILE)
                .starts_with(&home),
            "系统层文件必须落在数据根之下"
        );

        let base = base_system_config();
        assert!(
            base.providers.providers.is_empty(),
            "基座不得有幽灵 provider"
        );
        assert!(base.providers.web_search.is_none());
        assert_eq!(
            base.providers.default_provider, "",
            "空串 = 本层未声明默认，与 load_user_providers 的缺省一致"
        );
    }

    /// `PECO_PROVIDERS_CONFIG` 覆盖系统层根，且三条校验任一不过即启动失败。
    ///
    /// 三条都是防「静默不生效」：文件名或父目录打错、写成相对路径，三种情况在
    /// 运行期都与「没设这个变量」表现一样，只能在启动期判死。
    #[test]
    fn providers_config_env_is_validated_before_it_is_trusted() {
        let _serial = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let isolated = tempfile::tempdir().unwrap();
        let probe = std::fs::canonicalize(isolated.path()).unwrap();

        // ① 合法：绝对路径 + basename 正确 + 父目录存在 ⇒ 系统层根 = 该父目录。
        let config = {
            let _env = EnvVar::set("PECO_PROVIDERS_CONFIG", probe.join(PROVIDERS_FILE));
            DaemonConfig::load().unwrap()
        };
        assert_eq!(config.system_providers_root(), probe.as_path());

        // ② 相对路径：含义取决于 CWD（systemd 的 WorkingDirectory 与手动启动不同）。
        let relative = Path::new(PROVIDERS_FILE);
        let err = {
            let _env = EnvVar::set("PECO_PROVIDERS_CONFIG", relative);
            DaemonConfig::load().unwrap_err()
        };
        assert_eq!(err.code(), "config_error");
        assert!(err.to_string().contains("absolute"), "{err}");

        // ③ 父目录不存在：设了 env 却读不到任何文件，与「没设」无从分辨。
        let missing_parent = probe.join("missing-dir").join(PROVIDERS_FILE);
        let err = {
            let _env = EnvVar::set("PECO_PROVIDERS_CONFIG", &missing_parent);
            DaemonConfig::load().unwrap_err()
        };
        assert_eq!(err.code(), "config_error");
        assert!(err.to_string().contains("parent"), "{err}");

        // ④ basename 不是 providers.toml：层是按目录拼文件名的，指向别的文件名
        // 只会静默读空。
        let wrong_name = probe.join("other.toml");
        let err = {
            let _env = EnvVar::set("PECO_PROVIDERS_CONFIG", &wrong_name);
            DaemonConfig::load().unwrap_err()
        };
        assert_eq!(err.code(), "config_error");
        assert!(err.to_string().contains(PROVIDERS_FILE), "{err}");
    }

    /// 启动期校验只报错、不改文件；缺文件是合法状态。
    ///
    /// 损坏的系统层文件会让所有团队的空间都打不开，而 `load_layered` 的错误不带
    /// 路径 —— 入口这条错误是唯一能把「哪一层、哪一行」说清楚的地方。
    #[test]
    fn broken_system_providers_file_is_reported_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(dir.path())
            .unwrap()
            .join(PROVIDERS_FILE);

        assert!(
            check_system_providers_file(&path).is_ok(),
            "文件缺失是合法状态，不该报错"
        );

        let broken = "providers = [not toml\n";
        std::fs::write(&path, broken).unwrap();
        let err = check_system_providers_file(&path).unwrap_err();
        let text = err.to_string();
        assert!(text.contains(&path.display().to_string()), "{text}");
        assert!(text.contains("line 1"), "解析位置必须出现在文案里：{text}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            broken,
            "校验不得改文件"
        );

        std::fs::write(
            &path,
            "default_provider = \"gw\"\n\n[providers.gw]\ntype = \"openai\"\n",
        )
        .unwrap();
        assert!(check_system_providers_file(&path).is_ok());
    }

    /// 两个都配齐才开 TLS；只配一侧只留日志，行为维持明文。
    ///
    /// 变异方法：把 `tls()` 改成「有任一就返回」 ⇒ 缺私钥那条断言变红。
    #[test]
    fn tls_needs_both_the_certificate_and_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(dir.path()).unwrap();
        let write = |toml: &str| {
            std::fs::write(home.join(CONFIG_FILE), toml).unwrap();
            DaemonConfig::load_from(&home).unwrap()
        };

        let neither = write("[daemon]\nbind_addr = \"127.0.0.1:6971\"\n");
        assert!(neither.tls().is_none());
        assert!(!neither.has_partial_tls(), "什么都没配不算配了一半");
        assert_eq!(
            neither.token_path(),
            home.join(TOKEN_FILE),
            "缺省落在数据根"
        );

        let only_cert = write("[daemon]\ntls_cert = \"cert.pem\"\n");
        assert!(only_cert.tls().is_none(), "缺私钥不得开 TLS");
        assert!(only_cert.has_partial_tls(), "配了一半必须能被识别出来");

        let both = write("[daemon]\ntls_cert = \"cert.pem\"\ntls_key = \"/etc/peco/key.pem\"\n");
        let (cert, key) = both.tls().expect("两个都配了才有 TLS");
        assert_eq!(cert, home.join("cert.pem"), "相对路径按数据根解析");
        assert_eq!(key, Path::new("/etc/peco/key.pem"), "绝对路径原样保留");
        assert!(!both.has_partial_tls());
    }

    #[test]
    fn the_token_file_can_be_relocated() {
        let dir = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(
            home.join(CONFIG_FILE),
            "[daemon]\ntoken_file = \"secrets/setup-token\"\n",
        )
        .unwrap();

        let config = DaemonConfig::load_from(&home).unwrap();
        assert_eq!(config.token_path(), home.join("secrets/setup-token"));
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
