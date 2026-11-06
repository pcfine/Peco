// ============================================================================
// workspace_tools — 工作空间的创建 / 打开 / 跨空间派发
// ============================================================================
//
// 一个 workspace 就是一个 agent team 的工作空间：目录里的 `.workspace.json`
// 记录它属于哪个团队、叫什么、要做什么；实例化后，跨空间派发永远只找该团队的
// `@master`，调用方不需要知道内部 agent / skill / MCP 的细节。
//
// 三个工具共享一个进程级 [`WorkspaceHub`]：按规范化绝对路径索引已打开的
// `WorkSpace`，同一目录只会实例化一次。工具只持 `Weak<WorkspaceHub>`，与
// `machine_runtime` 同一条纪律 —— 否则 Hub → WorkSpace → Agent → 工具 → Hub
// 会闭合，实例永远无法释放。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::Duration;

use async_trait::async_trait;
use model_provider::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::debug;
use uuid::Uuid;

use super::machine_tool::{build_task, render_cancel, render_task_await, render_task_status};
use super::{Content, StringError, ToolDyn, ToolError};
use crate::agent::{Agent, AgentError, MASTER_AGENT_NAME};
use crate::config::{SystemConfig, system_config};
use crate::machine::runtime::RuntimeError;
use crate::machine::{MachineError, MachineRuntime, ObsStream, TaskId, TaskSnapshot, TaskState};
use crate::workspace::manifest::{self, ManifestError, WorkspaceManifest};
use crate::workspace::{WorkDir, WorkSpace};

/// 派发任务在 Hub 台账里的来源标签。
const DISPATCH_ORIGIN: &str = "dispatch_to_workspace";
/// 回执正文上限：与 `run_machine_task` 的单任务预览同档。
const MAX_OUTPUT_CHARS: usize = 8 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 300;
const MAX_TIMEOUT_SECS: u64 = 3600;
const MAX_DEADLINE_SECS: u64 = 86_400;
const MAX_LABEL_CHARS: usize = 128;
const MAX_NAME_CHARS: usize = 200;
const MAX_DESCRIPTION_CHARS: usize = 8 * 1024;
const MAX_PROMPT_CHARS: usize = 64 * 1024;

