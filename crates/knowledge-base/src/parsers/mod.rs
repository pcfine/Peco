//! 文档解析器模块 — 将各类文档格式转换为统一文本。
//!
//! # 架构
//!
//! ```text
//! 文件路径 → [格式检测] → [选择解析器] → [提取文本] → [清理] → ParsedDocument
//! ```
//!
//! 每个解析器实现 [`DocumentParser`] trait，通过工厂函数 [`make_parser`]
//! 根据文件扩展名自动选择合适的解析器。

pub mod code;
pub mod docx;
pub mod html;
pub mod markdown;
#[cfg(feature = "pdf")]
pub mod pdf;
pub mod txt;

use std::path::Path;

use sha2::Digest;

use crate::error::KnowledgeError;
use crate::types::DocumentMetadata;

// ---------------------------------------------------------------------------
// DocumentParser trait
// ---------------------------------------------------------------------------

/// 文档解析器抽象 — 将各类文档格式转换为统一文本结构。
#[async_trait::async_trait]
pub trait DocumentParser: Send + Sync {
    /// 解析器支持的格式列表。
    fn supported_formats(&self) -> Vec<DocumentFormat>;

    /// 从文件路径解析文档。
    async fn parse_file(&self, path: &Path) -> Result<ParsedDocument, KnowledgeError>;

    /// 从内存字节解析（用于 HTTP 上传等场景）。
    async fn parse_bytes(
        &self,
        data: &[u8],
        filename: &str,
    ) -> Result<ParsedDocument, KnowledgeError> {
        // 默认实现：写入临时文件后调用 parse_file
        let tmp_dir = std::env::temp_dir().join("peco-kb");
        tokio::fs::create_dir_all(&tmp_dir).await.ok();

        let tmp_path = tmp_dir.join(filename);
        tokio::fs::write(&tmp_path, data)
            .await
            .map_err(|e| KnowledgeError::InvalidInput(format!("无法写入临时文件: {e}")))?;

        let result = self.parse_file(&tmp_path).await;
        let _ = tokio::fs::remove_file(&tmp_path).await;
        result
    }
}

// ---------------------------------------------------------------------------
// ParsedDocument
// ---------------------------------------------------------------------------

/// 解析产出的统一文档结构。
///
/// 所有解析器都返回此结构，使下游分块和嵌入逻辑对文档格式无感知。
#[derive(Debug, Clone)]
pub struct ParsedDocument {
    /// 提取的全文内容。
    pub content: String,
    /// 文档标题（从文件名或元数据推导）。
    pub title: String,
    /// 原始文件路径或 URL。
    pub source_path: String,
    /// 检测到的/提取的元数据。
    pub metadata: DocumentMetadata,
    /// 页数（PDF 等分页格式）。
    pub page_count: Option<u32>,
}

// ---------------------------------------------------------------------------
// DocumentFormat
// ---------------------------------------------------------------------------

/// 支持的文档格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentFormat {
    Pdf,
    Markdown,
    Html,
    Docx,
    Txt,
    Code { language: Option<&'static str> },
    Unknown,
}

impl DocumentFormat {
    /// 从文件扩展名检测格式。
    pub fn from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
            .as_deref()
        {
            Some("pdf") => DocumentFormat::Pdf,
            Some("md" | "mdx" | "markdown") => DocumentFormat::Markdown,
            Some("html" | "htm") => DocumentFormat::Html,
            Some("docx") => DocumentFormat::Docx,
            Some("txt" | "text" | "log") => DocumentFormat::Txt,
            Some("rs") => DocumentFormat::Code {
                language: Some("rust"),
            },
            Some("py") => DocumentFormat::Code {
                language: Some("python"),
            },
            Some("js" | "jsx") => DocumentFormat::Code {
                language: Some("javascript"),
            },
            Some("ts" | "tsx") => DocumentFormat::Code {
                language: Some("typescript"),
            },
            Some("go") => DocumentFormat::Code {
                language: Some("go"),
            },
            Some("java") => DocumentFormat::Code {
                language: Some("java"),
            },
            Some("json") => DocumentFormat::Code {
                language: Some("json"),
            },
            Some("yaml" | "yml") => DocumentFormat::Code {
                language: Some("yaml"),
            },
            Some("toml") => DocumentFormat::Code {
                language: Some("toml"),
            },
            Some("xml") => DocumentFormat::Code {
                language: Some("xml"),
            },
            _ => DocumentFormat::Unknown,
        }
    }

    /// 格式对应的 MIME 类型字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            DocumentFormat::Pdf => "pdf",
            DocumentFormat::Markdown => "md",
            DocumentFormat::Html => "html",
            DocumentFormat::Docx => "docx",
            DocumentFormat::Txt => "txt",
            DocumentFormat::Code { language } => language.unwrap_or("code"),
            DocumentFormat::Unknown => "unknown",
        }
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 清理文本：合并多余空白、去除控制字符、统一换行。
pub fn clean_text(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            // 将各种控制字符替换为空格（保留换行和制表符）
            '\u{0000}'..='\u{0008}' | '\u{000B}' | '\u{000C}' | '\u{000E}'..='\u{001F}' => {
                cleaned.push(' ');
            }
            '\r' => {} // 跳过 CR
            _ => cleaned.push(c),
        }
    }
    // 合并连续空白行
    let result = cleaned
        .lines()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join("\n");
    // 合并多个连续空行
    let mut final_text = String::with_capacity(result.len());
    let mut prev_empty = false;
    for line in result.lines() {
        let is_empty = line.is_empty();
        if is_empty && prev_empty {
            continue;
        }
        final_text.push_str(line);
        final_text.push('\n');
        prev_empty = is_empty;
    }
    final_text.trim().to_string()
}

