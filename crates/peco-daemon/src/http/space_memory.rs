//! `/api/spaces/{ws_id}/memory/documents` —— 按空间浏览团队记忆库。
//!
//! 同团队的所有空间共用一套物理 KB（`kb_scope = team_id`），归属证据只有文档
//! metadata 里的 [`WS_ID_TAG`]：列表与详情都必须自己按标签过滤，读不到标签的
//! 文档一律判为「不属于任何空间」（fail-closed）—— 记忆正文是用户对话的摘要，
//! 放行一条没有标签的文档就等于让别的空间看见它。
//!
//! 只读：删除/巩固/审计仍归团队维度的记忆管理面，这里不重复开一条写路径。

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use knowledge_base::{Document, DocumentMetadata};
use peco_core::knowledge::KnowledgeModuleError;
use serde::{Deserialize, Serialize};

use crate::error::DaemonError;
use crate::memory::WS_ID_TAG;
use crate::memory::config::MemoryConfig;
use crate::state::AppState;

/// 默认一页条数。
const DEFAULT_LIMIT: i64 = 20;
/// 单页上限：每项都带 snippet，再多就是替前端做翻页。
const MAX_LIMIT: i64 = 100;
/// snippet 的字数上限。
const SNIPPET_CHARS: usize = 200;
/// 一次扫描的文档数上限。
///
/// 记忆库按团队累积，量级是几百条；取一个远大于实际规模的具体上界，而不是把
/// `usize::MAX` 交给底层按不可能达到的规模预分配。
const SCAN_LIMIT: usize = 10_000;
/// 记忆写入路径给文档打的 `source` 前缀（`ppa_{category}`）。
const SOURCE_PREFIX: &str = "ppa_";

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub query: Option<String>,
    pub category: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// 记忆类别 —— 与写路径的 `source` 一一对应。
///
/// 非 `ppa_*` 的文档共存在同一个 KB 里（例如用户上传的资料），它们归不进三类，
/// 统一落在 `other`：过滤参数因此是全覆盖的，前端不必为「第四种取值」再写一次
/// 特判。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryCategory {
    Profile,
    Semantic,
    Episodic,
    Other,
}