// ============================================================================
// 错误
// ============================================================================

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceToolError {
    #[error("'{field}' must be an absolute path: {path}")]
    NotAbsolute { field: &'static str, path: String },
    #[error("workspace directory does not exist: {path}")]
    DirMissing { path: String },
    #[error("invalid label: {reason}")]
    InvalidLabel { reason: String },
    #[error("workspace '{dir}' is not open; pass a label to open it")]
    LabelRequired { dir: String },
    #[error("workspace '{dir}' is already open under a different label")]
    LabelMismatch { dir: String },
    #[error("workspace '{dir}' is not open; open_workspace it first")]
    NotOpen { dir: String },
    #[error("workspace '{dir}' is closing; retry after the close finishes")]
    Busy { dir: String },
    #[error("team '{team}' does not exist or is not a directory")]
    TeamMissing { team: String },
    #[error("team '{team}' has no @master agent")]
    NoMasterAgent { team: String },
    #[error("workspace manifest not found at '{path}'")]
    ManifestMissing { path: String },
    #[error("process-level SystemConfig is not installed; call install_system_config at startup")]
    ConfigUnavailable,
    #[error("process-level workspace hub is gone")]
    HubGone,
    #[error("workspace runtime is gone")]
    RuntimeGone,
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("workspace manifest error: {0}")]
    Manifest(#[from] ManifestError),
    #[error("workspace open failed: {0}")]
    Open(#[from] WorkspaceOpenError),
    #[error("machine runtime error: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("task hub error: {0}")]
    Machine(#[from] MachineError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("background open task failed: {0}")]
    Join(String),
}

impl From<WorkspaceToolError> for ToolError {
    fn from(err: WorkspaceToolError) -> Self {
        ToolError::ToolCallError(Box::new(err))
    }
}

// ============================================================================
// 公开载荷
// ============================================================================

/// 一个已创建或已打开的工作空间。刻意不含 agent / skill / mcp 名单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceInfo {
    pub dir: String,
    pub name: String,
    pub description: String,
    pub team: String,
    pub session_id: String,
    pub created_at: String,
    pub already_open: bool,
}

/// 一次跨空间派发的回执。
///
/// `Deserialize` 供观察侧从工具结果原文还原：hook 只认这个形状，不猜其它包装。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchReceipt {
    pub dispatch_id: String,
    pub task_id: String,
    pub workspace: String,
    pub state: String,
    /// 仅 `wait=true` 且任务成功终态时非空。
    pub output: Option<String>,
    /// `wait=true` 到点仍未终态：任务没有取消，只是本次没等到。
    pub timed_out: bool,
}

// ============================================================================
// Handle / Opener — WorkspaceHub 的唯一实例来源
// ============================================================================

/// 已打开工作空间的公开能力面。
///
/// daemon 会持有本 trait 对象跑会话，`label` 是可覆盖的逻辑名字（默认等于
/// `user_id`）：`WorkSpace::open` 的 `user_id` 在 daemon 里是 team / kb_scope，
/// 而 `check_label` 需要的是对外展示的 label。
pub trait WorkspaceHandle: Send + Sync {
    /// 底层 `WorkSpace`。daemon 用它跑会话。
    fn workspace(&self) -> Arc<WorkSpace>;
    /// 传给 `WorkSpace::open` 的值（daemon 里是 team_id / kb_scope）。
    fn user_id(&self) -> &str;
    /// 逻辑 label，供 `check_label` 使用。daemon 可覆盖。
    fn label(&self) -> &str {
        self.user_id()
    }
    fn master_agent(&self) -> Result<Arc<Agent>, AgentError>;
    fn machine_runtime(&self) -> Weak<MachineRuntime>;
}

/// 一次打开请求。`dir` 必须已规范化（`ensure_open` 负责）。
pub struct OpenRequest {
    pub dir: PathBuf,
    pub label: Option<String>,
    pub config: Arc<SystemConfig>,
}

/// 打开失败的统一载体：opener 可以返回任意错误源，Hub 不关心种类。
#[derive(Debug, thiserror::Error)]
#[error("workspace open failed: {0}")]
pub struct WorkspaceOpenError(pub Box<dyn std::error::Error + Send + Sync>);

impl WorkspaceOpenError {
    fn new(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self(Box::new(err))
    }
}

/// `WorkSpace::open` 的唯一调用点，抽成 trait 让测试与 daemon 可替换实现。
#[async_trait]
pub trait WorkspaceOpener: Send + Sync {
    async fn open(&self, req: OpenRequest) -> Result<Arc<dyn WorkspaceHandle>, WorkspaceOpenError>;
}

/// 默认实现：读 `.workspace.json`、校验团队，再在阻塞线程池里 `WorkSpace::open`。
struct RealOpener;

struct RealHandle {
    ws: Arc<WorkSpace>,
}

impl WorkspaceHandle for RealHandle {
    fn workspace(&self) -> Arc<WorkSpace> {
        self.ws.clone()
    }

    fn user_id(&self) -> &str {
        self.ws.user_id()
    }

    fn master_agent(&self) -> Result<Arc<Agent>, AgentError> {
        self.ws.get_master_agent()
    }

    fn machine_runtime(&self) -> Weak<MachineRuntime> {
        self.ws.machine_runtime()
    }
}

#[async_trait]
impl WorkspaceOpener for RealOpener {
    async fn open(&self, req: OpenRequest) -> Result<Arc<dyn WorkspaceHandle>, WorkspaceOpenError> {
        let label = req.label.ok_or_else(|| {
            WorkspaceOpenError::new(WorkspaceToolError::LabelRequired {
                dir: req.dir.display().to_string(),
            })
        })?;
        let dir = req.dir;
        let config = req.config;
        // `WorkSpace::open` 是同步文件 IO + 大量装配，必须离开 async 线程。
        let opened = tokio::task::spawn_blocking(move || -> Result<_, WorkspaceToolError> {
            let manifest = read_manifest(&dir)?;
            let team = validate_team(Path::new(&manifest.team))?;
            let ws = WorkSpace::open(team, label, &config, WorkDir::new(dir))
                .map_err(WorkspaceOpenError::new)?;
            let ws = Arc::new(ws);
            ws.inject_deps();
            if !ws.has_master_agent() {
                return Err(WorkspaceToolError::NoMasterAgent {
                    team: ws.root().display().to_string(),
                });
            }
            Ok(ws)
        })
        .await
        .map_err(|err| WorkspaceOpenError::new(WorkspaceToolError::Join(err.to_string())))?
        .map_err(WorkspaceOpenError::new)?;
        Ok(Arc::new(RealHandle { ws: opened }))
    }
}

// ============================================================================
// WorkspaceHub — 进程级已打开实例表
// ============================================================================

pub struct WorkspaceHub {
    /// `None` = 每次打开时从进程级 [`SystemConfig`] 读取（生产路径）。
    config: Option<Arc<SystemConfig>>,
    opener: Arc<dyn WorkspaceOpener>,
    instances: RwLock<HashMap<PathBuf, Arc<dyn WorkspaceHandle>>>,
    /// 正在关闭的目录：`get` 返回 `None`，`ensure_open` 返回 `Busy`。
    /// 实例仍留在 `instances` 里，直到 `finish_close` 摘除并在锁外 drop。
    closing: RwLock<HashSet<PathBuf>>,
    /// 按目录的单飞锁：只串行同一目录的打开/关闭，不同目录互不阻塞。
    open_locks: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    /// 是否用 `label` 做隔离校验（默认 true）。
    ///
    /// 单用户 daemon 的 label 只是展示名，工具侧的 `"local"` 与 DB 里的
    /// team_id 会天然不一致；这类宿主可关闭校验，改为「路径即身份」。
    label_enforcement: bool,
}

impl Default for WorkspaceHub {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceHub {
    pub fn new() -> Self {
        Self {
            config: None,
            opener: Arc::new(RealOpener),
            instances: RwLock::new(HashMap::new()),
            closing: RwLock::new(HashSet::new()),
            open_locks: Mutex::new(HashMap::new()),
            label_enforcement: true,
        }
    }

    /// 显式绑定配置：测试与需要固定凭据的宿主使用。
    pub fn with_system_config(config: Arc<SystemConfig>) -> Self {
        Self {
            config: Some(config),
            ..Self::new()
        }
    }

    /// 替换 opener：测试注入与 daemon 自定义打开流程使用。
    pub fn with_opener(mut self, opener: Arc<dyn WorkspaceOpener>) -> Self {
        self.opener = opener;
        self
    }

    /// 关闭 `label` 一致性校验（默认开启）。
    ///
    /// 单用户 daemon 的隔离维度是路径而不是 label：工具侧可能传 `"local"`，
    /// 而 DB 行里存的是 team_id，两者永远不相等；此时应关闭校验，否则每次
    /// 复用已打开实例都会误报 `LabelMismatch`。
    pub fn with_label_enforcement(mut self, enforce: bool) -> Self {
        self.label_enforcement = enforce;
        self
    }

    /// 创建记录文件。不打开实例 —— 打开由 `open_workspace` / 首次派发负责。
    pub fn create(
        &self,
        dir: &Path,
        team: &Path,
        name: &str,
        description: &str,
    ) -> Result<WorkspaceInfo, WorkspaceToolError> {
        let dir = create_dir(dir)?;
        let team = validate_team(team)?;
        let name = required_text(name, "name", MAX_NAME_CHARS)?;
        let description = required_text(description, "description", MAX_DESCRIPTION_CHARS)?;

        let manifest = manifest::new_manifest(&name, &description, &team);
        manifest::write_exclusive(&dir, &manifest)?;
        Ok(WorkspaceInfo::from_manifest(&dir, &manifest, false))
    }

    /// 读取 `.workspace.json` 并实例化；已打开时直接复用。
    pub async fn open(
        &self,
        dir: &Path,
        label: Option<&str>,
    ) -> Result<WorkspaceInfo, WorkspaceToolError> {
        let dir = existing_dir(dir)?;
        let (_handle, already_open) = self.ensure_open_with_status(&dir, label).await?;
        let manifest = read_manifest(&dir)?;
        Ok(WorkspaceInfo::from_manifest(&dir, &manifest, already_open))
    }

    /// 向目标工作空间的 `@master` 派发一个任务。
    ///
    /// `wait=false` 只提交并返回 task_id；`wait=true` 等到终态或 `timeout_secs`。
    /// 等待超时不是取消：任务继续在目标 workspace 的 Hub 里运行。
    #[allow(clippy::too_many_arguments)]
    pub async fn dispatch(
        &self,
        dir: &Path,
        prompt: &str,
        wait: bool,
        deadline_secs: Option<u64>,
        timeout_secs: Option<u64>,
        label: Option<&str>,
    ) -> Result<DispatchReceipt, WorkspaceToolError> {
        let prompt = required_text(prompt, "prompt", MAX_PROMPT_CHARS)?;
        if let Some(secs) = deadline_secs
            && !(1..=MAX_DEADLINE_SECS).contains(&secs)
        {
            return Err(WorkspaceToolError::InvalidArgument(format!(
                "deadline_secs must be in 1..={MAX_DEADLINE_SECS}"
            )));
        }
        let timeout = Duration::from_secs(match timeout_secs {
            Some(secs) if (1..=MAX_TIMEOUT_SECS).contains(&secs) => secs,
            Some(secs) => {
                return Err(WorkspaceToolError::InvalidArgument(format!(
                    "timeout_secs must be in 1..={MAX_TIMEOUT_SECS}, got {secs}"
                )));
            }
            None => DEFAULT_TIMEOUT_SECS,
        });

        let dir = existing_dir(dir)?;
        let handle = self.ensure_open(&dir, label).await?;

        let runtime = handle
            .machine_runtime()
            .upgrade()
            .ok_or(WorkspaceToolError::RuntimeGone)?;
        let kind = runtime.ensure_machine_named(MASTER_AGENT_NAME, || handle.master_agent())?;
        let hub = runtime.hub()?;

        let task = build_task(
            &kind,
            &prompt,
            deadline_secs,
            handle.user_id(),
            !wait,
            DISPATCH_ORIGIN,
        );
        let task_id = hub.submit(&kind, task).await?;

        let snapshot = if wait {
            match hub.await_terminal(&task_id, Some(timeout)).await {
                Ok(snapshot) => snapshot,
                // 超时与订阅失败都归为 Unavailable：补一次快照判真实状态。
                Err(err) => {
                    debug!(task_id = %task_id, error = %err, "wait ended without a terminal snapshot");
                    hub.snapshot(&task_id).await?
                }
            }
        } else {
            hub.snapshot(&task_id).await?
        };

        Ok(DispatchReceipt {
            dispatch_id: Uuid::new_v4().simple().to_string(),
            task_id: task_id.to_string(),
            workspace: dir.display().to_string(),
            state: snapshot.state.as_str().to_string(),
            output: output_of(&snapshot),
            timed_out: wait && !snapshot.state.is_terminal(),
        })
    }

    // ── A 类查询：跨空间任务（事后取结果） ──────────────────────────────

    /// 读取目标空间里的任务快照。不会自动打开空间：新建的 Hub 不认识旧 task_id，
    /// 自动打开只会把「没有」谎报成「有」。
    pub async fn task_snapshot(
        &self,
        dir: &Path,
        id: TaskId,
    ) -> Result<TaskSnapshot, WorkspaceToolError> {
        let hub = self.runtime_for(dir)?.hub()?;
        Ok(hub.snapshot(&id).await?)
    }

    /// 等待目标空间里的任务进入终态。超时只返回错误，不取消任务。
    pub async fn await_task(
        &self,
        dir: &Path,
        id: TaskId,
        timeout: Option<Duration>,
    ) -> Result<TaskSnapshot, WorkspaceToolError> {
        let hub = self.runtime_for(dir)?.hub()?;
        Ok(hub.await_terminal(&id, timeout).await?)
    }

    /// 请求取消目标空间里的任务（幂等，只记录请求）。
    pub async fn cancel_task(&self, dir: &Path, id: TaskId) -> Result<(), WorkspaceToolError> {
        let hub = self.runtime_for(dir)?.hub()?;
        hub.cancel(&id).await?;
        Ok(())
    }

    /// 订阅目标空间里的任务观察流（首帧快照，随后增量）。
    pub async fn subscribe_task(
        &self,
        dir: &Path,
        id: TaskId,
    ) -> Result<ObsStream, WorkspaceToolError> {
        let hub = self.runtime_for(dir)?.hub()?;
        Ok(hub.subscribe(&id)?)
    }

    /// 目标空间已打开时返回其 runtime；**不自动打开**。
    fn runtime_for(&self, dir: &Path) -> Result<Arc<MachineRuntime>, WorkspaceToolError> {
        let dir = canonical(dir);
        let handle = self.get(&dir).ok_or_else(|| WorkspaceToolError::NotOpen {
            dir: dir.display().to_string(),
        })?;
        handle
            .machine_runtime()
            .upgrade()
            .ok_or(WorkspaceToolError::RuntimeGone)
    }

    // ── 单表读写 / 生命周期 ─────────────────────────────────────────────

    /// 已打开的实例；关闭中或未打开返回 `None`。
    pub fn get(&self, dir: &Path) -> Option<Arc<dyn WorkspaceHandle>> {
        let dir = canonical(dir);
        if self.is_closing(&dir) {
            return None;
        }
        let handle = self
            .instances
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&dir)
            .cloned();
        // `begin_close` 可能在两次读之间落地：再查一次，避免把关闭中的实例交出去。
        if self.is_closing(&dir) { None } else { handle }
    }

    /// 该目录当前是否处于打开状态（关闭中不算）。
    pub fn is_open(&self, dir: &Path) -> bool {
        self.get(dir).is_some()
    }

    /// 确保目录已打开，返回实例；`label` 只在冷打开时必需。
    pub async fn ensure_open(
        &self,
        dir: &Path,
        label: Option<&str>,
    ) -> Result<Arc<dyn WorkspaceHandle>, WorkspaceToolError> {
        self.ensure_open_with_status(dir, label)
            .await
            .map(|(handle, _)| handle)
    }

    /// 进入关闭态：此后 `get` 返回 `None`、`ensure_open` 返回 `Busy`。
    ///
    /// 实例不在这里 drop —— 真正的析构发生在 `finish_close`，且必须在锁外。
    pub async fn begin_close(&self, dir: &Path) -> Result<(), WorkspaceToolError> {
        let dir = canonical(dir);
        let lock = self.lock_for(&dir);
        let _guard = lock.lock().await;
        self.closing
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(dir);
        Ok(())
    }

    /// 摘除实例并允许重新打开。实例的 drop 在释放全部锁之后执行。
    pub async fn finish_close(&self, dir: &Path) {
        let dir = canonical(dir);
        let lock = self.lock_for(&dir);
        let removed = {
            let _guard = lock.lock().await;
            self.closing
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&dir);
            self.instances
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&dir)
        };
        // `Arc<WorkSpace>` 析构会连带 AgentManager / Hub，可能很慢：不得持锁。
        drop(removed);
    }

    /// 立即释放一个已打开实例（不经过 Closing 状态）。
    ///
    /// 实例的 drop 在释放 `instances` 写锁之后执行，理由同 `finish_close`。
    pub fn forget(&self, dir: &Path) -> bool {
        let dir = canonical(dir);
        let removed = self
            .instances
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&dir);
        let found = removed.is_some();
        drop(removed);
        found
    }

    /// 已打开实例的数量（测试与诊断）。
    pub fn open_count(&self) -> usize {
        self.instances
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    #[cfg(test)]
    pub(crate) fn insert_for_test(&self, dir: &Path, handle: Arc<dyn WorkspaceHandle>) {
        let dir = canonical(dir);
        self.instances
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(dir, handle);
    }

    async fn ensure_open_with_status(
        &self,
        dir: &Path,
        label: Option<&str>,
    ) -> Result<(Arc<dyn WorkspaceHandle>, bool), WorkspaceToolError> {
        let dir = existing_dir(dir)?;
        let label = normalize_label(label)?;

        if let Some(handle) = self.get(&dir) {
            self.check_label(&handle, label.as_deref(), &dir)?;
            return Ok((handle, true));
        }

        // 同一目录的单飞：锁内二次查缓存，避免并发各建一份实例。
        let lock = self.lock_for(&dir);
        let _guard = lock.lock().await;
        if let Some(handle) = self.get(&dir) {
            self.check_label(&handle, label.as_deref(), &dir)?;
            return Ok((handle, true));
        }
        if self.is_closing(&dir) {
            return Err(WorkspaceToolError::Busy {
                dir: dir.display().to_string(),
            });
        }

        // 校验全部交给 opener：daemon 的 DB 行可能先于 marker 存在，label 语义也不同，
        // Hub 只负责路径规范化、缓存、关闭态与配置注入。
        let config = self.config()?;

        let handle = self
            .opener
            .open(OpenRequest {
                dir: dir.clone(),
                label,
                config,
            })
            .await
            .map_err(unwrap_open_error)?;

        self.instances
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(dir.clone(), handle.clone());
        Ok((handle, false))
    }

    fn config(&self) -> Result<Arc<SystemConfig>, WorkspaceToolError> {
        self.config
            .clone()
            .or_else(system_config)
            .ok_or(WorkspaceToolError::ConfigUnavailable)
    }

    fn check_label(
        &self,
        handle: &Arc<dyn WorkspaceHandle>,
        label: Option<&str>,
        dir: &Path,
    ) -> Result<(), WorkspaceToolError> {
        if self.label_enforcement {
            check_label(handle, label, dir)?;
        }
        Ok(())
    }

    fn is_closing(&self, dir: &Path) -> bool {
        self.closing
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(dir)
    }

    fn lock_for(&self, dir: &Path) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .open_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks
            .entry(dir.to_path_buf())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
}