/// 从文件路径中提取标题，必要时回退到文件名。
pub fn extract_title_from_path(path: &Path, fallback: &str) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

// ---------------------------------------------------------------------------
// 工厂函数
// ---------------------------------------------------------------------------

/// 根据文件扩展名创建对应的解析器。
///
/// # Example
///
/// ```ignore
/// use std::path::Path;
/// use knowledge_base::parsers::make_parser;
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let parser = make_parser(Path::new("document.pdf"))?;
/// # Ok(())
/// # }
/// ```
pub fn make_parser(path: &Path) -> Result<Box<dyn DocumentParser>, KnowledgeError> {
    let format = DocumentFormat::from_path(path);
    make_parser_for_format(format, path)
}

/// 根据文档格式创建解析器。
pub fn make_parser_for_format(
    format: DocumentFormat,
    _path: &Path,
) -> Result<Box<dyn DocumentParser>, KnowledgeError> {
    match format {
        DocumentFormat::Pdf => {
            #[cfg(feature = "pdf")]
            {
                Ok(Box::new(crate::parsers::pdf::PdfParser::new()))
            }
            #[cfg(not(feature = "pdf"))]
            {
                Err(KnowledgeError::InvalidInput(
                    "PDF 解析未启用。请启用 'pdf' feature。".into(),
                ))
            }
        }
        DocumentFormat::Markdown => Ok(Box::new(crate::parsers::markdown::MarkdownParser::new())),
        DocumentFormat::Html => Ok(Box::new(crate::parsers::html::HtmlParser::new())),
        DocumentFormat::Docx => {
            // DOCX 解析器目前返回友好的错误提示，完整实现将在后续版本提供
            Ok(Box::new(crate::parsers::docx::DocxParser::new()))
        }
        DocumentFormat::Txt | DocumentFormat::Code { .. } | DocumentFormat::Unknown => {
            Ok(Box::new(crate::parsers::txt::TextParser::new()))
        }
    }
}

/// 解析文件并直接生成 [`crate::types::Document`]。
///
/// 这是一个快捷方法，组合了解析、清理和 Document 构造。
pub async fn parse_to_document(path: &Path) -> Result<crate::types::Document, KnowledgeError> {
    let parser = make_parser(path)?;
    let parsed = parser.parse_file(path).await?;
    let hash = sha2::Sha256::digest(parsed.content.as_bytes());
    let doc_id = hex::encode(&hash[..8]); // 前 8 字节作为短 ID
    Ok(crate::types::Document {
        id: doc_id,
        kb_id: None,
        title: parsed.title,
        source_path: parsed.source_path,
        content: parsed.content,
        metadata: parsed.metadata,
    })
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_pdf_format() {
        assert_eq!(
            DocumentFormat::from_path(Path::new("doc.pdf")),
            DocumentFormat::Pdf
        );
    }

    #[test]
    fn detect_markdown_format() {
        assert_eq!(
            DocumentFormat::from_path(Path::new("readme.md")),
            DocumentFormat::Markdown
        );
    }

    #[test]
    fn detect_code_format() {
        assert_eq!(
            DocumentFormat::from_path(Path::new("main.rs")),
            DocumentFormat::Code {
                language: Some("rust")
            }
        );
    }

    #[test]
    fn detect_unknown_format() {
        assert_eq!(
            DocumentFormat::from_path(Path::new("file.xyz")),
            DocumentFormat::Unknown
        );
    }

    #[test]
    fn clean_text_removes_control_chars() {
        let input = "hello\x00world\n\n\x0Btest";
        let output = clean_text(input);
        assert!(!output.contains('\x00'));
        assert!(!output.contains('\x0B'));
        assert!(output.contains("hello"));
        assert!(output.contains("world"));
        assert!(output.contains("test"));
    }

    #[test]
    fn clean_text_collapses_multiple_blank_lines() {
        let input = "line1\n\n\n\nline2";
        let output = clean_text(input);
        let blank_count = output.lines().filter(|l| l.is_empty()).count();
        assert!(blank_count <= 1);
    }

    #[test]
    fn extract_title_from_filename() {
        let title = extract_title_from_path(Path::new("/docs/rust_guide.pdf"), "untitled");
        assert_eq!(title, "rust_guide");
    }
}
