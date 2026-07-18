// ============================================================================
// display — 终端渲染抽象
// ============================================================================
//
// Renderer trait 提供统一输出接口，ConsoleRenderer 基于 `console` crate 实现
// 跨平台彩色终端输出。支持 mock 用于单元测试。

use std::io::{self, Write};

use console::{style, Term};
use peco_core::agent::{LooperEvent, TurnOutcome};

use crate::config::CliConfig;

// ============================================================================
// Renderer trait
// ============================================================================

/// 终端渲染器抽象。
///
/// 将 LooperEvent 转换为终端输出。不同实现可对应不同输出目标
/// （终端、日志文件、测试 buffer）。
pub trait Renderer {
    /// 打印启动问候信息。
    fn render_greeting(&mut self, session_id: &str) -> io::Result<()>;

    /// 处理并渲染单个 looper 事件。
    fn render_event(&mut self, event: &LooperEvent) -> io::Result<()>;

    /// 渲染错误消息。
    fn render_error(&mut self, error: &str) -> io::Result<()>;
}

// ============================================================================
// ConsoleRenderer
// ============================================================================

/// 基于 `console` crate 的终端渲染器。
pub struct ConsoleRenderer {
    term: Term,
    color: bool,
    show_reasoning: bool,
    show_tools: bool,
    show_tool_results: bool,
    /// 当前行是否正在流式输出（用于判断是否需要前缀换行）
    in_stream: bool,
}

impl ConsoleRenderer {
    /// 从 CLI 配置创建渲染器。
    pub fn new(config: &CliConfig) -> Self {
        Self {
            term: Term::stdout(),
            color: !config.no_color,
            show_reasoning: config.show_reasoning,
            show_tools: config.show_tools,
            show_tool_results: config.show_tools,
            in_stream: false,
        }
    }

    /// 重置所有选项为默认可见。
    #[cfg(test)]
    pub fn new_for_testing(color: bool) -> Self {
        Self {
            term: Term::stdout(),
            color,
            show_reasoning: true,
            show_tools: true,
            show_tool_results: true,
            in_stream: false,
        }
    }

    /// 如果正在流式输出中，先换行再重置状态。
    fn break_stream(&mut self) -> io::Result<()> {
        if self.in_stream {
            writeln!(self.term)?;
            self.in_stream = false;
        }
        Ok(())
    }

    /// 受限长度的文本预览。
    fn truncate(s: &str, max_lines: usize, max_chars: usize) -> String {
        let lines: Vec<&str> = s.lines().collect();
        let mut result = String::new();

        for (i, line) in lines.iter().enumerate() {
            if i >= max_lines {
                result.push_str("\n  ...");
                break;
            }
            if i > 0 {
                result.push('\n');
            }
            if line.len() > max_chars {
                result.push_str(&line[..max_chars]);
                result.push_str("...");
            } else {
                result.push_str(line);
            }
        }
        result
    }
}

impl Renderer for ConsoleRenderer {
    fn render_greeting(&mut self, session_id: &str) -> io::Result<()> {
        if self.color {
            writeln!(
                self.term,
                "{}",
                style("╔══════════════════════════════════════════════╗").dim()
            )?;
            writeln!(
                self.term,
                "{} {}",
                style("║  peco CLI 聊天助手").bold(),
                style(format!("会话: {session_id}")).dim()
            )?;
            writeln!(
                self.term,
                "{}",
                style("╚══════════════════════════════════════════════╝").dim()
            )?;
        } else {
            writeln!(self.term, "peco CLI 聊天助手 — 会话: {session_id}")?;
        }
        writeln!(self.term, "输入 /help 查看命令列表，Ctrl+D 退出\n")?;
        Ok(())
    }

    fn render_event(&mut self, event: &LooperEvent) -> io::Result<()> {
        match event {
            // ── 流式文本增量 ──
            LooperEvent::TextDelta { delta } => {
                self.in_stream = true;
                write!(self.term, "{delta}")?;
                self.term.flush()?;
            }

            // ── 推理增量 ──
            LooperEvent::ReasoningDelta { delta } => {
                if self.show_reasoning {
                    self.in_stream = true;
                    if self.color {
                        write!(self.term, "{}", style(delta).dim())?;
                    } else {
                        write!(self.term, "[思考] {delta}")?;
                    }
                    self.term.flush()?;
                }
            }

            // ── Tool 调用开始 ──
            LooperEvent::ToolCallStart {
                name, arguments, ..
            } => {
                if self.show_tools {
                    self.break_stream()?;
                    if self.color {
                        writeln!(
                            self.term,
                            "\n{} {}",
                            style("⚙").cyan(),
                            style(format!("[{name}]")).cyan().bold()
                        )?;
                    } else {
                        writeln!(self.term, "\n[调用工具: {name}]")?;
                    }
                    if !arguments.is_empty() && arguments != "{}" {
                        let args_preview = Self::truncate(arguments, 5, 200);
                        if self.color {
                            writeln!(self.term, "  {}", style(args_preview).dim())?;
                        } else {
                            writeln!(self.term, "  参数: {args_preview}")?;
                        }
                    }
                }
            }

            // ── Tool 结果 ──
            LooperEvent::ToolResult {
                name, result, ..
            } => {
                if self.show_tool_results {
                    let preview = Self::truncate(result, 10, 500);
                    if self.color {
                        writeln!(self.term, "  {}", style(format!("[{name}] → {preview}")).dim())?;
                    } else {
                        writeln!(self.term, "  [结果 {name}]: {preview}")?;
                    }
                }
            }

            // ── 本轮完成 ──
            LooperEvent::TurnComplete { outcome, .. } => {
                self.break_stream()?;
                match outcome {
                    TurnOutcome::Success { text: _ } => {
                        // TextDelta 已实时输出最终文本，此处只处理换行
                    }
                    TurnOutcome::Failed {
                        reason,
                        partial_text,
                    } => {
                        if !partial_text.is_empty() {
                            writeln!(self.term, "\n{partial_text}")?;
                        }
                        if self.color {
                            writeln!(
                                self.term,
                                "\n{}",
                                style(format!("✗ 本轮失败: {reason:?}")).red()
                            )?;
                        } else {
                            writeln!(self.term, "\n✗ 本轮失败: {reason:?}")?;
                        }
                    }
                }
                writeln!(self.term)?;
            }

            // ── Shutdown ──
            LooperEvent::Shutdown {
                reason,
                total_turns,
                total_usage: _,
            } => {
                self.break_stream()?;
                let msg = if reason == "done" {
                    format!("[会话结束，共 {total_turns} 轮对话]")
                } else {
                    format!("[会话结束: {reason}，共 {total_turns} 轮]")
                };
                if self.color {
                    writeln!(self.term, "\n{}", style(msg).yellow())?;
                } else {
                    writeln!(self.term, "\n{msg}")?;
                }
            }

            // ── ToolCallDelta / 状态变更 / 其他 — 默认忽略 ──
            _ => {}
        }
        Ok(())
    }

    fn render_error(&mut self, error: &str) -> io::Result<()> {
        self.break_stream()?;
        if self.color {
            writeln!(self.term, "{}", style(format!("✗ {error}")).red())
        } else {
            writeln!(self.term, "✗ {error}")
        }
    }
}