static HUB: OnceLock<Arc<WorkspaceHub>> = OnceLock::new();

/// 安装进程级 Hub。只能成功一次：已初始化时返回被拒的 `Arc`，调用方须自行处理。
///
/// daemon 在启动时先安装带自定义 opener 的 Hub；之后所有工具与
/// [`workspace_hub`] 都复用同一张实例表。
pub fn install_workspace_hub(hub: Arc<WorkspaceHub>) -> Result<(), Arc<WorkspaceHub>> {
    HUB.set(hub)
}

/// 进程级 Hub：所有工具共享同一张已打开实例表。未安装时惰性创建默认实现。
pub fn workspace_hub() -> Arc<WorkspaceHub> {
    HUB.get_or_init(|| Arc::new(WorkspaceHub::new())).clone()
}

impl WorkspaceInfo {
    fn from_manifest(dir: &Path, manifest: &WorkspaceManifest, already_open: bool) -> Self {
        Self {
            dir: dir.display().to_string(),
            name: manifest.name.clone(),
            description: manifest.description.clone(),
            team: manifest.team.clone(),
            session_id: manifest.session_id.clone(),
            created_at: manifest.created_at.clone(),
            already_open,
        }
    }
}

// ============================================================================
// 参数与路径校验
// ============================================================================

fn required_text(raw: &str, field: &str, max_chars: usize) -> Result<String, WorkspaceToolError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(WorkspaceToolError::InvalidArgument(format!(
            "{field} must not be empty"
        )));
    }
    if text.chars().count() > max_chars {
        return Err(WorkspaceToolError::InvalidArgument(format!(
            "{field} exceeds {max_chars} characters"
        )));
    }
    Ok(text.to_string())
}

fn normalize_label(label: Option<&str>) -> Result<Option<String>, WorkspaceToolError> {
    let Some(raw) = label else {
        return Ok(None);
    };
    let text = raw.trim();
    if text.is_empty() {
        return Err(WorkspaceToolError::InvalidLabel {
            reason: "must not be empty".to_string(),
        });
    }
    if text.chars().count() > MAX_LABEL_CHARS {
        return Err(WorkspaceToolError::InvalidLabel {
            reason: format!("must be at most {MAX_LABEL_CHARS} characters"),
        });
    }
    Ok(Some(text.to_string()))
}

fn check_label(
    handle: &Arc<dyn WorkspaceHandle>,
    label: Option<&str>,
    dir: &Path,
) -> Result<(), WorkspaceToolError> {
    if let Some(label) = label
        && label != handle.label()
    {
        return Err(WorkspaceToolError::LabelMismatch {
            dir: dir.display().to_string(),
        });
    }
    Ok(())
}

/// 创建目录并规范化；用于 `create_workspace`（目录允许不存在）。
fn create_dir(dir: &Path) -> Result<PathBuf, WorkspaceToolError> {
    if !dir.is_absolute() {
        return Err(WorkspaceToolError::NotAbsolute {
            field: "dir",
            path: dir.display().to_string(),
        });
    }
    std::fs::create_dir_all(dir)?;
    Ok(std::fs::canonicalize(dir)?)
}

