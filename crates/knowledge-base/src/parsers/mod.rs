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
#[cfg(feature = "docling")]
pub mod docling;
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
        tokio::fs::write(&tmp_path, data).await.map_err(|e| {
            KnowledgeError::InvalidInput(format!("Failed to write temporary file: {e}"))
        })?;

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
    Pptx,
    Xlsx,
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
            Some("pptx") => DocumentFormat::Pptx,
            Some("xlsx") => DocumentFormat::Xlsx,
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
            DocumentFormat::Pptx => "pptx",
            DocumentFormat::Xlsx => "xlsx",
            DocumentFormat::Txt => "txt",
            DocumentFormat::Code { language } => language.unwrap_or("code"),
            DocumentFormat::Unknown => "unknown",
        }
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 清理文本：去除控制字符、统一换行、合并连续空行、裁掉行尾空白。
///
/// **保留行首空白**：缩进对代码、YAML/TOML、以及 Markdown 缩进代码块等内容有语义，
/// 只有行尾空白与连续空行才是可安全合并的噪声。
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
        .map(|l| l.trim_end())
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
// Docling 格式覆盖
// ---------------------------------------------------------------------------

/// 仅 Docling 能解析的扩展名（原生解析器不支持）。
///
/// 这些格式只有在 Docling 启用时才进入「可摄入集合」，否则维持现状拒绝 ——
/// 上传被接受 ⟺ 会被摄入。
pub const DOCLING_ONLY_EXTENSIONS: &[&str] = &["docx", "pptx", "xlsx"];

/// 扩展名（不带点，大小写不敏感）是否属于 Docling 专属格式。
pub fn is_docling_only_ext(ext: &str) -> bool {
    DOCLING_ONLY_EXTENSIONS.contains(&ext.to_lowercase().as_str())
}

/// Docling 是否启用（转发 [`docling::docling_enabled`]；未编译该 feature 时恒 `false`）。
#[cfg(feature = "docling")]
pub fn docling_enabled() -> bool {
    docling::docling_enabled()
}

#[cfg(not(feature = "docling"))]
pub fn docling_enabled() -> bool {
    false
}

/// 格式对应的上传/表单 Content-Type。
#[cfg(feature = "docling")]
pub(crate) fn content_type_for(format: DocumentFormat) -> &'static str {
    match format {
        DocumentFormat::Pdf => "application/pdf",
        DocumentFormat::Markdown => "text/markdown",
        DocumentFormat::Html => "text/html",
        DocumentFormat::Txt => "text/plain",
        DocumentFormat::Docx => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        }
        DocumentFormat::Pptx => {
            "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        }
        DocumentFormat::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        DocumentFormat::Code { .. } | DocumentFormat::Unknown => "application/octet-stream",
    }
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
    make_parser_with_env(path)
}

#[cfg(feature = "docling")]
fn make_parser_with_env(path: &Path) -> Result<Box<dyn DocumentParser>, KnowledgeError> {
    make_parser_with_docling(path, docling::DoclingConfig::from_env().as_ref())
}

#[cfg(not(feature = "docling"))]
fn make_parser_with_env(path: &Path) -> Result<Box<dyn DocumentParser>, KnowledgeError> {
    make_parser_for_format(DocumentFormat::from_path(path), path)
}

