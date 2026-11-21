//! 空间打开：`WorkspaceHub` 的 daemon opener 与打开决策表。
//!
//! 进程内已打开实例表只有一张 —— `peco_core::tools::WorkspaceHub`。本模块
//! 提供它的 [`DaemonOpener`]：Hub 负责路径规范化、单飞、关闭态与缓存，
//! daemon 负责 marker/DB 冲突、团队绑定、依赖注入与审计挂载。
//!
//! 决策表的所有权字段以 marker 为准（`ws_id` / `work_dir` / `team`），
//! 运行属性以 DB 为准；唯一不可自动修的是「同 work_dir 的 ws_id 不同」——
//! `d_dispatches.target_ws_id` 已经引用了 DB 的值，就地改名会切断审计与唤醒。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use async_trait::async_trait;
use peco_core::agent::{Agent, AgentError};
use peco_core::knowledge::KnowledgeManager;
use peco_core::machine::MachineRuntime;
use peco_core::tools::{
    OpenRequest, WorkspaceHandle, WorkspaceHub, WorkspaceOpenError, WorkspaceOpener,
    WorkspaceToolError,
};
use peco_core::workspace::{WorkDir, WorkSpace};

use crate::db::teams::{self, NewTeam};
use crate::db::workspaces::{self, EXIT_POLICY_INHERIT, NewWorkspace, WorkspaceRow};
use crate::error::DaemonError;
use crate::file_watcher::FileWatcher;
use crate::memory::config::MemoryConfig;
use crate::memory::{cleanup, layout, migration};
use crate::provider::layer_roots_for;
use crate::space::SpaceEnv;
use crate::space::index;
use crate::space::manifest;
use crate::space::memory_audit::SqliteMemoryAudit;

/// daemon 的打开实现：持有 DB、能力根与系统配置，按决策表落地一个实例。
pub struct DaemonOpener {
    env: Arc<SpaceEnv>,
}

impl DaemonOpener {
    pub fn new(env: Arc<SpaceEnv>) -> Self {
        Self { env }
    }

    pub fn env(&self) -> &Arc<SpaceEnv> {
        &self.env
    }

