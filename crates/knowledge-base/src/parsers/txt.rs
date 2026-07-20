//! 纯文本解析器 — 直接读取 UTF-8 文本文件。
//!
//! 用于 TXT 文件、代码文件，以及作为任何未知格式的回退解析器。

use std::path::Path;

use crate::error::KnowledgeError;
use crate::parsers::{DocumentFormat, DocumentParser, ParsedDocument};
use crate::types::DocumentMetadata;

pub struct TextParser;

impl Default for TextParser {
    fn default() -> Self {
        Self::new()
    }
}

impl TextParser {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl DocumentParser for TextParser {
    fn supported_formats(&self) -> Vec<DocumentFormat> {
        vec![
            DocumentFormat::Txt,
            DocumentFormat::Code { language: None },
            DocumentFormat::Unknown,
        ]
    }

    async fn parse_file(&self, path: &Path) -> Result<ParsedDocument, KnowledgeError> {
        let path_str = path.to_string_lossy().to_string();
        let title = super::extract_title_from_path(path, "untitled");
        let format = DocumentFormat::from_path(path);
        let file_type = format.as_str().to_string();

        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| KnowledgeError::InvalidInput(format!("无法读取文件: {e}")))?;

        if content.trim().is_empty() {
            return Err(KnowledgeError::InvalidInput(format!(
                "文件内容为空: {path_str}"
            )));
        }

        let cleaned = super::clean_text(&content);

        tracing::info!(
            path = %path_str,
            chars = cleaned.chars().count(),
            format = %file_type,
            "文本解析成功"
        );

        Ok(ParsedDocument {
            content: cleaned,
            title,
            source_path: path_str,
            metadata: DocumentMetadata {
                file_type: Some(file_type),
                ..Default::default()
            },
            page_count: None,
        })
    }
}
