// ============================================================================
// menu — 终端交互菜单
// ============================================================================
//
// 提供 Agent 选择和 Session 选择的终端 UI。
// 使用 `console::Term` 渲染彩色框线，`rustyline::Editor` 读取数字输入。

use std::io::Write;

use console::{Term, style};
use peco_core::agent::AgentMeta;
use peco_core::session::SessionMeta;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

// ============================================================================
// 相对时间格式化
// ============================================================================

/// 将 Unix 时间戳转为人类可读的相对时间字符串。
fn format_relative_time(timestamp: u64) -> String {
    let now = chrono::Utc::now().timestamp() as u64;
    let secs = now.saturating_sub(timestamp);

    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else if secs < 2592000 {
        format!("{}d ago", secs / 86400)
    } else {
        format!("{}mo ago", secs / 2592000)
    }
}

// ============================================================================
// 输入辅助
// ============================================================================

/// 使用 rustyline 读取一行输入。
/// Ctrl+C / Ctrl+D → 退出进程。
fn read_menu_input(prompt: &str) -> anyhow::Result<String> {
    let mut editor = DefaultEditor::new()?;
    match editor.readline(prompt) {
        Ok(line) => Ok(line.trim().to_string()),
        Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
            eprintln!();
            std::process::exit(0);
        }
        Err(e) => Err(e.into()),
    }
}

// ============================================================================
// 框线渲染辅助
// ============================================================================

const BOX_WIDTH: usize = 48;

/// 渲染框顶（含居中标题）。
fn render_box_top(term: &mut Term, title: &str) -> std::io::Result<()> {
    let inner = BOX_WIDTH - 2;
    let padding = inner.saturating_sub(title.chars().count());
    let left = padding / 2;
    let right = padding - left;
    writeln!(term, "{}", style(format!("╔{}╗", "═".repeat(inner))))?;
    writeln!(
        term,
        "{}",
        style(format!(
            "║{}{}{}║",
            " ".repeat(left),
            style(title).bold(),
            " ".repeat(right)
        ))
    )?;
    Ok(())
}

/// 渲染框分隔线。
fn render_box_sep(term: &mut Term) -> std::io::Result<()> {
    let inner = BOX_WIDTH - 2;
    writeln!(term, "{}", style(format!("╠{}╣", "═".repeat(inner))))?;
    Ok(())
}

/// 渲染框中的一行（左对齐）。
fn render_box_line(term: &mut Term, line: &str) -> std::io::Result<()> {
    let inner = BOX_WIDTH - 2;
    let visible = line.chars().count();
    let padding = (inner - 1).saturating_sub(visible);
    writeln!(
        term,
        "{}",
        style(format!("║ {}{}║", line, " ".repeat(padding)))
    )?;
    Ok(())
}

/// 渲染框底。
fn render_box_bottom(term: &mut Term) -> std::io::Result<()> {
    let inner = BOX_WIDTH - 2;
    writeln!(term, "{}", style(format!("╚{}╝", "═".repeat(inner))))?;
    Ok(())
}

// ============================================================================
// Agent 选择菜单
// ============================================================================

/// 显示 Agent 列表并返回选中的 Agent 名称。
pub fn pick_agent(agents: &[AgentMeta], workspace_root: &str) -> anyhow::Result<String> {
    if agents.is_empty() {
        anyhow::bail!("没有可用的 Agent。请先用 -t 参数初始化 workspace。");
    }

    // 单个 Agent 自动选择
    if agents.len() == 1 {
        let name = agents[0].name.clone();
        eprintln!("[init] 自动选择唯一 Agent: {name}");
        return Ok(name);
    }

    let mut term = Term::stderr();
    term.write_line("")?;

    render_box_top(&mut term, &format!("WorkSpace: {workspace_root}"))?;
    render_box_line(&mut term, "")?;
    render_box_line(&mut term, "选择 Agent")?;
    render_box_sep(&mut term)?;

    for (i, meta) in agents.iter().enumerate() {
        let line = format!(
            "{}. {:<20} {}",
            i + 1,
            meta.name,
            if meta.description.is_empty() {
                ""
            } else {
                &meta.description
            }
        );
        render_box_line(&mut term, &line)?;
    }

    render_box_bottom(&mut term)?;

    // 读取输入
    loop {
        let input = read_menu_input(&format!("请选择 [1-{}]: ", agents.len()))?;
        match input.parse::<usize>() {
            Ok(n) if n >= 1 && n <= agents.len() => {
                return Ok(agents[n - 1].name.clone());
            }
            _ => {
                eprintln!("{}", style("输入无效，请重新选择").red());
            }
        }
    }
}

// ============================================================================
// Session 选择菜单
// ============================================================================

/// 显示会话列表，返回 `Some(id)` 恢复已有会话或 `None` 新建。
pub fn pick_session(agent_name: &str, sessions: &[SessionMeta]) -> anyhow::Result<Option<String>> {
    let mut term = Term::stderr();
    term.write_line("")?;

    render_box_top(&mut term, &format!("{agent_name} — 已保存的会话"))?;
    render_box_line(&mut term, "")?;

    if sessions.is_empty() {
        render_box_line(&mut term, "(无已保存的会话)")?;
    }

    // 选项 0: 新建
    writeln!(
        term,
        "{}",
        style(format!(
            "║ {}. {}║",
            style("0").cyan(),
            pad_right("[+ 创建新会话]", BOX_WIDTH - 5)
        ))
    )?;

    // 会话列表
    for (i, meta) in sessions.iter().enumerate() {
        let id_prefix: String = meta.id.chars().take(8).collect();
        let line = format!(
            "{}. {:<10}  ·  {:>3} turns  ·  {}",
            i + 1,
            id_prefix,
            meta.completed_turns,
            format_relative_time(meta.updated_at),
        );
        render_box_line(&mut term, &line)?;
    }

    render_box_bottom(&mut term)?;

    // 读取输入
    let max = sessions.len();
    loop {
        let prompt = if max == 0 {
            "按 Enter 创建新会话: ".to_string()
        } else {
            format!("请选择 [0-{max}]: ")
        };
        let input = read_menu_input(&prompt)?;

        if input.is_empty() {
            return Ok(None);
        }

        match input.parse::<usize>() {
            Ok(0) => return Ok(None),
            Ok(n) if n >= 1 && n <= max => {
                return Ok(Some(sessions[n - 1].id.clone()));
            }
            _ => {
                eprintln!("{}", style("输入无效，请重新选择").red());
            }
        }
    }
}

// ============================================================================
// 辅助
// ============================================================================

/// 将字符串右填充到指定宽度（按字符数，非字节数）。
fn pad_right(s: &str, width: usize) -> String {
    let count = s.chars().count();
    if count >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - count))
    }
}
