// ============================================================================
// config — CLI 配置聚合
// ============================================================================
//
// 配置来源优先级：CLI args > 环境变量 > 默认值

use std::path::PathBuf;

use clap::Parser;
use peco_core::agent::LooperConfig;

/// CLI 聊天助手 — 基于 peco-core 的交互式 AI REPL。
#[derive(Parser, Debug)]
#[command(name = "peco", version, about, long_about = None)]
pub struct CliArgs {
    /// Agent 名称（从 workspace 的 agents/ 目录加载）或 agent.md 文件路径
    #[arg(
        short = 'a',
        long,
        default_value = "personal-assistant",
        env = "PECO_AGENT_PATH"
    )]
    pub agent: PathBuf,

    /// WorkSpace 根目录（包含 agents/、skills/、knowledge/ 子目录）
    #[arg(short = 'w', long, default_value = "./", env = "PECO_WORKSPACE")]
    pub workspace: PathBuf,

    /// 恢复指定 ID 的会话
    #[arg(short = 's', long)]
    pub session: Option<String>,

    /// 禁用会话持久化
    #[arg(long)]
    pub no_persist: bool,

    /// 列出已保存的会话并退出
    #[arg(long)]
    pub list_sessions: bool,

    /// 会话存储目录
    #[arg(long, env = "PC_AGENT_SESSIONS_DIR")]
    pub sessions_dir: Option<PathBuf>,

    /// 禁用彩色输出
    #[arg(long, env = "NO_COLOR")]
    pub no_color: bool,

    /// 显示推理过程
    #[arg(long, default_value = "true")]
    pub show_reasoning: bool,

    /// 显示工具调用
    #[arg(long, default_value = "true")]
    pub show_tools: bool,

    /// 从内置模板初始化 workspace（personal / minimal / developer）
    #[arg(short = 't', long, env = "PECO_INIT_TEMPLATE")]
    pub init_template: Option<String>,
}

/// CLI 完整配置，聚合所有配置来源。
#[derive(Debug, Clone)]
pub struct CliConfig {
    pub agent_path: PathBuf,
    pub workspace_root: PathBuf,
    pub session_id: Option<String>,
    pub no_persist: bool,
    pub list_sessions: bool,
    pub sessions_dir: Option<PathBuf>,
    pub no_color: bool,
    pub show_reasoning: bool,
    pub show_tools: bool,
    pub init_template: Option<String>,
}

impl CliConfig {
    /// 从 CLI args + 环境变量聚合配置。
    pub fn from_args_and_env() -> anyhow::Result<Self> {
        let args = CliArgs::parse();

        Ok(Self {
            agent_path: args.agent,
            workspace_root: args.workspace,
            session_id: args.session,
            no_persist: args.no_persist,
            list_sessions: args.list_sessions,
            sessions_dir: args.sessions_dir,
            no_color: args.no_color,
            show_reasoning: args.show_reasoning,
            show_tools: args.show_tools,
            init_template: args.init_template,
        })
    }

    /// 转换为 `LooperConfig`。
    pub fn to_looper_config(&self) -> LooperConfig {
        LooperConfig::default()
    }
}
