//! Docling 外挂解析后端 — 通过 HTTP 调用 Docling 服务转换文档。
//!
//! Docling 是独立进程：仅在设置了 `PECO_KB_DOCLING_URL` 时启用，转换失败时
//! 回退到原生解析器（该格式无原生解析器则报错），绝不成为硬依赖。

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tracing::warn;

use crate::error::KnowledgeError;
use crate::parsers::{DocumentFormat, DocumentParser, ParsedDocument};
use crate::types::DocumentMetadata;

const ENV_URL: &str = "PECO_KB_DOCLING_URL";
const ENV_API_KEY: &str = "PECO_KB_DOCLING_API_KEY";
const ENV_OCR: &str = "PECO_KB_DOCLING_OCR";
const ENV_TABLE_MODE: &str = "PECO_KB_DOCLING_TABLE_MODE";
const ENV_TIMEOUT_SECS: &str = "PECO_KB_DOCLING_TIMEOUT_SECS";

/// OCR 默认开启：扫描件不做 OCR 会整体丢失正文，错误方向不对称。
const DEFAULT_DO_OCR: bool = true;
const DEFAULT_TABLE_MODE: &str = "accurate";
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Docling 端点与转换参数。
#[derive(Debug, Clone)]
pub struct DoclingConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub do_ocr: bool,
    pub table_mode: String,
    pub timeout_secs: u64,
}

impl DoclingConfig {
    /// 从环境变量读取配置；`PECO_KB_DOCLING_URL` 未设或为空 ⇒ `None`（Docling 关闭）。
    pub fn from_env() -> Option<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let base_url = lookup(ENV_URL)?.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return None;
        }
        Some(Self {
            base_url,
            api_key: lookup(ENV_API_KEY)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            do_ocr: lookup(ENV_OCR)
                .and_then(|v| parse_bool(&v))
                .unwrap_or(DEFAULT_DO_OCR),
            table_mode: match lookup(ENV_TABLE_MODE)
                .map(|s| s.trim().to_lowercase())
                .as_deref()
            {
                Some("fast") => "fast".to_string(),
                _ => DEFAULT_TABLE_MODE.to_string(),
            },
            timeout_secs: lookup(ENV_TIMEOUT_SECS)
                .and_then(|v| v.trim().parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(DEFAULT_TIMEOUT_SECS),
        })
    }
}

/// Docling 是否启用（`PECO_KB_DOCLING_URL` 已配置）。
pub fn docling_enabled() -> bool {
    DoclingConfig::from_env().is_some()
}

/// Docling 会优于原生文本抽取的格式：Docling 专属的二进制 Office 格式，
/// 加上 PDF（其版面/表格还原强于纯文本抽取器）。
pub fn supports_format(format: DocumentFormat) -> bool {
    matches!(
        format,
        DocumentFormat::Pdf | DocumentFormat::Docx | DocumentFormat::Pptx | DocumentFormat::Xlsx
    )
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

pub fn build_client(cfg: &DoclingConfig) -> Result<reqwest::Client, KnowledgeError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(cfg.timeout_secs))
        .build()
        .map_err(|e| KnowledgeError::Internal(format!("Failed to build Docling HTTP client: {e}")))
}

/// 将 Docling 响应映射为 [`ParsedDocument`] — 纯函数，便于单测。
///
/// `filename` 同时用作 `source_path` 与标题回退。
fn map_response(
    v: &Value,
    filename: &str,
    format: DocumentFormat,
) -> Result<ParsedDocument, String> {
    let document = v
        .get("document")
        .ok_or_else(|| "response missing 'document'".to_string())?;

    let md = document
        .get("md_content")
        .and_then(Value::as_str)
        .unwrap_or("");
    let text = document
        .get("text_content")
        .and_then(Value::as_str)
        .unwrap_or("");
    let chosen = if !md.trim().is_empty() { md } else { text };
    if chosen.trim().is_empty() {
        return Err("response carries no text content".to_string());
    }
    let content = super::clean_text(chosen);
    if content.is_empty() {
        return Err("content empty after cleaning".to_string());
    }

    let json = document.get("json_content").filter(|jc| !jc.is_null());
    let page_count = json
        .and_then(|jc| jc.get("pages"))
        .and_then(Value::as_object)
        .map(|pages| pages.len() as u32);

    let title = json
        .and_then(|jc| jc.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| super::extract_title_from_path(Path::new(filename), filename));

    Ok(ParsedDocument {
        content,
        title,
        source_path: filename.to_string(),
        metadata: DocumentMetadata {
            file_type: Some(format.as_str().to_string()),
            page_count,
            ..Default::default()
        },
        page_count,
    })
}