/// 表键的规范化：失败时退回原路径（目录消失只影响 `get` 是否命中）。
fn canonical(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// 已存在的目录必须规范化成唯一路径：`/a/b`、`/a/b/`、`/a/c/../b` 是同一个空间。
fn existing_dir(dir: &Path) -> Result<PathBuf, WorkspaceToolError> {
    if !dir.is_absolute() {
        return Err(WorkspaceToolError::NotAbsolute {
            field: "dir",
            path: dir.display().to_string(),
        });
    }
    let canonical = std::fs::canonicalize(dir).map_err(|_| WorkspaceToolError::DirMissing {
        path: dir.display().to_string(),
    })?;
    if !canonical.is_dir() {
        return Err(WorkspaceToolError::DirMissing {
            path: dir.display().to_string(),
        });
    }
    Ok(canonical)
}

/// 团队能力根：必须是已存在的绝对目录，且装有 `@master`。
///
/// 先校验再 `WorkSpace::open` —— 后者会无条件 `create_dir_all`，拼错路径
/// 会静默造出一个空团队。
fn validate_team(team: &Path) -> Result<PathBuf, WorkspaceToolError> {
    if !team.is_absolute() {
        return Err(WorkspaceToolError::NotAbsolute {
            field: "team",
            path: team.display().to_string(),
        });
    }
    let canonical = std::fs::canonicalize(team).map_err(|_| WorkspaceToolError::TeamMissing {
        team: team.display().to_string(),
    })?;
    if !canonical.is_dir() {
        return Err(WorkspaceToolError::TeamMissing {
            team: team.display().to_string(),
        });
    }
    if !master_path(&canonical).is_file() {
        return Err(WorkspaceToolError::NoMasterAgent {
            team: canonical.display().to_string(),
        });
    }
    Ok(canonical)
}

fn master_path(team: &Path) -> PathBuf {
    team.join("agents").join(MASTER_AGENT_NAME).join("agent.md")
}

fn read_manifest(dir: &Path) -> Result<WorkspaceManifest, WorkspaceToolError> {
    manifest::read(dir)?.ok_or_else(|| WorkspaceToolError::ManifestMissing {
        path: manifest::manifest_path(dir).display().to_string(),
    })
}

/// opener 错误回到工具错误域。
///
/// `RealOpener` 抛出的就是 [`WorkspaceToolError`]（`LabelRequired` / `ManifestMissing` /
/// `NoMasterAgent` 等），解包后原样归还，既有工具与测试看到的变体不变；daemon 自定义
/// opener 的错误源不是本类型，包成 `Open` 保留原始错误链，由 Phase 3 自行 downcast。
fn unwrap_open_error(err: WorkspaceOpenError) -> WorkspaceToolError {
    let WorkspaceOpenError(boxed) = err;
    match boxed.downcast::<WorkspaceToolError>() {
        Ok(inner) => *inner,
        Err(boxed) => WorkspaceToolError::Open(WorkspaceOpenError(boxed)),
    }
}

fn output_of(snapshot: &crate::machine::TaskSnapshot) -> Option<String> {
    if snapshot.state != TaskState::Completed {
        return None;
    }
    let output = snapshot.outcome.as_ref()?.output.as_ref()?;
    if output.tag != "text" {
        return None;
    }
    output
        .payload
        .as_str()
        .map(|text| truncate_chars(text, MAX_OUTPUT_CHARS))
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max).collect();
    truncated.push('…');
    truncated
}

// ============================================================================
// create_workspace
// ============================================================================

pub struct CreateWorkspace {
    hub: Weak<WorkspaceHub>,
}

impl CreateWorkspace {
    pub fn new(hub: Weak<WorkspaceHub>) -> Self {
        Self { hub }
    }
}

impl ToolDyn for CreateWorkspace {
    fn name(&self) -> String {
        "create_workspace".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "create_workspace".to_string(),
            description: "Create a workspace record at dir/.workspace.json. A workspace is one \
                agent team's working space: the file records the team root, the workspace name, \
                its task description, a session id and the creation time. The directory is created \
                when missing; an existing .workspace.json is never overwritten. This only writes \
                the record — use open_workspace to instantiate it."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["dir", "team", "name", "description"],
                "properties": {
                    "dir": {
                        "type": "string",
                        "description": "Absolute path of the workspace directory. Created when missing."
                    },
                    "team": {
                        "type": "string",
                        "description": "Absolute path of the team root (contains agents/, skills/, knowledge/, workflows/ and agents/@master/agent.md)."
                    },
                    "name": {
                        "type": "string",
                        "description": "Short human-readable workspace name."
                    },
                    "description": {
                        "type": "string",
                        "description": "The concrete task this workspace is meant to accomplish."
                    }
                }
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Args {
                dir: String,
                team: String,
                name: String,
                description: String,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let hub = self.hub.upgrade().ok_or(WorkspaceToolError::HubGone)?;
            let info = hub.create(
                Path::new(&parsed.dir),
                Path::new(&parsed.team),
                &parsed.name,
                &parsed.description,
            )?;
            Ok(Content::Text(serde_json::to_string(&info)?))
        })
    }
}

// ============================================================================
// open_workspace
// ============================================================================

pub struct OpenWorkspace {
    hub: Weak<WorkspaceHub>,
}

impl OpenWorkspace {
    pub fn new(hub: Weak<WorkspaceHub>) -> Self {
        Self { hub }
    }
}

impl ToolDyn for OpenWorkspace {
    fn name(&self) -> String {
        "open_workspace".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "open_workspace".to_string(),
            description: "Read dir/.workspace.json and instantiate that workspace, returning its \
                record. label is the acting user id and is required on the first open; opening an \
                already-open workspace with no label reuses the existing instance, and a different \
                label is rejected. The instance stays open in this process until forgotten."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["dir", "label"],
                "properties": {
                    "dir": {
                        "type": "string",
                        "description": "Absolute path of the workspace directory containing .workspace.json."
                    },
                    "label": {
                        "type": "string",
                        "description": "Acting user id (external label). Used as the workspace user id / knowledge scope on the first open."
                    }
                }
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Args {
                dir: String,
                label: String,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let hub = self.hub.upgrade().ok_or(WorkspaceToolError::HubGone)?;
            let info = hub
                .open(Path::new(&parsed.dir), Some(&parsed.label))
                .await?;
            Ok(Content::Text(serde_json::to_string(&info)?))
        })
    }
}

// ============================================================================
// dispatch_to_workspace
// ============================================================================

pub struct DispatchToWorkspace {
    hub: Weak<WorkspaceHub>,
}

impl DispatchToWorkspace {
    pub fn new(hub: Weak<WorkspaceHub>) -> Self {
        Self { hub }
    }
}

impl ToolDyn for DispatchToWorkspace {
    fn name(&self) -> String {
        "dispatch_to_workspace".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "dispatch_to_workspace".to_string(),
            description:
                "Hand a task to a workspace's team master (@master) and return a receipt. \
                The target is addressed by its directory; it is opened automatically when needed, \
                in which case label is required. By default this does not wait: it returns \
                dispatch_id and task_id right after submission. Set wait=true to block until the \
                task is terminal or timeout_secs (default 300) elapses; a wait timeout never \
                cancels the task. Callers cannot choose internal agents."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["dir", "prompt"],
                "properties": {
                    "dir": {
                        "type": "string",
                        "description": "Absolute path of the target workspace directory."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The complete task description handed to the team master as one user message. It cannot see this conversation."
                    },
                    "wait": {
                        "type": "boolean",
                        "default": false,
                        "description": "false (default): return right after submission. true: block until the task is terminal or timeout_secs elapses."
                    },
                    "deadline_secs": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_DEADLINE_SECS,
                        "description": "Optional wall-clock budget for the dispatched task."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_TIMEOUT_SECS,
                        "default": DEFAULT_TIMEOUT_SECS,
                        "description": "Upper bound for the wait phase when wait=true. Expiry is not a cancellation."
                    },
                    "label": {
                        "type": "string",
                        "description": "Acting user id; only needed when the target workspace is not open yet."
                    }
                }
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Args {
                dir: String,
                prompt: String,
                #[serde(default)]
                wait: bool,
                deadline_secs: Option<u64>,
                timeout_secs: Option<u64>,
                label: Option<String>,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let hub = self.hub.upgrade().ok_or(WorkspaceToolError::HubGone)?;
            let receipt = hub
                .dispatch(
                    Path::new(&parsed.dir),
                    &parsed.prompt,
                    parsed.wait,
                    parsed.deadline_secs,
                    parsed.timeout_secs,
                    parsed.label.as_deref(),
                )
                .await?;
            Ok(Content::Text(serde_json::to_string(&receipt)?))
        })
    }
}

// ============================================================================
// inspect_workspace_task
// ============================================================================

/// 跨空间任务的事后查询：`status` / `await` / `cancel`。
pub struct InspectWorkspaceTask {
    hub: Weak<WorkspaceHub>,
}

impl InspectWorkspaceTask {
    pub fn new(hub: Weak<WorkspaceHub>) -> Self {
        Self { hub }
    }
}