    async fn open_inner(&self, req: OpenRequest) -> Result<Arc<DaemonHandle>, DaemonError> {
        let env = self.env.as_ref();
        let OpenRequest {
            dir: work_dir,
            label: requested_label,
            config: system_config,
        } = req;
        // Hub 已把 dir 规范化成绝对路径；这里只把它当字符串键使用。
        let work_dir_str = work_dir.to_string_lossy().into_owned();

        // ── 1. 行：有则按决策表校验，无则从 marker 补建 ────────────────────
        let row = match workspaces::by_work_dir(&env.pool, &work_dir_str).await? {
            Some(row) => row,
            None => {
                self.index_from_marker(&work_dir, &work_dir_str, requested_label.as_deref())
                    .await?
            }
        };

        // 回收站里的空间行还占着 `work_dir`（UNIQUE），但它已经不该被打开：
        // 会话与记忆都挂起，打开一个「已删除」的空间只会让它半死不活地跑起来。
        if row.is_trashed() {
            return Err(DaemonError::SpaceTrashed {
                ws_id: row.id.clone(),
            });
        }

        // ── 2. marker/DB 冲突决策表 ────────────────────────────────────────
        let previously_opened = row.last_opened_at.is_some();
        let marker = manifest::read(&work_dir)?;

        let (team, marker) = match marker {
            Some(marker) => {
                let marker_ws_id = manifest::ws_id_of(&marker);
                if marker_ws_id != row.id {
                    return Err(DaemonError::MarkerDbConflict {
                        marker_ws_id,
                        db_ws_id: row.id.clone(),
                        work_dir: row.work_dir.clone(),
                    });
                }
                let team = manifest::team_id_of(&marker)?;
                ensure_team_path_matches(env, &team, &marker.team, &row.work_dir)?;
                (team, Some(marker))
            }
            None => {
                if previously_opened && !work_dir.is_dir() {
                    workspaces::set_closed_at(&env.pool, &row.id).await?;
                    return Err(DaemonError::WorkDirMissing {
                        work_dir: row.work_dir.clone(),
                    });
                }
                if previously_opened {
                    tracing::info!(ws_id = %row.id, "marker missing; rebuilding it from the DB row");
                }
                (row.team_id.clone(), None)
            }
        };

        ensure_team_indexed(env, &team).await?;
        if team != row.team_id {
            // 身份字段冲突取 marker：目录是空间的物理身份，DB 行是可重建的索引。
            tracing::warn!(
                ws_id = %row.id,
                marker_team = %team,
                db_team = %row.team_id,
                "marker wins on team binding"
            );
            workspaces::set_team(&env.pool, &row.id, &team).await?;
        }

        std::fs::create_dir_all(&work_dir).map_err(|e| DaemonError::WorkDirUnusable {
            work_dir: row.work_dir.clone(),
            reason: e.to_string(),
        })?;

        // kb_scope 一律传 team_id：同 team 的 N 个空间必须共用一套知识库命名空间。
        //
        // `WorkSpace::open_layered` 是同步阻塞的（读盘、开 LanceDB、建管理器），
        // 直接在 async worker 上调用会占住整条 runtime 线程；交给 blocking 池。
        let ws = {
            let root = env.team_root(&team);
            // provider 继承链与 provider 模块共用一处推导：打开的空间与
            // `GET .../providers` 必须看到同一份生效配置。
            let provider_layers = layer_roots_for(env, &team);
            let kb_scope = team.clone();
            let open_dir = work_dir.clone();
            let opened = tokio::task::spawn_blocking(move || {
                WorkSpace::open_layered(
                    root,
                    kb_scope,
                    system_config.as_ref(),
                    WorkDir::new(open_dir),
                    provider_layers,
                )
            })
            .await
            .map_err(|e| DaemonError::Internal {
                message: format!("workspace open task aborted: {e}"),
            })?;
            Arc::new(opened.map_err(DaemonError::Workspace)?)
        };
        ws.inject_deps();
        ws.set_memory_audit(Arc::new(SqliteMemoryAudit::new(env.pool.clone())));

        // 空间私有记忆：必须在任何 agent 加载之前注入，否则已构建的 agent 内嵌的
        // 是 None，`@memory` 子 agent 会继续读写团队库。这里只写配置、不建库 ——
        // `create_kb`/`open_kb` 会立刻加载嵌入模型，打开路径上不能付这笔开销。
        let memory_root = layout::memory_root(&env.home, &row.id);
        layout::ensure(&memory_root)?;
        let memory = layout::manager(&memory_root, &row.id);
        ws.set_memory_manager(Arc::clone(&memory));

        // 旧记忆清理挂在打开链路上：空间一打开记忆库就可用，且这是每次启动都会
        // 经过的地方。它是迁移动作，不该进请求路径 —— 全量扫描一次的开销放在
        // 打开空间时付一次，而不是每轮对话都付。
        spawn_legacy_memory_purge(Arc::clone(ws.knowledge_manager()), &team);

        // 存量记忆搬家与上面的清理同属升级动作：异步、不阻塞打开。
        spawn_legacy_memory_migration(
            Arc::clone(&self.env),
            Arc::clone(ws.knowledge_manager()),
            memory,
            memory_root,
            row.id.clone(),
            team.clone(),
        );

        // machine 执行体的观察 hook 必须在任何 machine 注册之前注入：注册时取快照，
        // 晚一步就等于「被派发的 @master 自己再派发出去的任务永远没人观察」。
        // 此时 Hub 里还没有本空间的 machine，生产路径上不存在遗漏已注册者的窗口。
        match ws.machine_runtime().upgrade() {
            Some(runtime) => runtime.set_looper_hooks(vec![env.dispatch_hook(
                row.id.clone(),
                team.clone(),
                work_dir.clone(),
            )]),
            None => tracing::warn!(
                ws_id = %row.id,
                "machine runtime is gone after open; dispatch observation not wired"
            ),
        }

        // 已有记录保持原字节（打开动作不改写创建时间/会话）；缺失时按 DB 行补写，
        // 会话 id 取自 ws_id，保证之后可重建同一个身份。
        if marker.is_none() {
            // 名称取目录末段：`label` 会被 PATCH 改掉，不能拿来命名；取不到末段
            // （如 work_dir 是根目录）才退 team_id。
            let name = work_dir
                .file_name()
                .and_then(|segment| segment.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| row.team_id.clone());
            manifest::ensure_for(&work_dir, &row.id, &env.team_root(&team), &name)?;
        }

        workspaces::touch_opened(&env.pool, &row.id).await?;

        // 磁盘上的 agent 可能还没有索引行（模板安装、手工拷贝）：打开是第一个
        // 能发现它们的地方，补不上只 warn —— 列表接口会再补一次。
        match index::ensure_agents_indexed(&env.pool, &ws, &team).await {
            Ok(0) => {}
            Ok(indexed) => tracing::info!(ws_id = %row.id, indexed, "agents indexed on open"),
            Err(e) => tracing::warn!(
                ws_id = %row.id,
                error = %e,
                "cannot index agents on open; GET /api/agents will retry"
            ),
        }

        // 监听团队根而不是空间目录：agent/skill 文件对同团队的所有空间是同一份。
        let watcher = start_file_watcher(env, &team, &ws);

        // 逻辑 label 以 DB 行为准；单用户 daemon 关闭了 Hub 的 label 校验，
        // 这里只保证展示/诊断时看到的是一个稳定值。
        let label = row
            .label
            .clone()
            .or(requested_label)
            .unwrap_or_else(|| team.clone());
        Ok(Arc::new(DaemonHandle {
            ws,
            label,
            _watcher: watcher,
        }))
    }

