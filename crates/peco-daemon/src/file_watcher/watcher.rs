//! 文件监控核心实现。
//!
//! 用 `notify` 监听团队根下的 `agents/` 与 `skills/`，500ms 去抖后：
//! - agent 变更 → 补 DB 索引（[`ensure_agents_indexed`]）+ 团队级缓存扇出
//!   （[`refresh_open_spaces`]）；
//! - skill 变更 → 只刷新本空间的注册表。
//!
//! 只监听这两棵子树：它们是 `teams/<team_id>/` 下与空间运行时状态直接相关的
//! 共享资产。知识库/工作流/provider 的变更由各自的写接口处理 —— 在团队根上
//! 监听它们会让 N 个空间对同一份目录各做一遍相同的事（N 倍重复劳动），而本期
//! 没有对应的团队级扇出。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::time::Duration;

use notify::{Event, EventKind, RecursiveMode, Watcher};
use peco_core::tools::WorkspaceHub;
use peco_core::workspace::WorkSpace;
use sqlx::SqlitePool;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

use crate::space::{fanout, index};

/// 去抖窗口：同一文件在此窗口内的多次变更合并为一次处理。
const DEBOUNCE_MS: u64 = 500;

/// 一次去抖窗口内需要执行的动作。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ReloadAction {
    /// `agents/` 下任何变更。不记名字：扇出按「磁盘名 ∪ 库内名」整体失效，
    /// 记住具体是哪个 agent 只会让去抖表变大而不改变行为。
    Agents,
    /// 重载单个 Skill: `skills/<name>/SKILL.md`。
    Skill(String),
    /// 移除单个 Skill: `skills/<name>/` 目录被删除。
    SkillRemoved(String),
}

pub async fn run(
    team_root: PathBuf,
    team_id: String,
    ws: Weak<WorkSpace>,
    pool: SqlitePool,
    hub: Weak<WorkspaceHub>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<notify::Result<Event>>();

    let mut watcher = match notify::recommended_watcher(move |event: notify::Result<Event>| {
        let _ = event_tx.send(event);
    }) {
        Ok(watcher) => watcher,
        Err(e) => {
            error!(
                error = %e,
                team_root = %team_root.display(),
                "cannot create file watcher; external edits to this team will not be picked up"
            );
            return;
        }
    };

    for subtree in ["agents", "skills"] {
        let path = team_root.join(subtree);
        if path.exists()
            && let Err(e) = watcher.watch(&path, RecursiveMode::Recursive)
        {
            warn!(path = %path.display(), error = %e, "cannot watch directory");
        }
    }
    // 目录尚不存在时（团队刚建、技能还没装）上面两行注册失败；监听团队根本身
    // 兜底，子目录被创建后由 `classify_event` 认出路径。
    if let Err(e) = watcher.watch(&team_root, RecursiveMode::NonRecursive) {
        warn!(team_root = %team_root.display(), error = %e, "cannot watch team root");
    }

    info!(team_id = %team_id, team_root = %team_root.display(), "file watcher started");

    // 待处理动作: action → 最后一次事件时间
    let mut pending: HashMap<ReloadAction, Instant> = HashMap::new();
    let mut flush_interval = tokio::time::interval(Duration::from_millis(DEBOUNCE_MS / 2));

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                info!(team_id = %team_id, "file watcher shutting down (signal received)");
                break;
            }
            event = event_rx.recv() => {
                match event {
                    Some(Ok(event)) => {
                        let now = Instant::now();
                        for action in classify_event(&team_root, &event) {
                            pending.insert(action, now);
                        }
                    }
                    Some(Err(e)) => warn!(error = %e, "file watcher event error"),
                    None => {
                        // 通道关闭：watcher 已被 drop。
                        debug!(team_id = %team_id, "watcher channel closed, exiting");
                        break;
                    }
                }
            }
            _ = flush_interval.tick() => {
                // 空间实例先于句柄消失时自行退出：watcher 不该比空间活得久。
                let Some(workspace) = ws.upgrade() else {
                    debug!(team_id = %team_id, "WorkSpace dropped, stopping file watcher");
                    break;
                };

                let cutoff = Instant::now() - Duration::from_millis(DEBOUNCE_MS);
                let ready: Vec<ReloadAction> = pending
                    .iter()
                    .filter(|(_, ts)| **ts <= cutoff)
                    .map(|(action, _)| action.clone())
                    .collect();
                for action in &ready {
                    pending.remove(action);
                }
                if ready.is_empty() {
                    continue;
                }

                // 一个去抖窗口只扇出一次：`agents/` 下的批量写入（模板安装、
                // git checkout）会一次产生几十个事件。
                if ready.iter().any(|action| matches!(action, ReloadAction::Agents)) {
                    refresh_agents(&pool, &hub, &workspace, &team_id, &team_root).await;
                }
                for action in &ready {
                    execute_skill_action(&workspace, action);
                }
            }
        }
    }

    drop(watcher);
    info!(team_id = %team_id, "file watcher stopped");
}

/// agent 变更后的两步：先把磁盘现状补进 DB 索引，再扇出缓存失效。
///
/// 顺序不能反：索引补行决定 `GET /api/agents` 能否看到新 agent，而扇出只
/// 处理缓存；反过来做会让「新文件已存在但列表没有」多停留一个去抖窗口。
async fn refresh_agents(
    pool: &SqlitePool,
    hub: &Weak<WorkspaceHub>,
    workspace: &WorkSpace,
    team_id: &str,
    team_root: &Path,
) {
    debug!(team_root = %team_root.display(), "file watcher: refreshing agents");
    if let Err(e) = index::ensure_agents_indexed(pool, workspace, team_id).await {
        // 索引补行失败不该拦住缓存失效：已经打开的空间仍应读到新正文。
        warn!(error = %e, team_root = %team_root.display(), "cannot index agents after file change");
    }
    let Some(hub) = hub.upgrade() else {
        debug!(team_id = %team_id, "hub gone; skipping agent cache fanout");
        return;
    };
    fanout::refresh_open_spaces(&hub, pool, team_id).await;
}

fn execute_skill_action(workspace: &WorkSpace, action: &ReloadAction) {
    match action {
        // agent 的缓存失效在 `refresh_agents` 里按团队完成。
        ReloadAction::Agents => {}
        ReloadAction::Skill(name) => {
            debug!(skill = %name, "file watcher: reloading skill");
            workspace.reload_skill(name);
        }
        ReloadAction::SkillRemoved(name) => {
            debug!(skill = %name, "file watcher: removing deleted skill");
            workspace.remove_skill(name);
        }
    }
}

/// 把文件系统事件分类为对应的重载动作。
fn classify_event(team_root: &Path, event: &Event) -> Vec<ReloadAction> {
    let is_remove = matches!(event.kind, EventKind::Remove(_));
    let is_relevant = matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    );
    if !is_relevant {
        return Vec::new();
    }

    let mut actions = Vec::new();
    for path in &event.paths {
        // 团队根本身的事件（新建子目录）不算资产变更：真正的文件事件随后就到。
        let Ok(relative) = path.strip_prefix(team_root) else {
            continue;
        };
        let components: Vec<String> = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect();

        match components.first().map(String::as_str) {
            Some("agents") => actions.push(ReloadAction::Agents),
            Some("skills") => {
                if let Some(name) = components.get(1) {
                    if is_remove {
                        actions.push(ReloadAction::SkillRemoved(name.clone()));
                    } else {
                        actions.push(ReloadAction::Skill(name.clone()));
                    }
                }
            }
            _ => debug!(path = %path.display(), "unclassified file change, ignoring"),
        }
    }
    actions
}