/// 按格式选择解析器，可选地叠加 Docling 后端。
///
/// `cfg` 为 `Some` 且该格式被 Docling 支持时返回 [`docling::DoclingParser`]
/// （回退到同格式原生解析器）；否则走原生解析器。
#[cfg(feature = "docling")]
pub fn make_parser_with_docling(
    path: &Path,
    cfg: Option<&docling::DoclingConfig>,
) -> Result<Box<dyn DocumentParser>, KnowledgeError> {
    let format = DocumentFormat::from_path(path);
    if let Some(cfg) = cfg.filter(|_| docling::supports_format(format)) {
        match docling::build_client(cfg) {
            Ok(client) => {
                // 原生解析器仅作兜底：pptx/xlsx 无原生实现，缺失时靠 Docling 本体
                let fallback = make_parser_for_format(format, path).ok();
                return Ok(Box::new(docling::DoclingParser::new(
                    cfg.clone(),
                    client,
                    fallback,
                )));
            }
            Err(e) => tracing::warn!(error = %e, "Docling client init failed; using native parser"),
        }
    }
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
                    "PDF parsing is not enabled. Enable the 'pdf' feature.".into(),
                ))
            }
        }
        DocumentFormat::Markdown => Ok(Box::new(crate::parsers::markdown::MarkdownParser::new())),
        DocumentFormat::Html => Ok(Box::new(crate::parsers::html::HtmlParser::new())),
        DocumentFormat::Docx => {
            // DOCX 解析器目前返回友好的错误提示，完整实现将在后续版本提供
            Ok(Box::new(crate::parsers::docx::DocxParser::new()))
        }
        // 无原生解析器；按文本读会产出乱码，故显式报错（Docling 开启时才会走到这里）
        DocumentFormat::Pptx | DocumentFormat::Xlsx => Err(KnowledgeError::InvalidInput(
            "PPTX/XLSX requires the Docling backend (PECO_KB_DOCLING_URL configured and reachable)"
                .into(),
        )),
        DocumentFormat::Code { .. } => {
            // 代码/结构化文本原样保留（缩进、空行有语义，不经 clean_text 清洗）。
            Ok(Box::new(crate::parsers::code::CodeFileParser::new()))
        }
        DocumentFormat::Txt | DocumentFormat::Unknown => {
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
    fn detect_docling_only_formats() {
        assert_eq!(
            DocumentFormat::from_path(Path::new("deck.pptx")),
            DocumentFormat::Pptx
        );
        assert_eq!(
            DocumentFormat::from_path(Path::new("book.xlsx")),
            DocumentFormat::Xlsx
        );
    }

    #[test]
    fn docling_only_ext_matches_case_insensitively() {
        for ext in ["docx", "DOCX", "Pptx", "xlsx"] {
            assert!(is_docling_only_ext(ext), "{ext}");
        }
        for ext in ["pdf", "md", "txt", ""] {
            assert!(!is_docling_only_ext(ext), "{ext}");
        }
        assert_eq!(DOCLING_ONLY_EXTENSIONS, &["docx", "pptx", "xlsx"]);
    }

    /// Pptx/Xlsx 不得有原生兜底解析器 —— 二进制被当文本读会产出乱码。
    #[test]
    fn pptx_xlsx_have_no_native_parser() {
        for f in [DocumentFormat::Pptx, DocumentFormat::Xlsx] {
            assert!(
                make_parser_for_format(f, Path::new("x")).is_err(),
                "{f:?} 不应有原生解析器"
            );
        }
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

    /// 代码文件走 `CodeFileParser`，缩进原样保留（回归：曾被路由到 TextParser 压平）。
    #[tokio::test]
    async fn code_files_keep_indentation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg.yaml");
        tokio::fs::write(&path, "root:\n  child: 1\n  items:\n    - a\n")
            .await
            .unwrap();
        let parser = make_parser(&path).unwrap();
        let doc = parser.parse_file(&path).await.unwrap();
        assert!(
            doc.content.contains("  child: 1"),
            "缩进丢失：{:?}",
            doc.content
        );
        assert!(
            doc.content.contains("    - a"),
            "缩进丢失：{:?}",
            doc.content
        );
    }

    /// `clean_text` 裁行尾空白，但保留行首缩进（缩进有语义）。
    #[test]
    fn clean_text_preserves_leading_indentation() {
        let out = clean_text("a   \n    indented\n\tb\n");
        assert!(out.contains("    indented"), "{out:?}");
        assert!(out.contains("\tb"), "{out:?}");
        assert!(!out.contains("a   "), "行尾空白应裁掉：{out:?}");
    }

    #[test]
    fn extract_title_from_filename() {
        let title = extract_title_from_path(Path::new("/docs/rust_guide.pdf"), "untitled");
        assert_eq!(title, "rust_guide");
    }

    #[cfg(feature = "docling")]
    #[test]
    fn docling_routing_prefers_docling_for_supported_formats() {
        use crate::parsers::docling::DoclingConfig;

        let cfg = DoclingConfig {
            base_url: "http://localhost:1".into(),
            api_key: None,
            do_ocr: true,
            table_mode: "fast".into(),
            timeout_secs: 5,
        };

        // PDF 命中 Docling 路由（DoclingParser 覆盖多格式）
        let docling = make_parser_with_docling(Path::new("a.pdf"), Some(&cfg)).unwrap();
        assert!(docling.supported_formats().len() > 1);

        // 无配置 → 原生 PDF 解析器
        let native = make_parser_with_docling(Path::new("a.pdf"), None).unwrap();
        assert_eq!(native.supported_formats(), vec![DocumentFormat::Pdf]);

        // Markdown 不在 Docling 支持集合 → 原生
        let md = make_parser_with_docling(Path::new("a.md"), Some(&cfg)).unwrap();
        assert_eq!(md.supported_formats(), vec![DocumentFormat::Markdown]);

        // pptx/xlsx 无原生解析器：Docling 可用时仍须路由成功，不得被兜底失败短路
        let pptx = make_parser_with_docling(Path::new("a.pptx"), Some(&cfg)).unwrap();
        assert!(pptx.supported_formats().contains(&DocumentFormat::Pptx));
        let xlsx = make_parser_with_docling(Path::new("a.xlsx"), Some(&cfg)).unwrap();
        assert!(xlsx.supported_formats().contains(&DocumentFormat::Xlsx));
    }

    /// Docling 转换失败且无原生兜底时，必须报错而非 panic 或吞掉。
    #[cfg(feature = "docling")]
    #[tokio::test]
    async fn docling_failure_without_fallback_is_an_error() {
        use crate::parsers::docling::{DoclingConfig, DoclingParser, build_client};

        let cfg = DoclingConfig {
            base_url: "http://127.0.0.1:1".into(),
            api_key: None,
            do_ocr: false,
            table_mode: "fast".into(),
            timeout_secs: 2,
        };
        let client = build_client(&cfg).unwrap();
        let parser = DoclingParser::new(cfg, client, None);
        match parser.parse_bytes(b"PK\x03\x04", "a.pptx").await {
            Ok(_) => panic!("无兜底且 Docling 不可达时必须报错"),
            Err(e) => assert!(
                matches!(e, KnowledgeError::InvalidInput(_)),
                "错误类型应为 InvalidInput：{e:?}"
            ),
        }
    }
}