impl MemoryCategory {
    fn of(source: &str) -> Self {
        match source.strip_prefix(SOURCE_PREFIX) {
            Some("profile") => Self::Profile,
            Some("semantic") => Self::Semantic,
            Some("episodic") => Self::Episodic,
            _ => Self::Other,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Profile => "profile",
            Self::Semantic => "semantic",
            Self::Episodic => "episodic",
            Self::Other => "other",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        [Self::Profile, Self::Semantic, Self::Episodic, Self::Other]
            .into_iter()
            .find(|category| category.as_str() == raw)
    }
}

#[derive(Debug, Serialize)]
pub struct DocumentListView {
    /// 过滤后的总条数（分页之前）：前端据此决定「还有没有下一页」。
    pub total: usize,
    pub items: Vec<DocumentItemView>,
}

#[derive(Debug, Serialize)]
pub struct DocumentItemView {
    pub id: String,
    pub title: String,
    pub source: String,
    pub category: &'static str,
    pub created_at: Option<String>,
    pub snippet: String,
}

#[derive(Debug, Serialize)]
pub struct DocumentDetailView {
    pub id: String,
    pub title: String,
    pub source: String,
    pub category: &'static str,
    pub created_at: Option<String>,
    pub content: String,
    pub metadata: DocumentMetadata,
}

pub async fn list(
    State(state): State<AppState>,
    UrlPath(ws_id): UrlPath<String>,
    Query(params): Query<ListQuery>,
) -> Result<Json<DocumentListView>, DaemonError> {
    let category = match params.category.as_deref() {
        None => None,
        Some(raw) => Some(MemoryCategory::parse(raw).ok_or_else(|| {
            DaemonError::InvalidArgument {
                message: format!(
                    "unknown category '{raw}'; expected one of profile, semantic, episodic, other"
                ),
            }
        })?),
    };
    let (limit, offset) = page_params(params.limit, params.offset)?;
    let needle = params
        .query
        .as_deref()
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(str::to_lowercase);

    let km = state
        .spaces
        .workspace(&ws_id)
        .await?
        .knowledge_manager()
        .clone();
    let kb_name = MemoryConfig::from_env().kb_name;

    let documents = match km
        .list_memory_documents_with_content(&kb_name, SCAN_LIMIT)
        .await
    {
        Ok(documents) => documents,
        // 新建的空间还没有记忆库：空列表才是「这个空间没记过事」的答案，
        // 500 会让面板把正常的新空间显示成故障。
        Err(KnowledgeModuleError::NotFound(_)) => Vec::new(),
        Err(e) => {
            return Err(DaemonError::Internal {
                message: format!("cannot list memories from '{kb_name}': {e}"),
            });
        }
    };

    let mut items: Vec<DocumentItemView> = documents
        .iter()
        .filter(|document| belongs_to(document, &ws_id))
        .filter(|document| match category {
            Some(category) => MemoryCategory::of(&document.source_path) == category,
            None => true,
        })
        .filter(|document| match needle.as_deref() {
            Some(needle) => matches_query(document, needle),
            None => true,
        })
        .map(item_view)
        .collect();
    // 倒序 + id 兜底：分页落在无序列表上会漏条或重条，而写入时刻相同的两条
    // 记忆只有 id 能定序。
    items.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    let total = items.len();
    // `page_params` 已保证两个值非负，直接当长度用即可。
    let items = items
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    Ok(Json(DocumentListView { total, items }))
}

pub async fn detail(
    State(state): State<AppState>,
    UrlPath((ws_id, doc_id)): UrlPath<(String, String)>,
) -> Result<Json<DocumentDetailView>, DaemonError> {
    let km = state
        .spaces
        .workspace(&ws_id)
        .await?
        .knowledge_manager()
        .clone();
    let kb_name = MemoryConfig::from_env().kb_name;

    let document = match km.get_document(&kb_name, &doc_id).await {
        Ok(document) => document,
        Err(KnowledgeModuleError::NotFound(_)) => None,
        Err(e) => {
            return Err(DaemonError::Internal {
                message: format!("cannot read document '{doc_id}' from '{kb_name}': {e}"),
            });
        }
    };

    // 「不存在」与「属于别的空间」回同一个错误：区分开就等于告诉调用方某个 id
    // 真实存在，跨空间探测因此能一条条试出别人的文档 id。
    let document = document
        .filter(|document| belongs_to(document, &ws_id))
        .ok_or_else(|| DaemonError::MemoryDocumentNotFound {
            ws_id: ws_id.clone(),
            doc_id: doc_id.clone(),
        })?;

    let category = MemoryCategory::of(&document.source_path).as_str();
    let Document {
        id,
        title,
        source_path,
        content,
        metadata,
        ..
    } = document;
    Ok(Json(DocumentDetailView {
        id,
        title,
        source: source_path,
        category,
        created_at: metadata.created_at.clone(),
        content,
        metadata,
    }))
}

/// 文档是否属于该空间。
///
/// 标签缺失或值与路径里的 `ws_id` 不同，都判为不属于：无法证明归属的文档不该
/// 出现在任何一个空间的列表里 —— 宁可少给一条，也不跨空间串记忆。
fn belongs_to(document: &Document, ws_id: &str) -> bool {
    document.metadata.extra.get(WS_ID_TAG).map(String::as_str) == Some(ws_id)
}

/// 标题或正文命中子串（大小写不敏感）。
fn matches_query(document: &Document, needle: &str) -> bool {
    document.title.to_lowercase().contains(needle)
        || document.content.to_lowercase().contains(needle)
}

/// 分页参数：`limit` 夹到上限，负数明确拒绝而不是悄悄按 0 处理。
fn page_params(limit: Option<i64>, offset: Option<i64>) -> Result<(i64, i64), DaemonError> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = offset.unwrap_or(0);
    if limit < 0 || offset < 0 {
        return Err(DaemonError::InvalidArgument {
            message: format!(
                "limit and offset must not be negative (limit={limit}, offset={offset})"
            ),
        });
    }
    Ok((limit, offset))
}

