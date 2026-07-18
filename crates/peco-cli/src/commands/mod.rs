// ============================================================================
// commands — Slash 命令系统
// ============================================================================
//
// 采用 trait + 注册表模式。每个命令是独立的 struct 实现 CliCommand trait。
// CommandRegistry 负责解析和分发。

use crate::app::CliApp;

// ============================================================================
// CommandResult
// ============================================================================

/// 命令执行上下文 — 提供命令所需的全部依赖。
pub struct CommandContext<'a> {
    pub app: &'a mut CliApp,
}

/// 命令执行结果。
pub enum CommandResult {
    /// 命令已完成，继续 REPL
    Continue,
    /// 退出 REPL
    Exit,
    /// 刷新 looper（/clear 等需要重启 looper 的命令）
    ReloadLooper,
}

// ============================================================================
// CliCommand trait
// ============================================================================

/// CLI 命令 trait。
///
/// 每个命令实现此 trait，通过 CommandRegistry 注册。
pub trait CliCommand: Send + Sync {
    /// 主命令名（不含 `/` 前缀）。
    fn name(&self) -> &str;

    /// 别名列表。
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// 简短描述（用于 `/help` 列表）。
    fn description(&self) -> &str;

    /// 用法说明（用于 `/help <命令>`）。
    fn usage(&self) -> &str {
        ""
    }

    /// 执行命令。
    ///
    /// `args` 是命令名之后的部分（可能为空字符串）。
    fn execute(
        &self,
        args: &str,
        ctx: &mut CommandContext<'_>,
    ) -> anyhow::Result<CommandResult>;
}

// ============================================================================
// CommandRegistry
// ============================================================================

/// 命令注册表。
///
/// 持有所有已注册的命令，负责解析输入并分发执行。
pub struct CommandRegistry {
    commands: Vec<Box<dyn CliCommand>>,
}

impl CommandRegistry {
    /// 创建空注册表。
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    /// 注册一个命令。
    pub fn register(mut self, cmd: impl CliCommand + 'static) -> Self {
        self.commands.push(Box::new(cmd));
        self
    }

    /// 所有已注册命令的引用。
    pub fn all(&self) -> &[Box<dyn CliCommand>] {
        &self.commands
    }

    /// 解析输入并分发到对应命令。
    ///
    /// 输入格式: `/命令名 [args...]`
    pub fn dispatch(
        &self,
        input: &str,
        app: &mut CliApp,
    ) -> anyhow::Result<CommandResult> {
        let input = input
            .strip_prefix('/')
            .unwrap_or(input)
            .trim();
        let (name, args) = input.split_once(char::is_whitespace)
            .map(|(n, a)| (n, a.trim()))
            .unwrap_or((input, ""));

        let name_lower = name.to_lowercase();

        // 按名称或别名查找
        for cmd in &self.commands {
            if cmd.name() == name_lower
                || cmd.aliases().iter().any(|a| a == &name_lower)
            {
                let mut ctx = CommandContext { app };
                return cmd.execute(args, &mut ctx);
            }
        }

        // 未找到命令
        eprintln!("未知命令: /{name}。输入 /help 查看可用命令。");
        Ok(CommandResult::Continue)
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 子模块
// ============================================================================

mod exit;
mod help;

pub use exit::ExitCommand;
pub use help::HelpCommand;

/// 创建包含所有内置命令的注册表。
pub fn create_registry() -> CommandRegistry {
    CommandRegistry::new()
        .register(ExitCommand)
        .register(HelpCommand)
}