    /// 无 DB 行时从 `.workspace.json` 补建索引；目录没有记录则拒绝打开。
    async fn index_from_marker(
        &self,
        work_dir: &Path,
        work_dir_str: &str,
        requested_label: Option<&str>,
    ) -> Result<WorkspaceRow, DaemonError> {
        let env = self.env.as_ref();
        let marker = manifest::read(work_dir)?.ok_or_else(|| DaemonError::MarkerCorrupt {
            path: work_dir
                .join(manifest::MANIFEST_FILE)
                .to_string_lossy()
                .into_owned(),
            preview: "workspace manifest is missing; create the space via POST /api/spaces"
                .to_string(),
        })?;
        let team = manifest::team_id_of(&marker)?;
        ensure_team_path_matches(env, &team, &marker.team, work_dir_str)?;
        ensure_team_indexed(env, &team).await?;

        let ws_id = manifest::ws_id_of(&marker);
        let label = requested_label.unwrap_or(&team);
        workspaces::insert_if_absent(
            &env.pool,
            &NewWorkspace {
                id: &ws_id,
                work_dir: work_dir_str,
                team_id: &team,
                label: Some(label),
                exit_policy: EXIT_POLICY_INHERIT,
            },
        )
        .await?;

        // 并发下可能是别人先插入了这一行，读回的值才是权威。
        workspaces::by_work_dir(&env.pool, work_dir_str)
            .await?
            .ok_or_else(|| DaemonError::SpaceNotFound { ws_id })
    }
}

#[async_trait]
impl WorkspaceOpener for DaemonOpener {
    async fn open(&self, req: OpenRequest) -> Result<Arc<dyn WorkspaceHandle>, WorkspaceOpenError> {
        self.open_inner(req)
            .await
            .map(|handle| handle as Arc<dyn WorkspaceHandle>)
            .map_err(|e| WorkspaceOpenError(Box::new(e)))
    }
}

