//! DOCX 解析器 — 无实际解析能力，仅为 Docling 不可用时的回退。
//!
//! DOCX 由 Docling 后端解析（见 [`crate::parsers::docling`]）。

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
        // 无原生解析器：DOCX 是 ZIP 二进制，当文本读会产出乱码。
        // 由 Docling 后端解析（见 parsers::docling），此处仅为其不可用时的回退。
        Err(KnowledgeError::InvalidInput(
            "DOCX requires the Docling backend (PECO_KB_DOCLING_URL configured and reachable)"
                .into(),
        ))
    }
}
