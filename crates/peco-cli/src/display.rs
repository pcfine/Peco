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
// StreamMode — 流式输出上下文状态机
// ============================================================================

/// 当前流式输出的模式，追踪「正在输出什么类型的内容」。
///
/// 用于在内容类型切换时自动插入适当的换行/分隔符，替代 ad-hoc bool 的
/// 手工管理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamMode {
    /// 空闲，不在流式输出中。
    Idle,
    /// 正在输出推理/思考增量。
    InReasoning,
    /// 正在输出正文文本增量。
    InText,
}

// ============================================================================
// Segment IR — 语义中间表示
// ============================================================================

/// 语义角色 — 描述「这是什么内容」，而非「长什么样」。
///
/// 样式（颜色、前缀、图标）由 `ConsoleRenderer` 集中映射，不再分散在
/// 每个事件处理器中。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentRole {
    /// 推理/思考过程
    Thinking,
    /// 正文回答
    Answer,
    /// 工具名称
    ToolName,
    /// 工具参数（截断后的预览）
    ToolArgs,
    /// 工具执行结果
    ToolResult,
    /// 错误信息
    Error,
    /// 一般信息（如 shutdown 消息）
    Info,
}

/// 一个待渲染的文本片段，携带语义角色。
#[derive(Debug, Clone)]
struct Segment {
    role: SegmentRole,
    text: String,
}

