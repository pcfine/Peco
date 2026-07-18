// ============================================================================
// help — /help, /h 命令
// ============================================================================

use console::style;

use super::{CliCommand, CommandContext, CommandResult};

pub struct HelpCommand;

impl CliCommand for HelpCommand {
    fn name(&self) -> &str {
        "help"
    }

    fn aliases(&self) -> &[&str] {
        &["h", "?"]
    }

    fn description(&self) -> &str {
        "显示帮助信息"
    }

    fn usage(&self) -> &str {
        "/help [命令名]  —  显示全部命令或单个命令详情"
    }

    fn execute(
        &self,
        args: &str,
        ctx: &mut CommandContext<'_>,
    ) -> anyhow::Result<CommandResult> {
        if args.is_empty() {
            print_all_commands(ctx);
        } else {
            print_command_detail(ctx, args);
        }
        Ok(CommandResult::Continue)
    }
}

fn print_all_commands(ctx: &mut CommandContext<'_>) {
    println!();
    println!("  {}  —  peco CLI 聊天助手", style("命令列表").bold());
    println!();
    println!("  {:<20}  {}", style("命令").bold(), style("说明").bold());
    println!("  {:-<20}  {:-<40}", "", "");

    for cmd in ctx.app.commands().all() {
        let name = format!("/{}", cmd.name());
        let aliases = cmd.aliases();
        let alias_str = if aliases.is_empty() {
            String::new()
        } else {
            format!("  ({})", aliases.iter().map(|a| format!("/{a}")).collect::<Vec<_>>().join(", "))
        };
        println!("  {:<20}  {}{}", style(name).cyan(), cmd.description(), style(alias_str).dim());
    }

    println!();
    println!("  {}", style("输入 /help <命令> 查看命令详细用法。").dim());
    println!();
}

fn print_command_detail(ctx: &mut CommandContext<'_>, name: &str) {
    let name_lower = name.to_lowercase();
    for cmd in ctx.app.commands().all() {
        if cmd.name() == name_lower || cmd.aliases().contains(&name_lower.as_str()) {
            println!();
            println!("  {}", style(format!("/{}", cmd.name())).cyan().bold());
            println!("  {}", cmd.description());
            if !cmd.usage().is_empty() {
                println!("  用法: {}", style(cmd.usage()).dim());
            }
            if !cmd.aliases().is_empty() {
                println!(
                    "  别名: {}",
                    style(cmd.aliases().join(", ")).dim()
                );
            }
            println!();
            return;
        }
    }
    eprintln!("未知命令: /{name}");
}