/// Hub 交出去的 daemon 句柄：底层 `WorkSpace` + 逻辑 label。
pub struct DaemonHandle {
    ws: Arc<WorkSpace>,
    label: String,
    /// 团队目录监听。只在这里被持有：实例被 Hub 摘表 ⇒ 句柄 drop ⇒ watcher 停，
    /// 「空间关闭」与「停止监听」因此是同一个动作，不需要额外的注册表。
    _watcher: Option<FileWatcher>,
}

impl WorkspaceHandle for DaemonHandle {
    fn workspace(&self) -> Arc<WorkSpace> {
        self.ws.clone()
    }

    fn user_id(&self) -> &str {
        self.ws.user_id()
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn master_agent(&self) -> Result<Arc<Agent>, AgentError> {
        self.ws.get_master_agent()
    }

    fn machine_runtime(&self) -> Weak<MachineRuntime> {
        self.ws.machine_runtime()
    }
}

/// 进程内「该团队的旧记忆已清过」的标记。
///
/// 只清进程里第一个打开的团队会漏掉其余团队 —— 每个团队有自己的
/// `@private_memory`，标签缺失的旧记忆分散在各团队的库里。按团队各记一笔，
/// 「每个库最多全量扫描一次」与「每个库都清到」同时成立。
static PURGED_TEAMS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// 该团队是否是本次进程里第一次被清理；`true` 表示调用方该真跑一次。
fn first_purge_for(team: &str) -> bool {
    let purged = PURGED_TEAMS.get_or_init(Default::default);
    let mut set = match purged.lock() {
        Ok(set) => set,
        // 锁只保护一个集合：别的线程 panic 过不该让清理永远不跑。
        Err(poisoned) => poisoned.into_inner(),
    };
    set.insert(team.to_string())
}

/// 删掉该团队记忆库里没有空间标签的旧记忆，只记日志。
///
/// 失败不冒泡：清理是升级动作，一个坏掉的知识库不该拦住空间打开。
fn spawn_legacy_memory_purge(km: Arc<KnowledgeManager>, team: &str) {
    if !first_purge_for(team) {
        return;
    }
    let config = MemoryConfig::from_env();
    let kb_name = config.kb_name.clone();
    let team = team.to_string();
    tokio::spawn(async move {
        match cleanup::purge_legacy(&km, &config).await {
            Ok(deleted) => tracing::info!(
                team = %team,
                kb = %kb_name,
                deleted,
                "legacy memories purged"
            ),
            Err(e) => tracing::warn!(
                team = %team,
                kb = %kb_name,
                error = %e,
                "legacy memory purge failed"
            ),
        }
    });
}

/// 把团队旧库里属于本空间的记忆搬进空间记忆库，只记日志。
///
/// 与 [`spawn_legacy_memory_purge`] 同位置同形状：搬家是升级动作，不该进请求
/// 路径，也不该因为一个坏掉的知识库拦住空间打开。
fn spawn_legacy_memory_migration(
    env: Arc<SpaceEnv>,
    team_km: Arc<KnowledgeManager>,
    memory: Arc<KnowledgeManager>,
    memory_root: PathBuf,
    ws_id: String,
    team: String,
) {
    if migration::is_migrated(&memory_root) {
        return;
    }
    tokio::spawn(async move {
        migrate_legacy_memory(&env, &team_km, &memory, &memory_root, &ws_id, &team).await;
    });
}

/// 搬一次存量记忆，搬完若团队再无待迁移的空间就摘牌团队级记忆库。
///
/// 公开面是为了让用例能在**同一批管理器实例**上重放这一步：InMemory 后端的
/// 记忆库是进程实例级的，另建实例只会读到空库，那样测不到真正的搬运。
pub async fn migrate_legacy_memory(
    env: &SpaceEnv,
    team_km: &Arc<KnowledgeManager>,
    memory: &Arc<KnowledgeManager>,
    memory_root: &Path,
    ws_id: &str,
    team: &str,
) -> migration::MigrationReport {
    let config = MemoryConfig::from_env();
    let report = migration::migrate(team_km, memory, memory_root, ws_id, &config).await;
    if !report.skipped && report.complete() {
        disable_team_memory_library(env, team, ws_id).await;
    }
    report
}

/// 团队级记忆库摘牌：把它的 `kb_config.json` 改名成 `.disabled`。
///
/// 摘牌是为了关闭隐私口 —— 团队库里还留着其它空间的记忆，只要它还在团队管理器
/// 的扫描范围内，agent 的知识工具就看得见。数据不动，改回名字即回滚。
///
/// 只在「该团队再没有别的空间需要迁移」时才摘：先摘牌会让后打开的空间读到
/// 「团队库不存在」，它的存量记忆就此永远搬不出来。回收站行也算在内 ——
/// 恢复之后它们仍要迁移。
async fn disable_team_memory_library(env: &SpaceEnv, team: &str, ws_id: &str) {
    let siblings = match workspaces::ids_by_team(&env.pool, team).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(team, error = %e, "cannot list the team spaces; memory library left in place");
            return;
        }
    };
    let pending: Vec<&str> = siblings
        .iter()
        .map(String::as_str)
        .filter(|id| *id != ws_id)
        .filter(|id| !migration::is_migrated(&layout::memory_root(&env.home, id)))
        .collect();
    if !pending.is_empty() {
        tracing::info!(
            team,
            ws_id,
            pending = pending.len(),
            "other spaces still need their legacy memories; team memory library left enabled"
        );
        return;
    }
    match rename_memory_kb_configs(&env.team_root(team).join("knowledge")) {
        Ok(0) => {}
        Ok(count) => tracing::info!(
            team,
            count,
            "team memory library retired; rename kb_config.json.disabled back to roll back"
        ),
        Err(e) => tracing::warn!(team, error = %e, "cannot retire the team memory library"),
    }
}