impl Segment {
    fn new(role: SegmentRole, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
        }
    }
}

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
    /// 轮是否已通过 TextDelta 输出过文本（区分 stream/batch）
    had_text_delta: bool,
    /// 当前流式输出的模式（Idle / InReasoning / InText）
    mode: StreamMode,
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
            had_text_delta: false,
            mode: StreamMode::Idle,
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
            had_text_delta: false,
            mode: StreamMode::Idle,
        }
    }

    /// 切换到新的流式输出模式，在模式切换时自动插入换行/分隔符。
    ///
    /// 规则：
    /// - `InReasoning → InText`：插入空行分隔思考与正文
    /// - `InReasoning/InText → Idle`：结束当前行（换行）
    /// - 同模式 / `Idle → *`：无额外输出
    fn transition_to(&mut self, new_mode: StreamMode) -> io::Result<()> {
        if new_mode == self.mode {
            return Ok(());
        }
        match (self.mode, new_mode) {
            (StreamMode::InReasoning, StreamMode::InText) => {
                // 推理结束 → 正文开始：空行分隔
                writeln!(self.term)?;
                writeln!(self.term)?;
            }
            (StreamMode::InReasoning | StreamMode::InText, StreamMode::Idle) => {
                // 流式内容结束 → 换行收尾
                writeln!(self.term)?;
            }
            _ => {}
        }
        self.mode = new_mode;
        Ok(())
    }

    // ── Segment IR 样式映射 ──────────────────────────────────────────────

    /// 返回角色对应的图标（color 和 no-color 通用）。
    fn icon_for(role: SegmentRole) -> &'static str {
        match role {
            SegmentRole::ToolName => "⚙ ",
            SegmentRole::Error => "✗ ",
            _ => "",
        }
    }

    /// 返回 no-color 模式下的文字前缀（color 模式通过颜色区分，也添加前缀以保证可访问性）。
    fn prefix_for(role: SegmentRole) -> &'static str {
        match role {
            SegmentRole::Thinking => "[思考] ",
            SegmentRole::ToolName => "[调用工具] ",
            SegmentRole::ToolArgs => "  参数: ",
            SegmentRole::ToolResult => "  [结果] ",
            _ => "",
        }
    }

    /// 渲染单个 segment。
    ///
    /// 这是唯一包含 `if self.color` 的方法 — 所有 color/no-color 分支逻辑
    /// 集中于此。
    fn write_segment(&mut self, seg: &Segment) -> io::Result<()> {
        let icon = Self::icon_for(seg.role);
        if self.color {
            match seg.role {
                SegmentRole::Thinking => {
                    write!(self.term, "{icon}{}", style(&seg.text).dim())?;
                }
                SegmentRole::Answer => {
                    write!(self.term, "{icon}{}", seg.text)?;
                }
                SegmentRole::ToolName => {
                    write!(self.term, "{icon}{}", style(&seg.text).cyan().bold())?;
                }
                SegmentRole::ToolArgs | SegmentRole::ToolResult => {
                    write!(self.term, "{icon}{}", style(&seg.text).dim())?;
                }
                SegmentRole::Error => {
                    write!(self.term, "{icon}{}", style(&seg.text).red())?;
                }
                SegmentRole::Info => {
                    write!(self.term, "{icon}{}", style(&seg.text).yellow())?;
                }
            }
        } else {
            let prefix = Self::prefix_for(seg.role);
            write!(self.term, "{icon}{prefix}{}", seg.text)?;
        }
        Ok(())
    }

    // ── 文本工具 ──────────────────────────────────────────────────────────

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
                let boundary = line.floor_char_boundary(max_chars);
                result.push_str(&line[..boundary]);
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
            writeln!(self.term, "{}", style("╔══════════════════════════════════════════════╗").dim())?;
            writeln!(
                self.term,
                "{}  {}",
                style("║  peco CLI 聊天助手").bold(),
                style(format!("会话: {session_id}")).dim()
            )?;
            writeln!(self.term, "{}", style("╚══════════════════════════════════════════════╝").dim())?;
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
                self.had_text_delta = true;
                self.transition_to(StreamMode::InText)?;
                write!(self.term, "{delta}")?;
                self.term.flush()?;
            }

            // ── 推理增量 ──
            LooperEvent::ReasoningDelta { delta } => {
                if self.show_reasoning {
                    self.transition_to(StreamMode::InReasoning)?;
                    self.write_segment(&Segment::new(SegmentRole::Thinking, delta))?;
                    self.term.flush()?;
                }
            }

            // ── Tool 调用开始 ──
            LooperEvent::ToolCallStart {
                name, arguments, ..
            } => {
                if self.show_tools {
                    self.transition_to(StreamMode::Idle)?;
                    writeln!(self.term)?;
                    self.write_segment(&Segment::new(SegmentRole::ToolName, format!("[{name}]")))?;
                    writeln!(self.term)?;
                    if !arguments.is_empty() && arguments != "{}" {
                        let args_preview = Self::truncate(arguments, 5, 200);
                        self.write_segment(&Segment::new(SegmentRole::ToolArgs, args_preview))?;
                        writeln!(self.term)?;
                    }
                }
            }

            // ── Tool 结果 ──
            LooperEvent::ToolResult {
                name, result, ..
            } => {
                if self.show_tool_results {
                    let preview = Self::truncate(result, 10, 500);
                    self.write_segment(&Segment::new(
                        SegmentRole::ToolResult,
                        format!("[{name}] → {preview}"),
                    ))?;
                    writeln!(self.term)?;
                }
            }

            // ── 本轮完成 ──
            LooperEvent::TurnComplete { outcome, .. } => {
                self.transition_to(StreamMode::Idle)?;
                match outcome {
                    TurnOutcome::Success { text } => {
                        // 流式模式：TextDelta 已输出文本；batch 模式：在此输出
                        if !self.had_text_delta && !text.is_empty() {
                            self.write_segment(&Segment::new(SegmentRole::Answer, text))?;
                            writeln!(self.term)?;
                        }
                    }
                    TurnOutcome::Failed {
                        reason,
                        partial_text,
                    } => {
                        if !partial_text.is_empty() {
                            writeln!(self.term, "\n{partial_text}")?;
                        }
                        writeln!(self.term)?;
                        self.write_segment(&Segment::new(
                            SegmentRole::Error,
                            format!("本轮失败: {reason:?}"),
                        ))?;
                        writeln!(self.term)?;
                    }
                }
                self.had_text_delta = false;
                writeln!(self.term)?;
            }

            // ── Shutdown ──
            LooperEvent::Shutdown {
                reason,
                total_turns,
                total_usage: _,
            } => {
                self.transition_to(StreamMode::Idle)?;
                let msg = if reason == "done" {
                    format!("[会话结束，共 {total_turns} 轮对话]")
                } else {
                    format!("[会话结束: {reason}，共 {total_turns} 轮]")
                };
                writeln!(self.term)?;
                self.write_segment(&Segment::new(SegmentRole::Info, msg))?;
                writeln!(self.term)?;
            }

            // ── ToolCallDelta / 状态变更 / 其他 — 默认忽略 ──
            LooperEvent::TurnStart { .. } => {
                self.had_text_delta = false;
                self.mode = StreamMode::Idle;
            }
            _ => {}
        }
        Ok(())
    }

    fn render_error(&mut self, error: &str) -> io::Result<()> {
        self.transition_to(StreamMode::Idle)?;
        self.write_segment(&Segment::new(SegmentRole::Error, error))?;
        writeln!(self.term)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── Segment 构造 ───────────────────────────────────────────────────────

    #[test]
    fn test_segment_new() {
        let seg = Segment::new(SegmentRole::Thinking, "hello");
        assert_eq!(seg.role, SegmentRole::Thinking);
        assert_eq!(seg.text, "hello");
    }

    // ── icon / prefix 映射 ─────────────────────────────────────────────────

    #[test]
    fn test_icon_for_tool() {
        assert_eq!(ConsoleRenderer::icon_for(SegmentRole::ToolName), "⚙ ");
    }

    #[test]
    fn test_icon_for_error() {
        assert_eq!(ConsoleRenderer::icon_for(SegmentRole::Error), "✗ ");
    }

    #[test]
    fn test_icon_for_answer_is_empty() {
        assert_eq!(ConsoleRenderer::icon_for(SegmentRole::Answer), "");
    }

    #[test]
    fn test_prefix_for_thinking() {
        assert_eq!(
            ConsoleRenderer::prefix_for(SegmentRole::Thinking),
            "[思考] "
        );
    }

    #[test]
    fn test_prefix_for_tool_name() {
        assert_eq!(
            ConsoleRenderer::prefix_for(SegmentRole::ToolName),
            "[调用工具] "
        );
    }

    #[test]
    fn test_prefix_for_answer_is_empty() {
        assert_eq!(ConsoleRenderer::prefix_for(SegmentRole::Answer), "");
    }

    // ── StreamMode ─────────────────────────────────────────────────────────

    #[test]
    fn test_stream_mode_debug() {
        assert_eq!(format!("{:?}", StreamMode::Idle), "Idle");
        assert_eq!(format!("{:?}", StreamMode::InReasoning), "InReasoning");
        assert_eq!(format!("{:?}", StreamMode::InText), "InText");
    }

    // ── ConsoleRenderer 构造 ───────────────────────────────────────────────

    #[test]
    fn test_new_renderer_starts_idle() {
        let r = ConsoleRenderer::new_for_testing(true);
        assert_eq!(r.mode, StreamMode::Idle);
        assert!(!r.had_text_delta);
        assert!(r.color);
        assert!(r.show_reasoning);
        assert!(r.show_tools);
    }

    #[test]
    fn test_new_renderer_no_color() {
        let r = ConsoleRenderer::new_for_testing(false);
        assert!(!r.color);
    }

    // ── transition_to 逻辑 ─────────────────────────────────────────────────

    #[test]
    fn test_transition_same_mode_noop() {
        let mut r = ConsoleRenderer::new_for_testing(true);
        r.mode = StreamMode::InReasoning;
        // 切换到同模式应为 no-op，不 panic
        r.transition_to(StreamMode::InReasoning).unwrap();
        assert_eq!(r.mode, StreamMode::InReasoning);
    }

    #[test]
    fn test_transition_idle_to_in_reasoning() {
        let mut r = ConsoleRenderer::new_for_testing(true);
        r.transition_to(StreamMode::InReasoning).unwrap();
        assert_eq!(r.mode, StreamMode::InReasoning);
    }

    #[test]
    fn test_transition_reasoning_to_text_changes_mode() {
        let mut r = ConsoleRenderer::new_for_testing(true);
        r.mode = StreamMode::InReasoning;
        r.transition_to(StreamMode::InText).unwrap();
        assert_eq!(r.mode, StreamMode::InText);
    }

    #[test]
    fn test_transition_text_to_idle_changes_mode() {
        let mut r = ConsoleRenderer::new_for_testing(true);
        r.mode = StreamMode::InText;
        r.transition_to(StreamMode::Idle).unwrap();
        assert_eq!(r.mode, StreamMode::Idle);
    }

    // ── truncate ───────────────────────────────────────────────────────────

    #[test]
    fn test_truncate_short_text() {
        let result = ConsoleRenderer::truncate("hello", 10, 200);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_long_line() {
        let result = ConsoleRenderer::truncate("abcdefghij", 10, 5);
        assert_eq!(result, "abcde...");
    }

    #[test]
    fn test_truncate_utf8_char_boundary() {
        // 6 字节处正好落在「好」(bytes 3-5) 中间，必须退到有效边界
        let result = ConsoleRenderer::truncate("你好世界", 10, 6);
        assert_eq!(result, "你好...");
    }

    #[test]
    fn test_truncate_many_lines() {
        let input = "line1\nline2\nline3\nline4";
        let result = ConsoleRenderer::truncate(input, 2, 200);
        assert!(result.starts_with("line1\nline2\n  ..."));
    }
}

