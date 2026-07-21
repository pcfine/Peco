// ============================================================================
// app — CLI 应用主循环
// ============================================================================
//
// CliApp 持有所有运行时组件，驱动 REPL 循环和事件分发。

use std::sync::Arc;

use peco_core::agent::{Agent, AgentLooper, LooperEvent, LooperHandle};
use peco_core::config::SystemConfig;
use peco_core::persistence::{FileSessionPersister, NullSessionPersister, SessionPersister};
use peco_core::session::Session;
use peco_core::workspace::WorkSpace;

use crate::commands::{self, CommandRegistry, CommandResult};
use crate::config::CliConfig;
use crate::display::{ConsoleRenderer, Renderer};
use crate::input::InputReader;

// ============================================================================
// CliApp
// ============================================================================

/// CLI 应用主结构。
pub struct CliApp {
    /// CLI 配置
    config: CliConfig,
    /// WorkSpace 实例（管理 agents/skills/knowledge，保持生命周期）
    #[allow(dead_code)]
    workspace: Arc<WorkSpace>,
    /// Agent 实例
    agent: Arc<Agent>,
    /// AgentLooper 控制句柄（spawn 后持有）
    handle: Option<LooperHandle>,
    /// 当前会话 ID
    session_id: String,
    /// 当前会话描述
    session_description: String,
    /// 持久化器
    persister: Arc<dyn SessionPersister>,
    /// 命令注册表
    command_registry: CommandRegistry,
    /// 渲染器
    renderer: Box<dyn Renderer>,
    /// 输入读取器
    input: InputReader,
    /// 退出标志
    should_exit: bool,
    /// 初始 session — `new()` 中创建，`run()` 中首次 spawn 时消费
    initial_session: Option<Session>,
}

impl CliApp {
    /// 创建 CliApp 实例。
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

        eprintln!(
            "[init] WorkSpace 已打开: root={}, skills={}",
            workspace.root().display(),
            workspace
                .skill_registry()
                .read()
                .map(|r| r.stats().registered)
                .unwrap_or(0),
        );

        // ── 3. 加载 Agent ─────────────────────────────────────────────
        let agent = load_agent_from_config(&workspace, &config)?;

        eprintln!(
            "[init] Agent 已加载: name={}, path={}, provider={}, model={}",
            agent.config().agent.name,
            agent.path().display(),
            agent.provider().name(),
            agent
                .model_config()
                .model_name
                .as_deref()
                .unwrap_or("default"),
        );

        // ── 4. 创建持久化器 ────────────────────────────────────────────
        let persister: Arc<dyn SessionPersister> = if config.no_persist {
            Arc::new(NullSessionPersister)
        } else {
            match &config.sessions_dir {
                Some(dir) => Arc::new(FileSessionPersister::new(dir.clone()).await?),
                None => Arc::new(FileSessionPersister::from_env().await?),
            }
        };

        // ── 5. 创建或恢复 Session ──────────────────────────────────────
        let (session, session_id, session_description) = if let Some(ref id) = config.session_id {
            match persister.load(id).await? {
                Some((snapshot, meta)) => {
                    eprintln!("[init] 恢复会话: {id} ({} turns)", meta.completed_turns);
                    let s = Session::from_snapshot(
                        meta.id.clone(),
                        meta.description.clone(),
                        meta.created_at,
                        snapshot,
                    );
                    (s, meta.id, meta.description)
                }
                None => {
                    anyhow::bail!("会话 {id} 不存在。使用 --list-sessions 查看可用会话。");
                }
            }
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            let desc = agent.config().agent.name.clone();
            eprintln!("[init] 新建会话: {id}");
            let s = Session::new(id.clone(), desc.clone());
            (s, id, desc)
        };

        // ── 6. 构建渲染器和输入 ────────────────────────────────────────
        let renderer: Box<dyn Renderer> = Box::new(ConsoleRenderer::new(&config));

        let input = InputReader::new()?;

        Ok(Self {
            config,
            workspace,
            agent,
            handle: None,
            session_id,
            session_description,
            persister,
            command_registry: commands::create_registry(),
            renderer,
            input,
            should_exit: false,
            initial_session: Some(session),
        })
    }

    /// 运行 REPL 主循环。
    pub async fn run(&mut self) -> anyhow::Result<()> {
        // ── Spawn looper（首次使用 initial_session）─────────────────────
        self.spawn_looper().await?;

        // ── 打印问候 ───────────────────────────────────────────────────
        let sid = self.session_id.clone();
        self.renderer.render_greeting(&sid)?;

        // ── REPL ───────────────────────────────────────────────────────
        while !self.should_exit {
            match self.input.read_line("> ")? {
                Some(line) if line.is_empty() => continue,
                Some(line) if line.starts_with('/') => {
                    // 交换出 registry 以解决 borrow checker 冲突
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
    pub fn persister(&self) -> &Arc<dyn SessionPersister> {
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
        // Clone handle 避免 borrow checker 冲突
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

        let looper_config = self.config.to_looper_config();

        let handle = AgentLooper::spawn(
            self.agent.clone(),
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
        let session = Session::new(self.session_id.clone(), self.session_description.clone());

        let looper_config = self.config.to_looper_config();

        let handle = AgentLooper::spawn(
            self.agent.clone(),
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

// ============================================================================
// 辅助函数
// ============================================================================

/// 根据 CliConfig 解析并加载 Agent。
///
/// - 如果 `agent_path` 指向一个已存在的文件 → 直接从文件加载（向后兼容），
///   使用 workspace 提供的 ToolDependencies。
/// - 否则 → 视为 workspace 内的 Agent 名称，通过 workspace 加载。
fn load_agent_from_config(
    workspace: &Arc<WorkSpace>,
    config: &CliConfig,
) -> anyhow::Result<Arc<Agent>> {
    if config.agent_path.is_file() {
        // 直接路径模式（向后兼容）
        eprintln!("[init] 从文件加载 Agent: {}", config.agent_path.display());
        let deps = workspace.agent_manager().build_deps();
        let agent = Agent::from_file(
            &config.agent_path,
            workspace.config(),
            workspace.skill_registry(),
            &deps,
        )?;
        Ok(Arc::new(agent))
    } else {
        // WorkSpace 内 Agent 名称模式
        let name = config
            .agent_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("personal-assistant");
        eprintln!("[init] 从 workspace 加载 Agent: {name}");
        workspace
            .agent_manager()
            .load_cached(name)
            .map_err(|e| anyhow::anyhow!("加载 Agent '{}' 失败: {e}", name))
    }
}

/// 列出已保存的会话并退出。
pub async fn list_sessions_and_exit(config: &CliConfig) -> anyhow::Result<()> {
    let persister: Arc<dyn SessionPersister> = if config.no_persist {
        Arc::new(NullSessionPersister)
    } else {
        match &config.sessions_dir {
            Some(dir) => Arc::new(FileSessionPersister::new(dir.clone()).await?),
            None => Arc::new(FileSessionPersister::from_env().await?),
        }
    };

    let sessions = persister.list().await?;

    if sessions.is_empty() {
        println!("没有已保存的会话。");
        return Ok(());
    }

    println!(
        "\n  {:<38}  {:>6}  {:>8}  描述",
        "会话 ID", "Turns", "Tokens"
    );
    println!("  {:-<38}  {:-<6}  {:-<8}  {:-<30}", "", "", "", "");

    for meta in &sessions {
        println!(
            "  {:<38}  {:>6}  {:>8}  {}",
            meta.id, meta.completed_turns, meta.tokens_used, meta.description,
        );
    }
    println!();

    Ok(())
}
