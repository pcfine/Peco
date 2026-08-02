//! 文件监控核心实现。
//!
//! 使用 `notify` crate 监听 workspace 目录变更，
//! 500ms 防抖后根据文件路径触发对应管理器的重载。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::time::Duration;

use notify::{Event, EventKind, RecursiveMode, Watcher};
use peco_core::workspace::WorkSpace;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

/// 防抖窗口：同一文件在此时间窗口内的多次变更合并为一次处理。
const DEBOUNCE_MS: u64 = 500;

/// 根据文件路径触发的重载动作。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ReloadAction {
    /// 重载单个 Agent: agents/{name}/agent.md
    Agent(String),
    /// 重载单个 Skill: skills/{name}/SKILL.md
    Skill(String),
    /// 移除单个 Skill: skills/{name}/ 目录被删除
    SkillRemoved(String),
    /// 增量同步知识库: knowledge/{kb_name}/docs/*
    KnowledgeSync(String),
    /// 重载知识库配置: knowledge/ 目录本身变更
    KnowledgeReload,
    /// 重载 MCP 配置: mcpconfig.json
    McpConfig,
    /// 重载单个 Workflow: workflows/{name}/workflow.md
    Workflow(String),
}

/// 主监控循环。
pub async fn run(
    workspace_root: PathBuf,
    ws: Weak<WorkSpace>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    // 创建 notify 事件通道
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<notify::Result<Event>>();

    let mut watcher = match notify::recommended_watcher(move |event: notify::Result<Event>| {
        let _ = event_tx.send(event);
    }) {
        Ok(w) => w,
        Err(e) => {
            error!(error = %e, workspace = %workspace_root.display(),
                "Failed to create file watcher; file monitoring disabled for this workspace");
            return;
        }
    };

    // 递归监听各子目录以捕获深层文件变更（如 agents/{name}/agent.md）。
    let watch_targets: &[(&str, RecursiveMode)] = &[
        ("agents", RecursiveMode::Recursive),
        ("skills", RecursiveMode::Recursive),
        ("workflows", RecursiveMode::Recursive),
        ("knowledge", RecursiveMode::Recursive),
    ];

    for (dir, mode) in watch_targets {
        let path = workspace_root.join(dir);
        if path.exists()
            && let Err(e) = watcher.watch(&path, *mode)
        {
            warn!(path = %path.display(), error = %e, "Failed to watch directory");
        }
    }

    // 监听根目录下的 mcpconfig.json
    let mcp_config = workspace_root.join("mcpconfig.json");
    if mcp_config.exists() {
        let _ = watcher.watch(&mcp_config, RecursiveMode::NonRecursive);
    }

    info!(workspace = %workspace_root.display(), "File watcher started");

    // 待处理动作: action_key → (action, last_event_time)
    let mut pending: HashMap<ReloadAction, Instant> = HashMap::new();
    // 防抖定时器：每 DEBOUNCE_MS/2 检查是否有到期动作
    let mut flush_interval = tokio::time::interval(Duration::from_millis(DEBOUNCE_MS / 2));

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                info!(workspace = %workspace_root.display(), "File watcher shutting down (signal received)");
                break;
            }
            event = event_rx.recv() => {
                match event {
                    Some(Ok(event)) => {
                        let now = Instant::now();
                        for action in classify_event(&workspace_root, &event) {
                            pending.insert(action, now);
                        }
                    }
                    Some(Err(e)) => {
                        warn!(error = %e, "File watcher event error");
                    }
                    None => {
                        // 通道关闭 — watcher 已被 drop
                        debug!(workspace = %workspace_root.display(), "Watcher channel closed, exiting");
                        break;
                    }
                }
            }
            _ = flush_interval.tick() => {
                // 检查 WorkSpace 是否仍存活
                let Some(ws) = ws.upgrade() else {
                    debug!(workspace = %workspace_root.display(),
                        "WorkSpace dropped, stopping file watcher");
                    break;
                };

                // 收集已过防抖窗口的动作
                let cutoff = Instant::now() - Duration::from_millis(DEBOUNCE_MS);
                let ready: Vec<ReloadAction> = pending
                    .iter()
                    .filter(|(_, ts)| **ts <= cutoff)
                    .map(|(a, _)| a.clone())
                    .collect();

                for action in &ready {
                    pending.remove(action);
                }

                for action in &ready {
                    execute_action(&ws, action).await;
                }
            }
        }
    }

    drop(watcher);
    info!(workspace = %workspace_root.display(), "File watcher stopped");
}