fn item_view(document: &Document) -> DocumentItemView {
    DocumentItemView {
        id: document.id.clone(),
        title: document.title.clone(),
        source: document.source_path.clone(),
        category: MemoryCategory::of(&document.source_path).as_str(),
        created_at: document.metadata.created_at.clone(),
        snippet: snippet(&document.content),
    }
}

/// 正文前 [`SNIPPET_CHARS`] 个字。
///
/// 按字符而不是字节截断：中文正文按字节切会把一个字劈成两半，前端拿到的就是
/// 一个非法 UTF-8 片段。
fn snippet(content: &str) -> String {
    content.chars().take(SNIPPET_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(ws_id: Option<&str>, source: &str, title: &str, content: &str) -> Document {
        let mut metadata = DocumentMetadata::default();
        if let Some(ws_id) = ws_id {
            metadata
                .extra
                .insert(WS_ID_TAG.to_string(), ws_id.to_string());
        }
        Document {
            id: "doc".into(),
            kb_id: None,
            title: title.into(),
            source_path: source.into(),
            content: content.into(),
            metadata,
        }
    }

    #[test]
    fn category_covers_the_write_path_sources() {
        assert_eq!(MemoryCategory::of("ppa_profile"), MemoryCategory::Profile);
        assert_eq!(MemoryCategory::of("ppa_semantic"), MemoryCategory::Semantic);
        assert_eq!(MemoryCategory::of("ppa_episodic"), MemoryCategory::Episodic);
        assert_eq!(
            MemoryCategory::of("uploaded/notes.md"),
            MemoryCategory::Other
        );
        assert!(MemoryCategory::parse("profile").is_some());
        assert!(MemoryCategory::parse("other").is_some());
        assert!(MemoryCategory::parse("Profile").is_none(), "取值区分大小写");
        assert!(MemoryCategory::parse("").is_none());
    }

    /// 无标签的文档不属于任何空间 —— 列表与详情共用这一条判据。
    #[test]
    fn untagged_documents_belong_to_no_space() {
        assert!(belongs_to(
            &document(Some("ws_a"), "ppa_profile", "t", "c"),
            "ws_a"
        ));
        assert!(!belongs_to(
            &document(Some("ws_b"), "ppa_profile", "t", "c"),
            "ws_a"
        ));
        assert!(!belongs_to(
            &document(None, "ppa_profile", "t", "c"),
            "ws_a"
        ));
    }

    #[test]
    fn query_matches_title_or_content_case_insensitively() {
        let doc = document(
            Some("ws_a"),
            "ppa_semantic",
            "memory_1",
            "用户在做 Rust daemon",
        );
        assert!(matches_query(&doc, "rust"));
        assert!(matches_query(&doc, "daemon"));
        assert!(matches_query(&doc, "memory_1"));
        assert!(!matches_query(&doc, "python"));
    }

    /// 按字截断：200 个汉字不能被切出半个字，短正文原样返回。
    #[test]
    fn snippet_takes_the_leading_characters() {
        let long: String = "记忆".repeat(SNIPPET_CHARS);
        assert_eq!(snippet(&long).chars().count(), SNIPPET_CHARS);
        assert!(snippet(&long).chars().all(|c| c == '记' || c == '忆'));
        assert_eq!(snippet("短正文"), "短正文");
    }

    #[test]
    fn page_params_default_and_clamp() {
        assert_eq!(page_params(None, None).unwrap(), (DEFAULT_LIMIT, 0));
        assert_eq!(page_params(Some(1000), None).unwrap().0, MAX_LIMIT);
        assert_eq!(page_params(Some(0), Some(5)).unwrap(), (0, 5));
        assert!(page_params(Some(-1), None).is_err());
        assert!(page_params(None, Some(-1)).is_err());
    }
}
