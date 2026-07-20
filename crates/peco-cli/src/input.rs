// ============================================================================
// input — 用户输入处理
// ============================================================================
//
// 封装 rustyline 提供行编辑、历史记录、Tab 补全和语法高亮。

use std::path::PathBuf;

use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

// ============================================================================
// InputReader
// ============================================================================

/// 用户输入读取器，封装 rustyline。
pub struct InputReader {
    editor: DefaultEditor,
}

impl InputReader {
    /// 创建新的输入读取器。
    pub fn new() -> anyhow::Result<Self> {
        let mut editor = DefaultEditor::new()?;

        // 加载历史文件
        let history_path = history_file_path();
        if let Some(parent) = history_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = editor.load_history(&history_path);

        Ok(Self { editor })
    }

    /// 读取一行用户输入。
    ///
    /// 返回 `Ok(Some(line))` 有输入、`Ok(Some(""))` 空行、
    /// `Ok(None)` EOF (Ctrl+D)。
    pub fn read_line(&mut self, prompt: &str) -> anyhow::Result<Option<String>> {
        match self.editor.readline(prompt) {
            Ok(line) => {
                let trimmed = line.trim().to_string();
                if !trimmed.is_empty() {
                    let _ = self.editor.add_history_entry(line);
                }
                Ok(Some(trimmed))
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl+C — 取消当前行，返回空行让 REPL 继续
                Ok(Some(String::new()))
            }
            Err(ReadlineError::Eof) => {
                // Ctrl+D — 退出
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// 保存历史到文件。
    pub fn save_history(&mut self) -> anyhow::Result<()> {
        let path = history_file_path();
        self.editor.save_history(&path)?;
        Ok(())
    }
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 历史文件路径。
fn history_file_path() -> PathBuf {
    let base = std::env::var("PC_AGENT_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".peco")
        });
    base.join("cli-history.txt")
}
