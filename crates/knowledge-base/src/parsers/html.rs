//! HTML 解析器 — 提取可见文本，去除标签。

use std::path::Path;

use crate::error::KnowledgeError;
use crate::parsers::{DocumentFormat, DocumentParser, ParsedDocument};
use crate::types::DocumentMetadata;

pub struct HtmlParser;

impl Default for HtmlParser {
    fn default() -> Self {
        Self::new()
    }
}

impl HtmlParser {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl DocumentParser for HtmlParser {
    fn supported_formats(&self) -> Vec<DocumentFormat> {
        vec![DocumentFormat::Html]
    }

    async fn parse_file(&self, path: &Path) -> Result<ParsedDocument, KnowledgeError> {
        let path_str = path.to_string_lossy().to_string();
        let title = super::extract_title_from_path(path, "untitled");

        let raw = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| KnowledgeError::InvalidInput(format!("无法读取 HTML 文件: {e}")))?;

        if raw.trim().is_empty() {
            return Err(KnowledgeError::InvalidInput(format!(
                "HTML 文件内容为空: {path_str}"
            )));
        }

        let text = strip_html_tags(&raw);
        let cleaned = super::clean_text(&text);

        if cleaned.trim().is_empty() {
            return Err(KnowledgeError::InvalidInput(format!(
                "HTML 文件无可提取文本: {path_str}"
            )));
        }

        tracing::info!(
            path = %path_str,
            chars = cleaned.chars().count(),
            "HTML 解析成功"
        );

        Ok(ParsedDocument {
            content: cleaned,
            title,
            source_path: path_str,
            metadata: DocumentMetadata {
                file_type: Some("html".into()),
                ..Default::default()
            },
            page_count: None,
        })
    }
}

/// 简单的 HTML 标签剥离 — 移除所有 `<...>` 标签，保留文本内容。
///
/// 这是轻量级实现。如需复杂的 HTML 解析（表格、结构化数据），
/// 建议未来引入 `html2text` 或 `scraper`。
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut tag_name = String::new();

    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                tag_name.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let tn = tag_name.trim().to_lowercase();
                // 仅闭合标签 </script> 和 </style> 结束脚本/样式模式
                if tn == "/script" {
                    in_script = false;
                } else if tn == "/style" {
                    in_style = false;
                } else if tn == "br"
                    || tn == "br/"
                    || tn == "p"
                    || tn == "/p"
                    || tn == "div"
                    || tn == "/div"
                    || tn == "li"
                    || tn == "/li"
                {
                    result.push('\n');
                }
                tag_name.clear();
            }
            _ if in_tag => {
                tag_name.push(c);
                if tag_name.eq_ignore_ascii_case("script") {
                    in_script = true;
                } else if tag_name.eq_ignore_ascii_case("style") {
                    in_style = true;
                }
            }
            _ => {
                if !in_tag && !in_script && !in_style {
                    result.push(c);
                }
            }
        }
    }

    // 将 HTML 实体替换为对应字符
    result = result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_basic_html() {
        let html = "<html><body><p>Hello world</p></body></html>";
        let text = strip_html_tags(html);
        assert!(text.contains("Hello world"));
        assert!(!text.contains("<p>"));
    }

    #[test]
    fn strip_script_tags() {
        let html = "<script>alert('xss')</script><p>safe content</p>";
        let text = strip_html_tags(html);
        assert!(text.contains("safe content"));
        assert!(!text.contains("alert"));
    }

    #[test]
    fn strip_style_tags() {
        let html = "<style>body { color: red; }</style><p>styled</p>";
        let text = strip_html_tags(html);
        assert!(text.contains("styled"));
        assert!(!text.contains("color"));
    }

    #[test]
    fn replace_html_entities() {
        let html = "<p>a &amp; b &lt; c &gt; d</p>";
        let text = strip_html_tags(html);
        assert!(text.contains("&"));
        assert!(text.contains("<"));
        assert!(text.contains(">"));
    }
}