/// 正文截断，仅用于日志（避免把整段响应写进日志）。
fn preview(body: &str) -> String {
    let trimmed = body.trim();
    let mut out: String = trimmed.chars().take(200).collect();
    if trimmed.chars().count() > 200 {
        out.push('…');
    }
    out
}

/// Docling 解析器 — 转换失败时回退到 `fallback`（该格式无原生解析器时为 `None`）。
pub struct DoclingParser {
    cfg: DoclingConfig,
    client: reqwest::Client,
    fallback: Option<Box<dyn DocumentParser>>,
}

impl DoclingParser {
    pub fn new(
        cfg: DoclingConfig,
        client: reqwest::Client,
        fallback: Option<Box<dyn DocumentParser>>,
    ) -> Self {
        Self {
            cfg,
            client,
            fallback,
        }
    }

    async fn convert(
        &self,
        data: &[u8],
        part_name: &str,
        source: &str,
        format: DocumentFormat,
    ) -> Result<ParsedDocument, KnowledgeError> {
        match self.request(data, part_name, format).await {
            Ok(value) => match map_response(&value, source, format) {
                Ok(doc) => return Ok(doc),
                Err(reason) => {
                    warn!(file = source, %reason, "Docling response unusable; falling back")
                }
            },
            Err(reason) => warn!(file = source, %reason, "Docling request failed; falling back"),
        }
        match &self.fallback {
            Some(fb) => fb.parse_bytes(data, part_name).await,
            None => Err(KnowledgeError::InvalidInput(
                "Docling conversion failed and no native fallback parser exists for this format"
                    .into(),
            )),
        }
    }