/// 将文件系统事件分类为对应的重载动作。
fn classify_event(workspace_root: &Path, event: &Event) -> Vec<ReloadAction> {
    // 只处理创建、修改、删除事件
    let is_remove = matches!(event.kind, EventKind::Remove(_));
    let is_relevant = matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    );
    if !is_relevant {
        return vec![];
    }

    let mut actions = Vec::new();

    for path in &event.paths {
        let relative = match path.strip_prefix(workspace_root) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let components: Vec<_> = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect();

        match components.first().map(|s| s.as_ref()) {
            Some("agents") => {
                // agents/{name}/agent.md 或 agents/{name}/ 目录本身
                if components.len() >= 2 {
                    let name = components[1].to_string();
                    actions.push(ReloadAction::Agent(name));
                }
            }
            Some("skills") => {
                if components.len() >= 2 {
                    let name = components[1].to_string();
                    if is_remove {
                        actions.push(ReloadAction::SkillRemoved(name));
                    } else {
                        actions.push(ReloadAction::Skill(name));
                    }
                }
            }
            Some("knowledge") => {
                // knowledge/{kb_name}/docs/... → 增量同步
                // knowledge/{kb_name}/kb_config.json 等 → 重载 KB 配置
                // knowledge/{kb_name}/ → 目录创建/删除 → 重载
                if components.len() >= 4 && components[2] == "docs" {
                    let kb_name = components[1].to_string();
                    actions.push(ReloadAction::KnowledgeSync(kb_name));
                } else {
                    actions.push(ReloadAction::KnowledgeReload);
                }
            }
            Some("workflows") => {
                if components.len() >= 2 {
                    let name = components[1].to_string();
                    actions.push(ReloadAction::Workflow(name));
                }
            }
            Some("mcpconfig.json") => {
                actions.push(ReloadAction::McpConfig);
            }
            _ => {
                // 未识别的文件变更，忽略
                debug!(path = %path.display(), "Unclassified file change, ignoring");
            }
        }
    }

    actions
}

/// 对 WorkSpace 执行重载动作。
async fn execute_action(ws: &WorkSpace, action: &ReloadAction) {
    match action {
        ReloadAction::Agent(name) => {
            debug!(agent = %name, "File watcher: reloading agent");
            ws.reload_agent(name);
        }
        ReloadAction::Skill(name) => {
            debug!(skill = %name, "File watcher: reloading skill");
            ws.reload_skill(name);
        }
        ReloadAction::SkillRemoved(name) => {
            debug!(skill = %name, "File watcher: removing deleted skill");
            ws.remove_skill(name);
        }
        ReloadAction::KnowledgeSync(kb_name) => {
            debug!(kb = %kb_name, "File watcher: syncing knowledge base");
            if let Err(e) = ws.sync_knowledge(kb_name).await {
                warn!(kb = %kb_name, error = %e,
                    "File watcher: knowledge sync failed");
            }
        }
        ReloadAction::KnowledgeReload => {
            debug!("File watcher: reloading knowledge configuration");
            if let Err(e) = ws.reload_knowledge().await {
                warn!(error = %e, "File watcher: knowledge reload failed");
            }
        }
        ReloadAction::McpConfig => {
            debug!("File watcher: reloading MCP config");
            // 使用系统 MCP 配置作为 fallback（watcher 无法获取 SystemConfig）
            // WorkSpace 内部会在文件不存在/无效时 fallback 到传入值
            let fallback = peco_core::config::McpConfig {
                mcp_servers: std::collections::HashMap::new(),
            };
            let count = ws.reload_mcp_config(&fallback);
            debug!(count, "File watcher: MCP config reloaded");
        }
        ReloadAction::Workflow(name) => {
            debug!(workflow = %name, "File watcher: reloading workflow");
            if let Err(e) = ws.reload_workflow(name) {
                warn!(workflow = %name, error = %e,
                    "File watcher: workflow reload failed");
            }
        }
    }
}
