// ============================================================================
// app — CLI 应用主循环
// ============================================================================
//
// CliApp 持有所有运行时组件，驱动交互启动流程和 REPL 循环。

use std::sync::Arc;

use peco_core::agent::{Agent, AgentLooper, LooperEvent, LooperHandle};
use peco_core::config::SystemConfig;
use peco_core::persistence::SessionPersister;
use peco_core::session::Session;
use peco_core::workspace::WorkSpace;

use crate::commands::{self, CommandRegistry, CommandResult};
use crate::config::CliConfig;
use crate::display::{ConsoleRenderer, Renderer};
use crate::input::InputReader;
use crate::menu;
use crate::session_map::AgentAwareSessionPersister;

// ============================================================================
// CliApp
// ============================================================================

/// CLI 应用主结构。
///
/// 构造时仅打开 WorkSpace 和持久化器，Agent 和 Session 在 `run()` 中
/// 通过终端菜单交互选择。
pub struct CliApp {
    /// CLI 配置
    config: CliConfig,
    /// WorkSpace 实例（管理 agents/skills/knowledge，保持生命周期）
    #[allow(dead_code)]
    workspace: Arc<WorkSpace>,
    /// Agent 实例（`run()` 中通过菜单选择后设置）
    agent: Option<Arc<Agent>>,
    /// AgentLooper 控制句柄（spawn 后持有）
    handle: Option<LooperHandle>,
    /// 当前会话 ID
    session_id: String,
    /// 当前会话描述
    session_description: String,
    /// 持久化器（带 agent 感知能力的 wrapper）
    persister: Arc<AgentAwareSessionPersister>,
    /// 命令注册表
    command_registry: CommandRegistry,
    /// 渲染器
    renderer: Box<dyn Renderer>,
    /// 输入读取器
    input: InputReader,
    /// 退出标志
    should_exit: bool,
    /// 初始 session — `run()` 中创建，spawn 时消费
    initial_session: Option<Session>,
}

impl CliApp {
    /// 创建 CliApp 实例 — 仅打开 WorkSpace 和持久化器。
    ///
    /// Agent 和 Session 在 `run()` 中通过交互菜单选择。
    pub async fn new(config: CliConfig) -> anyhow::Result<Self> {
        // ── 1. 加载系统配置 ────────────────────────────────────────────
        let system_config = SystemConfig::load();

        // ── 2. 创建 WorkSpace ──────────────────────────────────────────
        let user_id = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "cli-user".to_string());

        let workspace = Arc::new(
            WorkSpace::open(config.workspace_root.clone(), user_id, &system_config)
                .map_err(|e| anyhow::anyhow!("WorkSpace 创建失败: {e}"))?,
        );
        workspace.inject_deps();

        eprintln!(
            "[init] WorkSpace 已打开: root={}, skills={}",
            workspace.root().display(),
            workspace.skill_registry().stats().registered,
        );

        // ── 3. 创建持久化器 ────────────────────────────────────────────
        let sessions_dir = config.workspace_root.join(".peco").join("sessions");
        let persister: Arc<AgentAwareSessionPersister> = Arc::new(
            AgentAwareSessionPersister::new(sessions_dir, config.workspace_root.as_path()).await?,
        );

        // ── 4. 构建渲染器和输入（不依赖 Agent 选择）────────────────────
        let renderer: Box<dyn Renderer> = Box::new(ConsoleRenderer::new(&config));
        let input = InputReader::new()?;

