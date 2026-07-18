//! 代码文件解析器 — 按编程语言分类，保留代码结构。
//!
//! 代码文件作为纯文本处理，但保留语言信息用于元数据标注。

use std::path::Path;

use crate::error::KnowledgeError;
use crate::parsers::{DocumentFormat, DocumentParser, ParsedDocument};
use crate::types::DocumentMetadata;

pub struct CodeFileParser;

impl CodeFileParser {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl DocumentParser for CodeFileParser {
    fn supported_formats(&self) -> Vec<DocumentFormat> {
        vec![DocumentFormat::Code { language: None }]
    }

    async fn parse_file(&self, path: &Path) -> Result<ParsedDocument, KnowledgeError> {
        let path_str = path.to_string_lossy().to_string();
        let title = super::extract_title_from_path(path, "untitled");
        let format = DocumentFormat::from_path(path);

        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| KnowledgeError::InvalidInput(format!("无法读取代码文件: {e}")))?;

        if content.trim().is_empty() {
            return Err(KnowledgeError::InvalidInput(format!(
                "代码文件内容为空: {path_str}"
            )));
        }

        tracing::info!(
            path = %path_str,
            chars = content.chars().count(),
            format = format.as_str(),
            "代码文件解析成功"
        );

        Ok(ParsedDocument {
            content,
            title,
            source_path: path_str,
            metadata: DocumentMetadata {
                file_type: Some(format.as_str().to_string()),
                language: match format {
                    DocumentFormat::Code {
                        language: Some(lang),
                    } => Some(lang.to_string()),
                    _ => None,
                },
                ..Default::default()
            },
            page_count: None,
        })
    }
}
