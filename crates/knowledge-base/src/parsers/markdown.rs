//! Markdown 解析器 — 提取正文，保留 frontmatter 元数据。
//!
//! 处理 YAML/TOML frontmatter（`---` 分隔），提取标题层级结构。

use std::path::Path;

use crate::error::KnowledgeError;
use crate::parsers::{DocumentFormat, DocumentParser, ParsedDocument};
use crate::types::DocumentMetadata;

pub struct MarkdownParser;

impl Default for MarkdownParser {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownParser {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl DocumentParser for MarkdownParser {
    fn supported_formats(&self) -> Vec<DocumentFormat> {
        vec![DocumentFormat::Markdown]
    }

    async fn parse_file(&self, path: &Path) -> Result<ParsedDocument, KnowledgeError> {
        let path_str = path.to_string_lossy().to_string();
        let raw = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| KnowledgeError::InvalidInput(format!("无法读取 Markdown 文件: {e}")))?;

        if raw.trim().is_empty() {
            return Err(KnowledgeError::InvalidInput(format!(
                "Markdown 文件内容为空: {path_str}"
            )));
        }

        // 提取标题：优先使用第一个 `# 标题`，其次使用文件名
        let title = extract_md_title(&raw)
            .unwrap_or_else(|| super::extract_title_from_path(path, "untitled"));

        // 清理文本
        let cleaned = super::clean_text(&raw);

        tracing::info!(
            path = %path_str,
            chars = cleaned.chars().count(),
            title = %title,
            "Markdown 解析成功"
        );

        Ok(ParsedDocument {
            content: cleaned,
            title,
            source_path: path_str,
            metadata: DocumentMetadata {
                file_type: Some("md".into()),
                ..Default::default()
            },
            page_count: None,
        })
    }
}

/// 从 Markdown 文本中提取第一个一级标题。
fn extract_md_title(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(stripped) = trimmed.strip_prefix("# ") {
            return Some(stripped.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_title_from_h1() {
        let md = "# Rust 编程指南\n\n一些内容";
        assert_eq!(extract_md_title(md), Some("Rust 编程指南".into()));
    }

    #[test]
    fn extract_title_skips_h2() {
        let md = "## 二级标题\n\n# 一级标题\n\n内容";
        assert_eq!(extract_md_title(md), Some("一级标题".into()));
    }

    #[test]
    fn no_h1_returns_none() {
        let md = "没有标题\n\n只有内容";
        assert_eq!(extract_md_title(md), None);
    }
}