        Ok(Self {
            config,
            workspace,
            agent: None,
            handle: None,
            session_id: String::new(),
            session_description: String::new(),
            persister,
            command_registry: commands::create_registry(),
            renderer,
            input,
            should_exit: false,
            initial_session: None,
        })
    }

    /// 运行完整启动流程：交互选择 Agent → 交互选择 Session → REPL。
    pub async fn run(&mut self) -> anyhow::Result<()> {
        // ── Phase 1: 选择 Agent ────────────────────────────────────────
        self.init_agent().await?;

        // ── Phase 2: 选择/创建 Session ─────────────────────────────────
        self.init_session().await?;

        // ── Phase 3: Spawn looper ──────────────────────────────────────
        self.spawn_looper().await?;

        // ── Phase 4: 打印问候 ──────────────────────────────────────────
        self.renderer.render_greeting(&self.session_id)?;

        // ── Phase 5: REPL ──────────────────────────────────────────────
        while !self.should_exit {
            match self.input.read_line("> ")? {
                Some(line) if line.is_empty() => continue,
                Some(line) if line.starts_with('/') => {
                    let registry = std::mem::take(&mut self.command_registry);
                    let result = registry.dispatch(&line, self)?;
                    self.command_registry = registry;
                    match result {
                        CommandResult::Exit => self.should_exit = true,
                        CommandResult::ReloadLooper => {
                            self.respawn_looper().await?;
                        }
                        CommandResult::Continue => {}
                    }
                }
                Some(line) => {
                    self.dispatch_user_message(line).await?;
                }
                None => {
                    // EOF (Ctrl+D) → 退出
                    self.should_exit = true;
                }
            }
        }

        self.shutdown().await?;
        Ok(())
    }

    // ── 交互初始化 ────────────────────────────────────────────────────────

    /// 显示 Agent 选择菜单并加载选中的 Agent。
    async fn init_agent(&mut self) -> anyhow::Result<()> {
        let metas = self.workspace.agent_manager().list_meta();

        if metas.is_empty() {
            anyhow::bail!(
                "workspace 中没有可用的 Agent。\n\
                 请使用 peco -t personal 初始化 workspace。\n\
                 workspace: {}",
                self.config.workspace_root.display()
            );
        }

        let workspace_root = self.config.workspace_root.display().to_string();
        let name = menu::pick_agent(&metas, &workspace_root)?;

        let agent = self
            .workspace
            .agent_manager()
            .load_cached(&name)
            .map_err(|e| anyhow::anyhow!("加载 Agent '{}' 失败: {e}", name))?;

        eprintln!(
            "[init] Agent 已加载: name={}, provider={}, model={}",
            agent.config().agent.name,
            agent.provider().name(),
            agent
                .model_config()
                .model_name
                .as_deref()
                .unwrap_or("default"),
        );

        self.agent = Some(agent);
        Ok(())
    }

    /// 显示 Session 选择菜单，恢复已有会话或创建新会话。
    async fn init_session(&mut self) -> anyhow::Result<()> {
        let agent = self
            .agent
            .as_ref()
            .expect("agent must be initialized before session");

        let agent_name = agent.config().agent.name.clone();

        // 按 agent 过滤已有会话
        let agent_sessions = self
            .persister
            .list_by_agent(&agent_name)
            .await
            .map_err(|e| anyhow::anyhow!("列出会话失败: {e}"))?;

        let picked = menu::pick_session(&agent_name, &agent_sessions)?;

        match picked {
            Some(id) => match self.persister.load(&id).await? {
                Some((snapshot, meta)) => {
                    eprintln!(
                        "[init] 恢复会话: {} ({} turns)",
                        &meta.id, meta.completed_turns
                    );
                    let session = Session::from_snapshot(
                        meta.id.clone(),
                        meta.description.clone(),
                        meta.created_at,
                        snapshot,
                    );
                    self.session_id = meta.id;
                    self.session_description = meta.description;
                    self.initial_session = Some(session);
                }
                None => {
                    anyhow::bail!("会话 {} 不存在（可能已被删除）。", id);
                }
            },
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                eprintln!("[init] 新建会话: {id}");
                // 注册 session → agent 映射
                self.persister.register_session(&id, &agent_name).await?;
                let session = Session::new(id.clone(), String::new());
                self.session_id = id;
                self.session_description = String::new();
                self.initial_session = Some(session);
            }
        }

        Ok(())
    }

    // ── 公开访问器 ────────────────────────────────────────────────────────

    /// 返回命令注册表的引用。
    pub fn commands(&self) -> &CommandRegistry {
        &self.command_registry
    }

    /// 返回会话 ID。
    #[allow(dead_code)]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 返回会话持久化器的引用。
    #[allow(dead_code)]
    pub fn persister(&self) -> &Arc<AgentAwareSessionPersister> {
        &self.persister
    }

    /// 返回渲染器的可变引用。
    #[allow(dead_code)]
    pub fn renderer_mut(&mut self) -> &mut Box<dyn Renderer> {
        &mut self.renderer
    }

    /// 请求退出 REPL。
    pub fn request_exit(&mut self) {
        self.should_exit = true;
    }

    // ── 内部方法 ──────────────────────────────────────────────────────────

    /// 发送用户消息并进入事件循环。
    async fn dispatch_user_message(&mut self, input: String) -> anyhow::Result<()> {
        let handle = match self.handle.as_ref() {
            Some(h) => h.clone(),
            None => {
                self.renderer.render_error("Agent 未初始化")?;
                return Ok(());
            }
        };

        // 发送查询
        if let Err(e) = handle.send_query(input).await {
            self.renderer.render_error(&format!("发送消息失败: {e}"))?;
            return Ok(());
        }

        // 事件循环 — 消费事件直到 TurnComplete 或 Shutdown
        loop {
            match handle.recv_event().await {
                Some(event) => {
                    let is_terminal = matches!(
                        event,
                        LooperEvent::TurnComplete { .. } | LooperEvent::Shutdown { .. }
                    );

                    self.renderer.render_event(&event)?;

                    if matches!(event, LooperEvent::Shutdown { .. }) {
                        self.should_exit = true;
                    }

                    if is_terminal {
                        break;
                    }
                }
                None => {
                    // Channel 关闭 — looper 意外退出
                    self.renderer.render_error("Agent 意外退出")?;
                    break;
                }
            }
        }

        Ok(())
    }

    /// 首次 spawn looper（消费 initial_session）。
    async fn spawn_looper(&mut self) -> anyhow::Result<()> {
        let session = self
            .initial_session
            .take()
            .ok_or_else(|| anyhow::anyhow!("initial_session 已被消费"))?;

        let agent = self
            .agent
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("agent 未初始化"))?
            .clone();

        let looper_config = self.config.to_looper_config();

        let handle = AgentLooper::spawn(
            agent,
            Box::new(session),
            looper_config,
            self.persister.clone(),
        );

        self.handle = Some(handle);
        Ok(())
    }

    /// 重新创建 looper（用于 /clear 和 session switch）。
    async fn respawn_looper(&mut self) -> anyhow::Result<()> {
        // 先关闭旧 looper
        if let Some(ref h) = self.handle {
            let _ = h.shutdown().await;
        }
        self.handle = None;

        // 创建新 session（空状态，/clear 语义）
        // description 置空 — 由 persister wrapper 在首次 save 时从 query 提取
        let session = Session::new(self.session_id.clone(), String::new());

        let agent = self
            .agent
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("agent 未初始化"))?
            .clone();

        let looper_config = self.config.to_looper_config();

        let handle = AgentLooper::spawn(
            agent,
            Box::new(session),
            looper_config,
            self.persister.clone(),
        );

        self.handle = Some(handle);
        Ok(())
    }

    /// 优雅关闭。
    async fn shutdown(&mut self) -> anyhow::Result<()> {
        if let Some(ref handle) = self.handle {
            let _ = handle.shutdown().await;
        }
        self.handle = None;

        // 保存历史
        let _ = self.input.save_history();

        Ok(())
    }
}