/// 把团队知识根下所有「库名是 `@private_memory`」的配置改名成 `.disabled`，返回改名数。
///
/// 按配置里的 `name` 认库而不是按目录名：模板铺的目录叫 `@private_memory`，
/// 由 API 建的同名库落在 `private_memory`，两个名字指的是同一个库。
fn rename_memory_kb_configs(knowledge_root: &Path) -> std::io::Result<usize> {
    let entries = match std::fs::read_dir(knowledge_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut renamed = 0;
    for entry in entries {
        let dir = entry?.path();
        let config = dir.join("kb_config.json");
        if !config.is_file() {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&config) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if value.get("name").and_then(serde_json::Value::as_str)
            != Some(crate::memory::config::KB_NAME)
        {
            continue;
        }
        std::fs::rename(&config, dir.join("kb_config.json.disabled"))?;
        renamed += 1;
    }
    Ok(renamed)
}

/// 启动团队根的文件监听。
///
/// Hub 是晚绑定注入的（它需要 opener，而 opener 需要 Hub 才能扇出）；没有 Hub
/// 说明装配顺序被破坏，此时只 warn 不 panic：没有监听仍能正常读写，只是外部
/// 编辑器改文件不会生效。
fn start_file_watcher(env: &SpaceEnv, team: &str, ws: &Arc<WorkSpace>) -> Option<FileWatcher> {
    let Some(hub) = env.hub() else {
        tracing::warn!(
            team,
            "space env has no workspace hub; external file edits will not be picked up"
        );
        return None;
    };
    Some(FileWatcher::start(
        env.team_root(team),
        team.to_string(),
        Arc::downgrade(ws),
        env.pool.clone(),
        Arc::downgrade(&hub),
    ))
}

/// 按 `ws_id` 打开已索引的空间；先做 daemon 侧的「目录消失」判定，再委托 Hub。
///
/// 这是 `POST /api/spaces/{ws_id}/open` 与测试的入口：Hub 的 `ensure_open`
/// 要求目录存在，而 daemon 需要把「打开过、目录又没了」归类为
/// `work_dir_missing` 并写 `closed_at`。
pub async fn ensure_open_indexed(
    env: &SpaceEnv,
    hub: &Arc<WorkspaceHub>,
    ws_id: &str,
) -> Result<Arc<dyn WorkspaceHandle>, DaemonError> {
    let row =
        workspaces::by_id(&env.pool, ws_id)
            .await?
            .ok_or_else(|| DaemonError::SpaceNotFound {
                ws_id: ws_id.to_string(),
            })?;
    if row.is_trashed() {
        return Err(DaemonError::SpaceTrashed {
            ws_id: ws_id.to_string(),
        });
    }
    let work_dir = PathBuf::from(&row.work_dir);

    if row.last_opened_at.is_some() && !work_dir.is_dir() {
        workspaces::set_closed_at(&env.pool, &row.id).await?;
        return Err(DaemonError::WorkDirMissing {
            work_dir: row.work_dir.clone(),
        });
    }

    let label = row.label.clone().unwrap_or_else(|| row.team_id.clone());
    hub.ensure_open(&work_dir, Some(&label))
        .await
        .map_err(|e| map_tool_error(e, ws_id))
}

/// `WorkspaceToolError` → daemon 稳定错误码。
///
/// opener 自己抛出的 `DaemonError` 被包在 `Open(WorkspaceOpenError(..))` 里；
/// 其余（Busy / NotOpen / DirMissing）按调用方已知的 `ws_id` 翻译。
pub fn map_tool_error(err: WorkspaceToolError, ws_id: &str) -> DaemonError {
    match err {
        WorkspaceToolError::Open(WorkspaceOpenError(boxed)) => {
            match boxed.downcast::<DaemonError>() {
                Ok(err) => *err,
                Err(other) => DaemonError::Internal {
                    message: other.to_string(),
                },
            }
        }
        WorkspaceToolError::Busy { .. } => DaemonError::WsBusy {
            ws_id: ws_id.to_string(),
        },
        WorkspaceToolError::DirMissing { path } => DaemonError::WorkDirMissing { work_dir: path },
        other => DaemonError::Internal {
            message: other.to_string(),
        },
    }
}

/// 按 `work_dir` 解析出 `ws_id`（必要时建行）。
///
/// 这是 `POST /api/spaces` 与 Hub opener 之间的唯一桥梁：并发的两次调用必须
/// 收敛到同一个 `ws_id`，否则「按 work_dir 加载」会退化成全局锁或干脆无锁。
pub async fn resolve_ws_id(
    env: &SpaceEnv,
    work_dir: &Path,
    team_id: &str,
) -> Result<String, DaemonError> {
    let work_dir = normalize_work_dir(work_dir)?;
    let work_dir_str = work_dir.to_string_lossy().into_owned();

    if let Some(row) = workspaces::by_work_dir(&env.pool, &work_dir_str).await? {
        // 回收站行仍占着 `work_dir`。销毁它再建新空间由 `POST /api/spaces` 负责；
        // 这里是「按目录解析身份」的通用入口，撞上 trashed 只能拒绝，否则会把
        // 一个已删除的空间交回调用方。
        if row.is_trashed() {
            return Err(DaemonError::SpaceTrashed { ws_id: row.id });
        }
        // 同目录换团队是改绑，不是重新打开：静默返回既有空间会让调用方以为已经
        // 绑到新团队，而 marker 与对话历史仍属于旧团队。
        if row.team_id != team_id {
            return Err(DaemonError::TeamConflict {
                work_dir: work_dir_str,
                current_team: row.team_id,
                requested_team: team_id.to_string(),
            });
        }
        return Ok(row.id);
    }

    ensure_team_indexed(env, team_id).await?;

    // marker 在时以它的会话派生 ws_id 为准：索引丢失（DB 行被删）不该换来
    // 一个新身份，否则旧台账会被判成「另一个空间的历史」。
    let candidate = match manifest::read(&work_dir)? {
        Some(value) => manifest::ws_id_of(&value),
        None => peco_core::workspace::manifest::new_ws_id(),
    };
    workspaces::insert_if_absent(
        &env.pool,
        &NewWorkspace {
            id: &candidate,
            work_dir: &work_dir_str,
            team_id,
            label: Some(team_id),
            exit_policy: EXIT_POLICY_INHERIT,
        },
    )
    .await?;

    // 并发下可能是别人先插入了这一行，读回的值才是权威。
    let row = workspaces::by_work_dir(&env.pool, &work_dir_str)
        .await?
        .ok_or_else(|| DaemonError::SpaceNotFound {
            ws_id: candidate.clone(),
        })?;
    Ok(row.id)
}

/// 团队的目录存在但索引缺失时补行：`teams/<id>/` 就是权威，别让 FK 拦住运行期使用。
///
/// 改绑团队（PATCH）也走这里：`workspaces.team_id` 有外键，只校验目录存在而不补行
/// 会在写库时撞 FK。
pub async fn ensure_team_indexed(env: &SpaceEnv, team_id: &str) -> Result<(), DaemonError> {
    if !env.team_exists(team_id) {
        return Err(team_missing(env, team_id));
    }
    if teams::get(&env.pool, team_id).await?.is_none() {
        let dir = env.team_root(team_id);
        let (display_name, template_version) = teams::meta_from_dir(&dir, team_id);
        teams::upsert(
            &env.pool,
            &NewTeam {
                id: team_id,
                display_name: &display_name,
                template_version,
            },
        )
        .await?;
        tracing::info!(team_id, "team directory had no DB row; indexed it");
    }
    Ok(())
}

/// 记录里的能力根必须就是 daemon 的 `teams/<team_id>`。
///
/// daemon 只用末段当 `team_id` 去打开实例，路径不一致等于「打开的是哪个团队」
/// 有两份答案：外部工具在别处建的空间会被静默当成 daemon 团队根下的同名团队。
/// 团队根不存在时按 [`DaemonError::TeamMissing`] 处理（能力根缺失），因为此时
/// 无从比对；记录路径指向别处（含已不存在的位置）一律 `team_path_mismatch`。
fn ensure_team_path_matches(
    env: &SpaceEnv,
    team_id: &str,
    marker_team: &str,
    work_dir: &str,
) -> Result<(), DaemonError> {
    let expected =
        std::fs::canonicalize(env.team_root(team_id)).map_err(|_| team_missing(env, team_id))?;
    match std::fs::canonicalize(marker_team) {
        Ok(recorded) if recorded == expected => Ok(()),
        _ => Err(DaemonError::TeamPathMismatch {
            work_dir: work_dir.to_string(),
            marker_team: marker_team.to_string(),
            expected: expected.to_string_lossy().into_owned(),
        }),
    }
}

fn team_missing(env: &SpaceEnv, team_id: &str) -> DaemonError {
    DaemonError::TeamMissing {
        team_id: team_id.to_string(),
        available: env.available_teams(),
    }
}

/// 规范化 `work_dir`：相对路径直接拒绝（marker 落点必须可预测）。
///
/// HTTP 层查「该 work_dir 是否已有空间行」时必须用同一个规范化结果，否则
/// `/a/b` 与 `/a/b/` 会查成两行。
pub fn normalize_work_dir(path: &Path) -> Result<PathBuf, DaemonError> {
    if !path.is_absolute() {
        return Err(DaemonError::WorkDirNotAbsolute {
            work_dir: path.display().to_string(),
        });
    }
    Ok(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
}

#[cfg(test)]
mod purge_guard_tests {
    use super::first_purge_for;

    /// 每个团队在进程内最多清理一次：重开空间不该再全量扫描一遍记忆库，
    /// 但不同团队必须各自清到（各有一份 `@private_memory`）。
    #[test]
    fn each_team_purges_at_most_once_per_process() {
        assert!(first_purge_for("purge-guard-team-a"));
        assert!(!first_purge_for("purge-guard-team-a"));
        assert!(first_purge_for("purge-guard-team-b"));
        assert!(!first_purge_for("purge-guard-team-b"));
    }
}
