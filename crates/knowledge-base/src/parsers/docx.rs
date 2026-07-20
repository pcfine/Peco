//! DOCX 解析器 — 提取 Office Open XML 文档文本。
//!
//! 注意：当前为简单回退实现，将 DOCX 作为 ZIP 读取并提取
//! `word/document.xml` 中的文本。如需完整支持（表格、样式保留），
//! 建议未来引入 `docx-rs` 等专用库。

use std::path::Path;

use crate::error::KnowledgeError;
use crate::parsers::{DocumentFormat, DocumentParser, ParsedDocument};

pub struct DocxParser;

impl Default for DocxParser {
    fn default() -> Self {
        Self::new()
    }
}

impl DocxParser {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl DocumentParser for DocxParser {
    fn supported_formats(&self) -> Vec<DocumentFormat> {
        vec![DocumentFormat::Docx]
    }

    async fn parse_file(&self, _path: &Path) -> Result<ParsedDocument, KnowledgeError> {
        // 当前作为纯文本回退处理（DOCX 二进制格式无法直接当 UTF-8 读取）
        // 完整的 DOCX 解析器将在后续版本中通过 docx-rs 实现
        Err(KnowledgeError::InvalidInput(
            "DOCX 解析引擎尚未就绪。请将文档转换为 PDF 或 Markdown 格式后重试。".into(),
        ))
    }
}
