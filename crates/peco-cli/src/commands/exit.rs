// ============================================================================
// exit — /exit, /quit 命令
// ============================================================================

use super::{CliCommand, CommandContext, CommandResult};

pub struct ExitCommand;

impl CliCommand for ExitCommand {
    fn name(&self) -> &str {
        "exit"
    }

    fn aliases(&self) -> &[&str] {
        &["quit", "q"]
    }

    fn description(&self) -> &str {
        "退出程序"
    }

    fn usage(&self) -> &str {
        "/exit  —  退出 CLI 聊天助手"
    }

    fn execute(
        &self,
        _args: &str,
        ctx: &mut CommandContext<'_>,
    ) -> anyhow::Result<CommandResult> {
        ctx.app.request_exit();
        Ok(CommandResult::Exit)
    }
}