impl ToolDyn for InspectWorkspaceTask {
    fn name(&self) -> String {
        "inspect_workspace_task".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "inspect_workspace_task".to_string(),
            description: "Observe a task previously submitted with dispatch_to_workspace in \
                another workspace. dir is the workspace path from the dispatch receipt and that \
                workspace must still be open in this process; use open_workspace first if it was \
                never opened here. action=status reads the current state and, once terminal, the \
                final answer. action=await waits until the task is terminal or timeout_secs \
                (default 300) elapses; a wait timeout never cancels the task. action=cancel \
                requests cancellation; it is idempotent and returns once the request is recorded, \
                not when the task converged. Tasks submitted with wait=false are not canceled by \
                the parent turn ending."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["dir", "task_id", "action"],
                "properties": {
                    "dir": {
                        "type": "string",
                        "description": "Absolute path of the target workspace (the `workspace` field of the dispatch receipt). It must still be open."
                    },
                    "task_id": {
                        "type": "string",
                        "description": "The task id returned by dispatch_to_workspace."
                    },
                    "action": {
                        "type": "string",
                        "enum": ["status", "await", "cancel"],
                        "description": "status: read the current state. await: wait for a terminal state or timeout. cancel: request cancellation."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_TIMEOUT_SECS,
                        "default": DEFAULT_TIMEOUT_SECS,
                        "description": "Upper bound for action=await. Expiry is not a cancellation."
                    }
                }
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Args {
                dir: String,
                task_id: String,
                action: String,
                timeout_secs: Option<u64>,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let action = parsed.action.as_str();
            if !matches!(action, "status" | "await" | "cancel") {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "action must be one of: status, await, cancel".to_string(),
                ))));
            }
            let id = Uuid::parse_str(&parsed.task_id).map(TaskId).map_err(|_| {
                ToolError::ToolCallError(Box::new(StringError(format!(
                    "invalid task_id \"{}\": expected the uuid returned by dispatch_to_workspace",
                    parsed.task_id
                ))))
            })?;
            let timeout = match parsed.timeout_secs {
                Some(secs) if (1..=MAX_TIMEOUT_SECS).contains(&secs) => Duration::from_secs(secs),
                Some(secs) => {
                    return Err(ToolError::ToolCallError(Box::new(StringError(format!(
                        "timeout_secs must be in 1..={MAX_TIMEOUT_SECS}, got {secs}"
                    )))));
                }
                None => Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            };

            let dir = PathBuf::from(&parsed.dir);
            let hub = self.hub.upgrade().ok_or(WorkspaceToolError::HubGone)?;

            match action {
                "status" => {
                    let snapshot = hub
                        .task_snapshot(&dir, id)
                        .await
                        .map_err(|err| task_error(&dir, id, err))?;
                    Ok(Content::Text(render_task_status(&snapshot)))
                }
                "await" => {
                    let (snapshot, timed_out) = match hub.await_task(&dir, id, Some(timeout)).await
                    {
                        Ok(snapshot) => (snapshot, false),
                        // 超时与订阅失效都归为 Unavailable：补一次快照判真实状态。
                        Err(WorkspaceToolError::Machine(MachineError::Unavailable { reason })) => {
                            debug!(task_id = %id, error = %reason, "await ended without a terminal snapshot");
                            let snapshot = hub
                                .task_snapshot(&dir, id)
                                .await
                                .map_err(|err| task_error(&dir, id, err))?;
                            let timed_out = !snapshot.state.is_terminal();
                            (snapshot, timed_out)
                        }
                        Err(err) => return Err(task_error(&dir, id, err)),
                    };
                    Ok(Content::Text(render_task_await(&snapshot, timed_out)))
                }
                _ => {
                    // 请求前快照是 `already_terminal` 的唯一判据。
                    let before = hub
                        .task_snapshot(&dir, id)
                        .await
                        .map_err(|err| task_error(&dir, id, err))?;
                    hub.cancel_task(&dir, id)
                        .await
                        .map_err(|err| task_error(&dir, id, err))?;
                    Ok(Content::Text(render_cancel(id, before.state)))
                }
            }
        })
    }
}

/// 任务不存在时给出本工具自己的文案；空间未打开等其余错误原样透出。
fn task_error(dir: &Path, id: TaskId, err: WorkspaceToolError) -> ToolError {
    match err {
        WorkspaceToolError::Machine(MachineError::NotFound { .. }) => {
            ToolError::ToolCallError(Box::new(StringError(format!(
                "task '{id}' not found in workspace '{}'",
                dir.display()
            ))))
        }
        other => other.into(),
    }
}