    async fn request(
        &self,
        data: &[u8],
        part_name: &str,
        format: DocumentFormat,
    ) -> Result<Value, String> {
        let url = format!("{}/v1/convert/file", self.cfg.base_url);
        let part = reqwest::multipart::Part::bytes(data.to_vec())
            .file_name(part_name.to_string())
            .mime_str(super::content_type_for(format))
            .map_err(|e| format!("bad content type: {e}"))?;
        let form = reqwest::multipart::Form::new()
            .part("files", part)
            .text("to_formats", "md")
            .text("to_formats", "json")
            .text("do_ocr", self.cfg.do_ocr.to_string())
            .text("table_mode", self.cfg.table_mode.clone());

        let mut req = self.client.post(&url).multipart(form);
        if let Some(key) = &self.cfg.api_key {
            req = req.header("X-Api-Key", key);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| format!("request error: {e}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| format!("failed to read response body: {e}"))?;
        if !status.is_success() {
            return Err(format!("HTTP {status}: {}", preview(&body)));
        }
        let value: Value = serde_json::from_str(&body).map_err(|e| format!("invalid JSON: {e}"))?;
        match value.get("status").and_then(Value::as_str) {
            Some("success" | "partial_success") => Ok(value),
            other => Err(format!(
                "status={} errors={}",
                other.unwrap_or("missing"),
                value
                    .get("errors")
                    .map(Value::to_string)
                    .unwrap_or_else(|| "[]".to_string())
            )),
        }
    }
}

#[async_trait::async_trait]
impl DocumentParser for DoclingParser {
    fn supported_formats(&self) -> Vec<DocumentFormat> {
        vec![
            DocumentFormat::Pdf,
            DocumentFormat::Docx,
            DocumentFormat::Pptx,
            DocumentFormat::Xlsx,
        ]
    }

    async fn parse_file(&self, path: &Path) -> Result<ParsedDocument, KnowledgeError> {
        let data = tokio::fs::read(path).await.map_err(|e| {
            KnowledgeError::InvalidInput(format!("Failed to read file for Docling: {e}"))
        })?;
        let format = DocumentFormat::from_path(path);
        let part_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("document");
        let source = path.to_string_lossy();
        self.convert(&data, part_name, &source, format).await
    }

    async fn parse_bytes(
        &self,
        data: &[u8],
        filename: &str,
    ) -> Result<ParsedDocument, KnowledgeError> {
        let format = DocumentFormat::from_path(Path::new(filename));
        self.convert(data, filename, filename, format).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg_lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn config_disabled_without_or_empty_url() {
        assert!(DoclingConfig::from_lookup(cfg_lookup(&[])).is_none());
        assert!(DoclingConfig::from_lookup(cfg_lookup(&[(ENV_URL, "   ")])).is_none());
    }

    #[test]
    fn config_defaults_and_overrides() {
        let cfg =
            DoclingConfig::from_lookup(cfg_lookup(&[(ENV_URL, "http://localhost:5001/")])).unwrap();
        assert_eq!(cfg.base_url, "http://localhost:5001");
        assert!(cfg.do_ocr);
        assert_eq!(cfg.table_mode, "accurate");
        assert_eq!(cfg.timeout_secs, 300);
        assert_eq!(cfg.api_key, None);

        let cfg = DoclingConfig::from_lookup(cfg_lookup(&[
            (ENV_URL, "http://d:5001"),
            (ENV_API_KEY, " secret "),
            (ENV_OCR, "false"),
            (ENV_TABLE_MODE, "FAST"),
            (ENV_TIMEOUT_SECS, "42"),
        ]))
        .unwrap();
        assert_eq!(cfg.api_key.as_deref(), Some("secret"));
        assert!(!cfg.do_ocr);
        assert_eq!(cfg.table_mode, "fast");
        assert_eq!(cfg.timeout_secs, 42);
    }

    #[test]
    fn config_ignores_malformed_values() {
        let cfg = DoclingConfig::from_lookup(cfg_lookup(&[
            (ENV_URL, "http://d:5001"),
            (ENV_OCR, "maybe"),
            (ENV_TABLE_MODE, "bogus"),
            (ENV_TIMEOUT_SECS, "0"),
        ]))
        .unwrap();
        assert!(cfg.do_ocr);
        assert_eq!(cfg.table_mode, "accurate");
        assert_eq!(cfg.timeout_secs, 300);
    }

    #[test]
    fn supports_only_binary_and_pdf() {
        assert!(supports_format(DocumentFormat::Pdf));
        assert!(supports_format(DocumentFormat::Docx));
        assert!(supports_format(DocumentFormat::Pptx));
        assert!(supports_format(DocumentFormat::Xlsx));
        assert!(!supports_format(DocumentFormat::Markdown));
        assert!(!supports_format(DocumentFormat::Txt));
    }

    #[test]
    fn map_response_prefers_markdown_and_counts_pages() {
        let v = json!({
            "document": {
                "md_content": "# 标题\n\n| a | b |\n|---|---|\n| 1 | 2 |",
                "text_content": "fallback text",
                "json_content": { "name": "报告", "pages": {"1": {}, "2": {}, "3": {}} }
            },
            "status": "success"
        });
        let doc = map_response(&v, "report.pdf", DocumentFormat::Pdf).unwrap();
        assert!(doc.content.contains("| a | b |"), "{}", doc.content);
        assert_eq!(doc.title, "报告");
        assert_eq!(doc.page_count, Some(3));
        assert_eq!(doc.metadata.page_count, Some(3));
        assert_eq!(doc.metadata.file_type.as_deref(), Some("pdf"));
        assert_eq!(doc.source_path, "report.pdf");
    }

    #[test]
    fn map_response_falls_back_to_text_and_filename_title() {
        let v = json!({
            "document": {
                "md_content": "   ",
                "text_content": "plain body",
                "json_content": null
            },
            "status": "success"
        });
        let doc = map_response(&v, "/tmp/guide.docx", DocumentFormat::Docx).unwrap();
        assert_eq!(doc.content, "plain body");
        assert_eq!(doc.title, "guide");
        assert_eq!(doc.page_count, None);
        assert_eq!(doc.metadata.file_type.as_deref(), Some("docx"));
    }

    #[test]
    fn map_response_errors_when_no_text() {
        let v = json!({
            "document": { "md_content": "", "text_content": "  ", "json_content": null },
            "status": "success"
        });
        assert!(map_response(&v, "a.pdf", DocumentFormat::Pdf).is_err());

        let missing = json!({ "status": "success" });
        assert!(map_response(&missing, "a.pdf", DocumentFormat::Pdf).is_err());
    }
}
