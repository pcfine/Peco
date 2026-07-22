// ============================================================================
// config — CLI 配置聚合
// ============================================================================
//
// 配置来源优先级：CLI args > 环境变量 > 默认值

use std::path::PathBuf;

use clap::Parser;
use peco_core::agent::LooperConfig;

/// CLI 聊天助手 — 基于 peco-core 的交互式 AI REPL。
///
/// 启动时通过终端菜单交互选择 Agent 和 Session，
/// 无需通过命令行参数指定。
#[derive(Parser, Debug)]
#[command(name = "peco", version, about, long_about = None)]
pub struct CliArgs {
    /// WorkSpace 根目录（包含 agents/、skills/、knowledge/ 子目录）
    #[arg(short = 'w', long, default_value = "./", env = "PECO_WORKSPACE")]
    pub workspace: PathBuf,

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
    pub workspace_root: PathBuf,
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
            workspace_root: args.workspace,
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