// ============================================================================
// 测试 — marker / Hub 单飞 / label / 派发（脚本化 provider，零真实模型调用）
// ============================================================================

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::*;
    use crate::agent::test_support::{
        GateTool, completed, scripted_agent, scripted_agent_with_tools, tool_call_response,
    };
    use crate::config::{McpConfig, ProvidersConfig};
    use crate::machine::{HubConfig, TaskId};
    use crate::workspace::manifest::{MANIFEST_FILE, MANIFEST_FORMAT};

    fn test_config() -> SystemConfig {
        SystemConfig {
            providers: ProvidersConfig {
                default_provider: "deepseek".to_string(),
                providers: HashMap::new(),
                web_search: None,
            },
            mcp: McpConfig::empty(),
        }
    }

    /// 最小团队：`agents/@master/agent.md` 存在即可满足打开前的校验。
    fn make_team() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write_master(dir.path());
        dir
    }

    fn write_master(team: &Path) {
        let master = team.join("agents").join(MASTER_AGENT_NAME);
        std::fs::create_dir_all(&master).unwrap();
        std::fs::write(
            master.join("agent.md"),
            "---\nagent:\n  name: \"@master\"\n  description: \"d\"\nllm:\n  provider: \"deepseek\"\n  model: \"m\"\n---\n# master\n",
        )
        .unwrap();
    }

    fn test_runtime() -> Arc<MachineRuntime> {
        Arc::new(MachineRuntime::with_hub_config(HubConfig {
            emit_min_interval: Duration::ZERO,
            scan_interval: Duration::from_millis(10),
            terminal_grace: Duration::from_millis(50),
            cancel_grace: Duration::from_millis(50),
            ..HubConfig::default()
        }))
    }

    struct FakeHandle {
        user_id: String,
        agent: Arc<Agent>,
        runtime: Arc<MachineRuntime>,
    }

    impl WorkspaceHandle for FakeHandle {
        fn workspace(&self) -> Arc<WorkSpace> {
            unimplemented!("fake handles have no real WorkSpace")
        }

        fn user_id(&self) -> &str {
            &self.user_id
        }

        fn master_agent(&self) -> Result<Arc<Agent>, AgentError> {
            Ok(self.agent.clone())
        }

        fn machine_runtime(&self) -> Weak<MachineRuntime> {
            Arc::downgrade(&self.runtime)
        }
    }

    fn fake_handle(
        user_id: &str,
        agent: Arc<Agent>,
        runtime: Arc<MachineRuntime>,
    ) -> Arc<dyn WorkspaceHandle> {
        runtime
            .ensure_machine_named(MASTER_AGENT_NAME, || Ok(agent.clone()))
            .expect("register the master machine");
        Arc::new(FakeHandle {
            user_id: user_id.to_string(),
            agent,
            runtime,
        })
    }

    struct FakeOpener {
        calls: AtomicUsize,
        agent: Arc<Agent>,
        runtime: Arc<MachineRuntime>,
        /// 命中目录的打开会被卡住，直到 `release`：用于验证不同目录互不阻塞。
        gate: Option<(PathBuf, Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    }

    impl FakeOpener {
        fn new(agent: Arc<Agent>, runtime: Arc<MachineRuntime>) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                agent,
                runtime,
                gate: None,
            })
        }

        fn gated(
            agent: Arc<Agent>,
            runtime: Arc<MachineRuntime>,
            dir: PathBuf,
        ) -> (
            Arc<Self>,
            Arc<tokio::sync::Notify>,
            Arc<tokio::sync::Notify>,
        ) {
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let opener = Arc::new(Self {
                calls: AtomicUsize::new(0),
                agent,
                runtime,
                gate: Some((dir, entered.clone(), release.clone())),
            });
            (opener, entered, release)
        }
    }

    #[async_trait]
    impl WorkspaceOpener for FakeOpener {
        async fn open(
            &self,
            req: OpenRequest,
        ) -> Result<Arc<dyn WorkspaceHandle>, WorkspaceOpenError> {
            // label 是 opener 的职责（Hub 不再代做）：与 RealOpener 同语义，
            // 同时让 Hub 的 downcast 解包路径被用例覆盖。
            let Some(label) = req.label else {
                return Err(WorkspaceOpenError::new(WorkspaceToolError::LabelRequired {
                    dir: req.dir.display().to_string(),
                }));
            };
            // 只数真正被接受的打开：冷开缺 label 的拒绝不算一次打开。
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some((dir, entered, release)) = &self.gate
                && *dir == req.dir
            {
                entered.notify_one();
                release.notified().await;
            }
            Ok(fake_handle(
                &label,
                self.agent.clone(),
                self.runtime.clone(),
            ))
        }
    }

    fn fake_hub(
        agent: Arc<Agent>,
        runtime: Arc<MachineRuntime>,
    ) -> (Arc<WorkspaceHub>, Arc<FakeOpener>) {
        let opener = FakeOpener::new(agent, runtime);
        let hub = Arc::new(
            WorkspaceHub::with_system_config(Arc::new(test_config()))
                .with_opener(opener.clone() as Arc<dyn WorkspaceOpener>),
        );
        (hub, opener)
    }

    /// 返回 team / dir 两个 tempdir：它们必须活到用例结束，否则打开时路径已消失。
    fn create_workspace(
        hub: &WorkspaceHub,
    ) -> (tempfile::TempDir, tempfile::TempDir, WorkspaceInfo) {
        let team = make_team();
        let dir = tempfile::tempdir().unwrap();
        let info = hub
            .create(dir.path(), team.path(), "demo", "把迁移做完")
            .expect("create");
        (team, dir, info)
    }

    #[test]
    fn create_writes_the_manifest_and_rejects_a_second_create() {
        let hub = WorkspaceHub::with_system_config(Arc::new(test_config()));
        let team = make_team();
        let dir = tempfile::tempdir().unwrap();

        let info = hub
            .create(dir.path(), team.path(), "demo", "把迁移做完")
            .expect("create");
        assert_eq!(info.name, "demo");
        assert_eq!(info.description, "把迁移做完");
        assert!(!info.already_open);

        let raw = std::fs::read_to_string(dir.path().join(MANIFEST_FILE)).unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["format"], json!(MANIFEST_FORMAT));
        assert_eq!(value["team"], json!(info.team));
        assert!(!raw.contains("work_dir"));

        let err = hub
            .create(dir.path(), team.path(), "again", "another")
            .expect_err("an existing manifest must be rejected");
        assert!(
            matches!(
                err,
                WorkspaceToolError::Manifest(ManifestError::AlreadyExists { .. })
            ),
            "{err}"
        );
    }

    #[test]
    fn create_requires_absolute_paths_and_a_team_with_master() {
        let hub = WorkspaceHub::with_system_config(Arc::new(test_config()));
        let dir = tempfile::tempdir().unwrap();
        let team = make_team();

        let relative = Path::new("relative/dir");
        assert!(matches!(
            hub.create(relative, team.path(), "n", "d"),
            Err(WorkspaceToolError::NotAbsolute { field: "dir", .. })
        ));
        assert!(matches!(
            hub.create(dir.path(), Path::new("relative/team"), "n", "d"),
            Err(WorkspaceToolError::NotAbsolute { field: "team", .. })
        ));

        let empty_team = tempfile::tempdir().unwrap();
        assert!(matches!(
            hub.create(dir.path(), empty_team.path(), "n", "d"),
            Err(WorkspaceToolError::NoMasterAgent { .. })
        ));
        assert!(matches!(
            hub.create(dir.path(), Path::new("/definitely/missing"), "n", "d"),
            Err(WorkspaceToolError::TeamMissing { .. })
        ));
    }

    #[tokio::test]
    async fn open_is_single_flight_and_reuses_one_instance() {
        let (agent, _provider) = scripted_agent(vec![]);
        let (hub, opener) = fake_hub(agent, test_runtime());
        let (_team, _dir, info) = create_workspace(&hub);
        let dir = Path::new(&info.dir);

        let first = hub.open(dir, Some("alice")).await.expect("open");
        assert!(!first.already_open);
        assert_eq!(opener.calls.load(Ordering::SeqCst), 1);

        let second = hub.open(dir, Some("alice")).await.expect("reopen");
        assert!(second.already_open);
        assert_eq!(opener.calls.load(Ordering::SeqCst), 1);

        // 不传 label 复用；换 label 报错，不静默改知识作用域。
        assert!(
            hub.open(dir, None)
                .await
                .expect("open without label")
                .already_open
        );
        assert!(matches!(
            hub.open(dir, Some("bob")).await,
            Err(WorkspaceToolError::LabelMismatch { .. })
        ));

        // 并发打开同一目录：opener 只调用一次。
        let (_team2, _dir2, info2) = create_workspace(&hub);
        let dir2 = Path::new(&info2.dir);
        let (a, b) = tokio::join!(hub.open(dir2, Some("u")), hub.open(dir2, Some("u")));
        a.expect("first concurrent open");
        b.expect("second concurrent open");
        assert_eq!(
            opener.calls.load(Ordering::SeqCst),
            2,
            "one call per workspace"
        );
    }

    #[tokio::test]
    async fn a_cold_open_requires_a_label() {
        let (agent, _provider) = scripted_agent(vec![]);
        let (hub, opener) = fake_hub(agent, test_runtime());
        let (_team, _dir, info) = create_workspace(&hub);

        let err = hub
            .open(Path::new(&info.dir), None)
            .await
            .expect_err("a cold open without label must be rejected");
        assert!(
            matches!(err, WorkspaceToolError::LabelRequired { .. }),
            "{err}"
        );
        assert_eq!(opener.calls.load(Ordering::SeqCst), 0);
    }

    /// 委托边界：Hub 不读 marker、不强制 label，opener 说了算；非本域错误包成 `Open`。
    #[tokio::test]
    async fn a_custom_opener_owns_validation_and_errors() {
        struct CustomOpener {
            agent: Arc<Agent>,
            runtime: Arc<MachineRuntime>,
            fail: bool,
        }

        #[async_trait]
        impl WorkspaceOpener for CustomOpener {
            async fn open(
                &self,
                req: OpenRequest,
            ) -> Result<Arc<dyn WorkspaceHandle>, WorkspaceOpenError> {
                if self.fail {
                    return Err(WorkspaceOpenError::new(std::io::Error::other(
                        "daemon marker is missing",
                    )));
                }
                Ok(fake_handle(
                    req.label.as_deref().unwrap_or("daemon"),
                    self.agent.clone(),
                    self.runtime.clone(),
                ))
            }
        }

        let (agent, _provider) = scripted_agent(vec![]);
        let runtime = test_runtime();
        let dir = tempfile::tempdir().unwrap();

        // 没有 .workspace.json、也没有 label：默认 opener 会拒，自定义 opener 可以接受。
        let opener = Arc::new(CustomOpener {
            agent: agent.clone(),
            runtime: runtime.clone(),
            fail: false,
        });
        let hub = WorkspaceHub::with_system_config(Arc::new(test_config()))
            .with_opener(opener.clone() as Arc<dyn WorkspaceOpener>);
        let handle = hub
            .ensure_open(dir.path(), None)
            .await
            .expect("the opener decides what a valid open is");
        assert_eq!(handle.label(), "daemon");

        // 非 WorkspaceToolError 的错误源原样保留在 `Open` 里。
        let dir2 = tempfile::tempdir().unwrap();
        let failing = Arc::new(CustomOpener {
            agent,
            runtime,
            fail: true,
        });
        let hub = WorkspaceHub::with_system_config(Arc::new(test_config()))
            .with_opener(failing as Arc<dyn WorkspaceOpener>);
        let err = match hub.ensure_open(dir2.path(), Some("alice")).await {
            Ok(_) => panic!("the opener failure must surface"),
            Err(err) => err,
        };
        match err {
            WorkspaceToolError::Open(WorkspaceOpenError(inner)) => {
                assert!(
                    inner.to_string().contains("daemon marker is missing"),
                    "{inner}"
                );
            }
            other => panic!("expected a wrapped Open error, got {other}"),
        }
    }

    #[tokio::test]
    async fn open_rejects_a_manifest_without_master() {
        let team = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(team.path().join("agents")).unwrap();
        let hub = WorkspaceHub::with_system_config(Arc::new(test_config()));
        let dir = tempfile::tempdir().unwrap();
        let manifest = manifest::new_manifest("demo", "task", team.path());
        manifest::write(dir.path(), &manifest).unwrap();

        let err = hub
            .open(dir.path(), Some("alice"))
            .await
            .expect_err("a team without @master must not open");
        assert!(
            matches!(err, WorkspaceToolError::NoMasterAgent { .. }),
            "{err}"
        );
    }

    #[tokio::test]
    async fn the_real_opener_instantiates_a_workspace_once() {
        let team = make_team();
        let dir = tempfile::tempdir().unwrap();
        let hub = WorkspaceHub::with_system_config(Arc::new(test_config()));
        let info = hub
            .create(dir.path(), team.path(), "demo", "task")
            .expect("create");

        let opened = hub
            .open(Path::new(&info.dir), Some("alice"))
            .await
            .expect("open");
        assert!(!opened.already_open);
        assert_eq!(hub.open_count(), 1);

        let again = hub
            .open(Path::new(&info.dir), Some("alice"))
            .await
            .expect("reopen");
        assert!(again.already_open);
    }

    #[tokio::test]
    async fn dispatch_wait_true_returns_the_master_output() {
        let (agent, _provider) = scripted_agent(vec![completed("done")]);
        let runtime = test_runtime();
        let hub = WorkspaceHub::with_system_config(Arc::new(test_config()));
        let dir = tempfile::tempdir().unwrap();
        hub.insert_for_test(dir.path(), fake_handle("alice", agent, runtime));

        let receipt = hub
            .dispatch(dir.path(), "hi", true, None, Some(5), None)
            .await
            .expect("dispatch");

        assert_eq!(
            receipt.state, "completed",
            "the receipt state is the model-facing name, not Debug"
        );
        assert_eq!(receipt.output.as_deref(), Some("done"));
        assert!(!receipt.timed_out);
        assert!(!receipt.task_id.is_empty());
        assert_eq!(
            receipt.workspace,
            std::fs::canonicalize(dir.path())
                .unwrap()
                .display()
                .to_string()
        );
    }

    #[tokio::test]
    async fn dispatch_without_wait_returns_a_task_id_and_keeps_running() {
        let gate = GateTool::new("echo", Content::Text("x".to_string()));
        let entered = gate.entered.clone();
        let release = gate.release.clone();
        let (agent, _provider) = scripted_agent_with_tools(
            vec![
                tool_call_response("c1", "echo", json!({})),
                completed("done"),
            ],
            vec![Box::new(gate)],
        );
        let runtime = test_runtime();
        let hub = WorkspaceHub::with_system_config(Arc::new(test_config()));
        let dir = tempfile::tempdir().unwrap();
        hub.insert_for_test(dir.path(), fake_handle("alice", agent, runtime.clone()));

        let receipt = hub
            .dispatch(dir.path(), "hi", false, None, None, None)
            .await
            .expect("dispatch");
        assert!(!receipt.task_id.is_empty());
        assert_eq!(receipt.output, None);
        assert!(
            !receipt.timed_out,
            "wait=false never reports a wait timeout"
        );

        // 释放门闸后任务仍能跑完：异步提交不取消。
        entered.notified().await;
        release.notify_one();
        let task_id = TaskId(receipt.task_id.parse().expect("task id is a uuid"));
        let snapshot = runtime
            .hub()
            .expect("hub")
            .await_terminal(&task_id, Some(Duration::from_secs(2)))
            .await
            .expect("terminal snapshot");
        assert_eq!(snapshot.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn a_wait_timeout_does_not_cancel_the_task() {
        let gate = GateTool::new("echo", Content::Text("x".to_string()));
        let entered = gate.entered.clone();
        let release = gate.release.clone();
        let (agent, _provider) = scripted_agent_with_tools(
            vec![
                tool_call_response("c1", "echo", json!({})),
                completed("done"),
            ],
            vec![Box::new(gate)],
        );
        let runtime = test_runtime();
        let hub = WorkspaceHub::with_system_config(Arc::new(test_config()));
        let dir = tempfile::tempdir().unwrap();
        hub.insert_for_test(dir.path(), fake_handle("alice", agent, runtime.clone()));

        let receipt = hub
            .dispatch(dir.path(), "hi", true, None, Some(1), None)
            .await
            .expect("dispatch");
        assert!(
            receipt.timed_out,
            "the task was still gated when the wait expired"
        );
        assert_eq!(
            receipt.state, "working",
            "a task gated on a tool call is still working, and the receipt must say so in lowercase"
        );

        entered.notified().await;
        release.notify_one();
        let task_id = TaskId(receipt.task_id.parse().expect("task id is a uuid"));
        let snapshot = runtime
            .hub()
            .expect("hub")
            .await_terminal(&task_id, Some(Duration::from_secs(2)))
            .await
            .expect("terminal snapshot");
        assert_eq!(snapshot.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn dispatch_auto_opens_with_a_label() {
        let (agent, _provider) = scripted_agent(vec![completed("done")]);
        let (hub, opener) = fake_hub(agent, test_runtime());
        let (_team, _dir, info) = create_workspace(&hub);
        let dir = Path::new(&info.dir);

        let err = hub
            .dispatch(dir, "hi", true, None, Some(5), None)
            .await
            .expect_err("a cold dispatch without label must be rejected");
        assert!(
            matches!(err, WorkspaceToolError::LabelRequired { .. }),
            "{err}"
        );

        let receipt = hub
            .dispatch(dir, "hi", true, None, Some(5), Some("alice"))
            .await
            .expect("dispatch with auto-open");
        assert_eq!(receipt.output.as_deref(), Some("done"));
        assert_eq!(opener.calls.load(Ordering::SeqCst), 1);
    }

    /// 卡在第一次工具调用上的 @master：`release` 之前任务停在 `Working`。
    fn gated_master_agent() -> (
        Arc<Agent>,
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
    ) {
        let gate = GateTool::new("echo", Content::Text("x".to_string()));
        let entered = gate.entered.clone();
        let release = gate.release.clone();
        let (agent, _provider) = scripted_agent_with_tools(
            vec![
                tool_call_response("c1", "echo", json!({})),
                completed("done"),
            ],
            vec![Box::new(gate)],
        );
        (agent, entered, release)
    }

    #[tokio::test]
    async fn different_directories_do_not_block_each_other() {
        let (agent, _provider) = scripted_agent(vec![]);
        let runtime = test_runtime();
        let setup = WorkspaceHub::with_system_config(Arc::new(test_config()));
        let (_team, _dir_a, info_a) = create_workspace(&setup);
        let (_team_b, _dir_b, info_b) = create_workspace(&setup);
        let dir_a = canonical(Path::new(&info_a.dir));
        let dir_b = canonical(Path::new(&info_b.dir));

        let (opener, entered, release) = FakeOpener::gated(agent, runtime, dir_a.clone());
        let hub = Arc::new(
            WorkspaceHub::with_system_config(Arc::new(test_config()))
                .with_opener(opener as Arc<dyn WorkspaceOpener>),
        );

        // A 卡在 opener 里：B 仍必须能完成，证明锁是按目录分片的。
        let opening_a = {
            let hub = Arc::clone(&hub);
            let dir = dir_a.clone();
            tokio::spawn(async move { hub.open(&dir, Some("u")).await })
        };
        entered.notified().await;
        tokio::time::timeout(Duration::from_secs(1), hub.open(&dir_b, Some("u")))
            .await
            .expect("opening a different directory must not wait for the gated one")
            .expect("open B");

        release.notify_one();
        opening_a
            .await
            .expect("the gated open must not panic")
            .expect("open A");
    }

    #[tokio::test]
    async fn closing_hides_the_instance_until_it_finishes() {
        let (agent, _provider) = scripted_agent(vec![]);
        let (hub, opener) = fake_hub(agent, test_runtime());
        let (_team, _dir, info) = create_workspace(&hub);
        let dir = Path::new(&info.dir);

        hub.open(dir, Some("alice")).await.expect("open");
        assert!(hub.is_open(dir));
        assert_eq!(opener.calls.load(Ordering::SeqCst), 1);

        hub.begin_close(dir).await.expect("begin close");
        assert!(!hub.is_open(dir), "a closing space is not observable");
        assert!(hub.get(dir).is_none());
        assert!(
            matches!(
                hub.ensure_open(dir, Some("alice")).await,
                Err(WorkspaceToolError::Busy { .. })
            ),
            "ensure_open must not resurrect a closing instance"
        );
        assert_eq!(
            opener.calls.load(Ordering::SeqCst),
            1,
            "a closing space must not be reopened"
        );

        hub.finish_close(dir).await;
        assert!(hub.get(dir).is_none());
        let reopened = hub
            .open(dir, Some("alice"))
            .await
            .expect("reopen after finish_close");
        assert!(!reopened.already_open);
        assert_eq!(opener.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn task_snapshot_and_await_cross_the_hub() {
        let (agent, entered, release) = gated_master_agent();
        let runtime = test_runtime();
        let hub = WorkspaceHub::with_system_config(Arc::new(test_config()));
        let dir = tempfile::tempdir().unwrap();
        hub.insert_for_test(dir.path(), fake_handle("alice", agent, runtime.clone()));

        let receipt = hub
            .dispatch(dir.path(), "hi", false, None, None, None)
            .await
            .expect("dispatch");
        let id = TaskId(receipt.task_id.parse().expect("task id"));

        let live = hub.task_snapshot(dir.path(), id).await.expect("snapshot");
        assert!(
            matches!(live.state, TaskState::Submitted | TaskState::Working),
            "unexpected state: {:?}",
            live.state
        );

        entered.notified().await;
        release.notify_one();
        let terminal = hub
            .await_task(dir.path(), id, Some(Duration::from_secs(2)))
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.state, TaskState::Completed);
        assert_eq!(
            terminal
                .outcome
                .and_then(|outcome| outcome.output)
                .and_then(|output| output.payload.as_str().map(str::to_string))
                .as_deref(),
            Some("done")
        );
    }

    #[tokio::test]
    async fn cancel_task_reaches_the_target_workspace() {
        let (agent, entered, _release) = gated_master_agent();
        let runtime = test_runtime();
        let hub = WorkspaceHub::with_system_config(Arc::new(test_config()));
        let dir = tempfile::tempdir().unwrap();
        hub.insert_for_test(dir.path(), fake_handle("alice", agent, runtime.clone()));

        let receipt = hub
            .dispatch(dir.path(), "hi", false, None, None, None)
            .await
            .expect("dispatch");
        let id = TaskId(receipt.task_id.parse().expect("task id"));
        entered.notified().await;

        hub.cancel_task(dir.path(), id)
            .await
            .expect("cancel request");
        let terminal = hub
            .await_task(dir.path(), id, Some(Duration::from_secs(2)))
            .await
            .expect("cancel converges");
        assert_eq!(terminal.state, TaskState::Canceled);
    }

    #[tokio::test]
    async fn task_queries_require_an_open_workspace_and_a_known_id() {
        let (agent, _provider) = scripted_agent(vec![]);
        let (hub, _opener) = fake_hub(agent, test_runtime());
        let closed = tempfile::tempdir().unwrap();
        let unknown = TaskId(Uuid::new_v4());

        let err = hub
            .task_snapshot(closed.path(), unknown)
            .await
            .expect_err("an unopened space must be reported");
        assert!(matches!(err, WorkspaceToolError::NotOpen { .. }), "{err}");

        let (_team, _dir, info) = create_workspace(&hub);
        hub.open(Path::new(&info.dir), Some("alice"))
            .await
            .expect("open");
        let err = hub
            .task_snapshot(Path::new(&info.dir), unknown)
            .await
            .expect_err("an id this workspace never issued must be NotFound");
        assert!(
            matches!(
                err,
                WorkspaceToolError::Machine(MachineError::NotFound { .. })
            ),
            "{err}"
        );
    }

    // ── inspect_workspace_task ──────────────────────────────────────────

    async fn inspect_text(tool: &InspectWorkspaceTask, args: Value) -> String {
        match tool.call(args.to_string()).await {
            Ok(Content::Text(text)) => text,
            other => panic!("expected plain text, got {other:?}"),
        }
    }

    fn report_json(text: &str) -> Value {
        let (body, _) = crate::artifacts::strip_artifact_lines(text);
        serde_json::from_str(&body).expect("the report body must be JSON")
    }

    #[tokio::test]
    async fn inspect_workspace_task_reports_status_await_and_cancel() {
        let (agent, _provider) = scripted_agent(vec![completed("done")]);
        let runtime = test_runtime();
        let hub = Arc::new(WorkspaceHub::with_system_config(Arc::new(test_config())));
        let dir = tempfile::tempdir().unwrap();
        hub.insert_for_test(dir.path(), fake_handle("alice", agent, runtime.clone()));
        let receipt = hub
            .dispatch(dir.path(), "hi", true, None, Some(5), None)
            .await
            .expect("dispatch");

        let tool = InspectWorkspaceTask::new(Arc::downgrade(&hub));

        let status = report_json(
            &inspect_text(
                &tool,
                json!({
                    "dir": receipt.workspace,
                    "task_id": receipt.task_id,
                    "action": "status"
                }),
            )
            .await,
        );
        assert_eq!(status["action"], json!("status"));
        assert_eq!(status["found"], json!(true));
        assert_eq!(status["task"]["state"], json!("completed"));
        assert_eq!(status["task"]["output"], json!("done"));

        let awaited = report_json(
            &inspect_text(
                &tool,
                json!({
                    "dir": receipt.workspace,
                    "task_id": receipt.task_id,
                    "action": "await",
                    "timeout_secs": 2
                }),
            )
            .await,
        );
        assert_eq!(awaited["action"], json!("await"));
        assert_eq!(awaited["timed_out"], json!(false));
        assert_eq!(awaited["task"]["state"], json!("completed"));

        let cancel = report_json(
            &inspect_text(
                &tool,
                json!({
                    "dir": receipt.workspace,
                    "task_id": receipt.task_id,
                    "action": "cancel"
                }),
            )
            .await,
        );
        assert_eq!(cancel["accepted"], json!(true));
        assert_eq!(cancel["already_terminal"], json!(true));
        assert_eq!(cancel["state"], json!("completed"));
    }

    #[tokio::test]
    async fn inspect_workspace_task_cancel_stops_a_running_task() {
        let (agent, entered, _release) = gated_master_agent();
        let runtime = test_runtime();
        let hub = Arc::new(WorkspaceHub::with_system_config(Arc::new(test_config())));
        let dir = tempfile::tempdir().unwrap();
        hub.insert_for_test(dir.path(), fake_handle("alice", agent, runtime.clone()));
        let receipt = hub
            .dispatch(dir.path(), "hi", false, None, None, None)
            .await
            .expect("dispatch");
        let id = TaskId(receipt.task_id.parse().expect("task id"));
        entered.notified().await;

        let tool = InspectWorkspaceTask::new(Arc::downgrade(&hub));
        let cancel = report_json(
            &inspect_text(
                &tool,
                json!({
                    "dir": receipt.workspace,
                    "task_id": receipt.task_id,
                    "action": "cancel"
                }),
            )
            .await,
        );
        assert_eq!(cancel["accepted"], json!(true));
        assert_eq!(cancel["already_terminal"], json!(false));

        let terminal = hub
            .await_task(dir.path(), id, Some(Duration::from_secs(2)))
            .await
            .expect("cancel converges");
        assert_eq!(terminal.state, TaskState::Canceled);
    }

    #[tokio::test]
    async fn inspect_workspace_task_rejects_closed_spaces_and_unknown_ids() {
        let (agent, _provider) = scripted_agent(vec![]);
        let (hub, _opener) = fake_hub(agent, test_runtime());
        let tool = InspectWorkspaceTask::new(Arc::downgrade(&hub));
        let closed = tempfile::tempdir().unwrap();
        let unknown = Uuid::new_v4();

        let err = tool
            .call(
                json!({
                    "dir": closed.path().display().to_string(),
                    "task_id": unknown.to_string(),
                    "action": "status"
                })
                .to_string(),
            )
            .await
            .expect_err("an unopened space must be an error");
        assert!(
            err.to_string().contains("is not open"),
            "an unopened space must name itself: {err}"
        );

        let (_team, _dir, info) = create_workspace(&hub);
        hub.open(Path::new(&info.dir), Some("alice"))
            .await
            .expect("open");
        let err = tool
            .call(
                json!({
                    "dir": info.dir,
                    "task_id": unknown.to_string(),
                    "action": "status"
                })
                .to_string(),
            )
            .await
            .expect_err("an unknown task must be an error");
        assert!(
            err.to_string().contains("not found in workspace"),
            "an unknown id must say where it was looked up: {err}"
        );

        let err = tool
            .call(
                json!({
                    "dir": info.dir,
                    "task_id": unknown.to_string(),
                    "action": "list"
                })
                .to_string(),
            )
            .await
            .expect_err("unknown actions must be rejected");
        assert_eq!(
            err.to_string(),
            "action must be one of: status, await, cancel"
        );
    }

    #[test]
    fn inspect_workspace_task_definition_matches_the_contract() {
        let definition = InspectWorkspaceTask::new(Weak::new()).definition();
        assert_eq!(definition.name, "inspect_workspace_task");
        assert_eq!(
            definition.parameters["required"],
            json!(["dir", "task_id", "action"])
        );
        assert_eq!(
            definition.parameters["properties"]["action"]["enum"],
            json!(["status", "await", "cancel"])
        );
        assert_eq!(
            definition.parameters["properties"]["timeout_secs"]["maximum"],
            json!(MAX_TIMEOUT_SECS)
        );
    }

    #[test]
    fn dispatch_definition_hides_internal_agents() {
        let tool = DispatchToWorkspace::new(Weak::new());
        let def = tool.definition();

        assert_eq!(def.name, "dispatch_to_workspace");
        assert_eq!(def.parameters["required"], json!(["dir", "prompt"]));
        assert!(
            def.parameters["properties"].get("agent_name").is_none(),
            "callers must not choose internal agents"
        );
        assert_eq!(
            def.parameters["properties"]["wait"]["default"],
            json!(false)
        );

        let text =
            tool.call(r#"{"dir":"/tmp/ws","prompt":"go","agent_name":"@coder"}"#.to_string());
        let err = futures::executor::block_on(text).expect_err("unknown fields must be rejected");
        assert!(matches!(err, ToolError::JsonError(_)), "{err}");
    }

    #[test]
    fn create_and_open_definitions_match_the_contract() {
        let create = CreateWorkspace::new(Weak::new());
        assert_eq!(
            create.definition().parameters["required"],
            json!(["dir", "team", "name", "description"])
        );

        let open = OpenWorkspace::new(Weak::new());
        assert_eq!(
            open.definition().parameters["required"],
            json!(["dir", "label"])
        );
    }
}
