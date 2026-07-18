//! PDF 文档解析器 — 基于 `pdf-extract`。
//!
//! 提取 PDF 文本内容，保留页码信息。

use std::path::Path;

use crate::error::KnowledgeError;
use crate::parsers::{DocumentFormat, DocumentParser, ParsedDocument};
use crate::types::DocumentMetadata;

pub struct PdfParser;

impl PdfParser {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl DocumentParser for PdfParser {
    fn supported_formats(&self) -> Vec<DocumentFormat> {
        vec![DocumentFormat::Pdf]
    }

    async fn parse_file(&self, path: &Path) -> Result<ParsedDocument, KnowledgeError> {
        let path_str = path.to_string_lossy().to_string();
        let title = super::extract_title_from_path(path, "untitled");

        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| KnowledgeError::InvalidInput(format!("无法读取 PDF 文件: {e}")))?;

        let content = tokio::task::spawn_blocking(move || {
            pdf_extract::extract_text_from_mem(&bytes).map_err(|e| format!("PDF 解析失败: {e}"))
        })
        .await
        .map_err(|e| KnowledgeError::InvalidInput(format!("spawn_blocking 失败: {e}")))?
        .map_err(|e| KnowledgeError::InvalidInput(e))?;

        if content.trim().is_empty() {
            return Err(KnowledgeError::InvalidInput(format!(
                "PDF 文件内容为空: {path_str}"
            )));
        }

        let cleaned = super::clean_text(&content);
        let page_count = estimate_page_count(&cleaned);

        tracing::info!(
            path = %path_str,
            chars = cleaned.chars().count(),
            ?page_count,
            "PDF 解析成功"
        );

        Ok(ParsedDocument {
            content: cleaned,
            title,
            source_path: path_str,
            metadata: DocumentMetadata {
                file_type: Some("pdf".into()),
                page_count,
                ..Default::default()
            },
            page_count,
        })
    }
}

/// 粗略估算 PDF 页数（~3000 字符/页）。
fn estimate_page_count(content: &str) -> Option<u32> {
    let chars_per_page = 3000.0;
    let pages = (content.chars().count() as f64 / chars_per_page).ceil() as u32;
    Some(pages.max(1))
}
