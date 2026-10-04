// ============================================================================
// Knowledge Tools — 9 个知识库工具（依赖注入版）
// ============================================================================
//
// CLI 和 Web 使用同一个工具实现。
// 差异仅在 KnowledgeAccess 的构造方式（不同用户对应不同 KnowledgeManager）。

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use super::deps::{KnowledgeAccess, MemoryAuditAccess, MemoryAuditEntry};

use super::{Content, StringError, ToolDyn, ToolError};
use crate::knowledge::hash_manifest::now_iso8601;
use tracing::{info, warn};

fn string_err(msg: impl ToString) -> ToolError {
    ToolError::ToolCallError(Box::new(StringError(msg.to_string())))
}

/// 检查指定的知识库是否在 Agent 的访问白名单中。
///
/// `allowed` 为空时，拒绝一切 KB 访问。
fn check_kb_access(allowed: &[String], kb_name: &str) -> Result<(), ToolError> {
    if !allowed.contains(&kb_name.to_string()) {
        return Err(ToolError::ToolCallError(Box::new(StringError(format!(
            "Access denied: knowledge base '{kb_name}' is not in the agent's \
             knowledge_bases list. Declare accessible knowledge bases in agent.md."
        )))));
    }
    Ok(())
}

// ============================================================================
// SearchKnowledge
// ============================================================================

pub struct SearchKnowledge {
    access: Arc<dyn KnowledgeAccess>,
    allowed_kbs: Vec<String>,
}

impl SearchKnowledge {
    pub fn new(access: Arc<dyn KnowledgeAccess>, allowed_kbs: Vec<String>) -> Self {
        Self {
            access,
            allowed_kbs,
        }
    }
}

impl ToolDyn for SearchKnowledge {
    fn name(&self) -> String {
        "search_knowledge".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "search_knowledge".to_string(),
            description: "Search knowledge bases for information. Supports hybrid retrieval \
                          across all knowledge bases or a single specified one."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query, natural language."
                    },
                    "kb_name": {
                        "type": "string",
                        "description": "Knowledge base name to search. If omitted, searches all knowledge bases."
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Number of results to return, default 5"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args {
                query: String,
                kb_name: Option<String>,
                #[serde(default = "default_top_k")]
                top_k: usize,
            }
            fn default_top_k() -> usize {
                5
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(string_err)?;

            let formatted = if let Some(name) = &parsed.kb_name {
                check_kb_access(&self.allowed_kbs, name)?;
                let results = km
                    .search_kb(name, &parsed.query, parsed.top_k)
                    .await
                    .map_err(string_err)?;
                results
                    .iter()
                    .map(|r| {
                        json!({
                            "kb": name.clone(), "title": r.title, "snippet": r.snippet,
                            "score": r.score, "source": r.source_path,
                        })
                    })
                    .collect::<Vec<_>>()
            } else {
                // search_all → 仅保留允许列表中的 KB
                let all = km
                    .search_all(&parsed.query, parsed.top_k)
                    .await
                    .map_err(string_err)?;
                all.into_iter()
                    .filter(|(kb_name, _)| self.allowed_kbs.contains(kb_name))
                    .flat_map(|(kb_name, hits)| {
                        hits.into_iter().map(move |h| {
                            json!({
                                "kb": kb_name.clone(), "title": h.title, "snippet": h.snippet,
                                "score": h.score, "source": h.source_path,
                            })
                        })
                    })
                    .collect::<Vec<_>>()
            };

            serde_json::to_string_pretty(&formatted)
                .map_err(string_err)
                .map(Content::Text)
        })
    }
}

// ============================================================================
// ListKnowledgeBases
// ============================================================================

pub struct ListKnowledgeBases {
    access: Arc<dyn KnowledgeAccess>,
    allowed_kbs: Vec<String>,
}

impl ListKnowledgeBases {
    pub fn new(access: Arc<dyn KnowledgeAccess>, allowed_kbs: Vec<String>) -> Self {
        Self {
            access,
            allowed_kbs,
        }
    }
}

impl ToolDyn for ListKnowledgeBases {
    fn name(&self) -> String {
        "list_knowledge_bases".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_knowledge_bases".to_string(),
            description: "List all available knowledge bases with name, description, document \
                          count, backend type, and other details."
                .to_string(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }

    fn call<'a>(
        &'a self,
        _args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(string_err)?;

            let infos = km.list_kbs().await.map_err(string_err)?;
            let display: Vec<_> = infos
                .into_iter()
                .filter(|i| self.allowed_kbs.contains(&i.name))
                .map(|i| {
                    json!({
                        "name": i.name, "description": i.description, "backend": i.backend,
                        "embedding_model": i.embedding_model, "document_count": i.document_count,
                        "chunk_count": i.chunk_count,
                    })
                })
                .collect();

            serde_json::to_string_pretty(&display)
                .map_err(string_err)
                .map(Content::Text)
        })
    }
}

// ============================================================================
// AddToKnowledgeBase
// ============================================================================

pub struct AddToKnowledgeBase {
    access: Arc<dyn KnowledgeAccess>,
    allowed_kbs: Vec<String>,
}

impl AddToKnowledgeBase {
    pub fn new(access: Arc<dyn KnowledgeAccess>, allowed_kbs: Vec<String>) -> Self {
        Self {
            access,
            allowed_kbs,
        }
    }
}

impl ToolDyn for AddToKnowledgeBase {
    fn name(&self) -> String {
        "add_to_knowledge_base".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "add_to_knowledge_base".to_string(),
            description: "Add text content to a knowledge base. Supports specifying a storage \
                          mode to control the ingestion path."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": { "type": "string", "description": "Name of the target knowledge base" },
                    "title": { "type": "string", "description": "Title of the content" },
                    "content": { "type": "string", "description": "Text content" },
                    "source": { "type": "string", "description": "Source identifier" },
                    "storage_mode": {
                        "type": "string",
                        "enum": ["full", "vector_only", "text_only", "graph_only", "vector_and_text", "vector_and_graph", "text_and_graph"],
                        "description": "Storage mode: full (everything), vector_only, text_only, graph_only, vector_and_text, vector_and_graph, text_and_graph. Default full"
                    }
                },
                "required": ["kb_name", "title", "content"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args {
                kb_name: String,
                title: String,
                content: String,
                #[serde(default)]
                source: Option<String>,
                #[serde(default)]
                storage_mode: Option<String>,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let source = parsed.source.unwrap_or_else(|| "manual".to_string());

            let mode = match parsed.storage_mode.as_deref() {
                Some("vector_only") => knowledge_base::StorageMode::VectorOnly,
                Some("text_only") => knowledge_base::StorageMode::TextOnly,
                Some("graph_only") => knowledge_base::StorageMode::GraphOnly,
                Some("vector_and_text") => knowledge_base::StorageMode::VectorAndText,
                Some("vector_and_graph") => knowledge_base::StorageMode::VectorAndGraph,
                Some("text_and_graph") => knowledge_base::StorageMode::TextAndGraph,
                _ => knowledge_base::StorageMode::Full,
            };

            check_kb_access(&self.allowed_kbs, &parsed.kb_name)?;

            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(string_err)?;

            let doc = km
                .add_text_to_kb_with_mode(
                    &parsed.kb_name,
                    &parsed.title,
                    &parsed.content,
                    &source,
                    mode,
                )
                .await
                .map_err(string_err)?;

            info!(kb = %parsed.kb_name, title = %parsed.title, doc_id = %doc.id, "Document added via tool");
            Ok(Content::Text(format!(
                "Document added: {} (id: {})",
                doc.title, doc.id
            )))
        })
    }
}

// ============================================================================
// SyncKnowledgeBase
// ============================================================================

pub struct SyncKnowledgeBase {
    access: Arc<dyn KnowledgeAccess>,
    allowed_kbs: Vec<String>,
}

impl SyncKnowledgeBase {
    pub fn new(access: Arc<dyn KnowledgeAccess>, allowed_kbs: Vec<String>) -> Self {
        Self {
            access,
            allowed_kbs,
        }
    }
}

impl ToolDyn for SyncKnowledgeBase {
    fn name(&self) -> String {
        "sync_knowledge_base".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "sync_knowledge_base".to_string(),
            description: "Sync a knowledge base: scan the source documents directory, detect \
                          added, changed, and deleted files, and incrementally update the \
                          vector database."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": { "type": "string", "description": "Knowledge base name. If omitted, syncs all knowledge bases." }
                },
                "required": []
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args {
                kb_name: Option<String>,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(string_err)?;

            if let Some(name) = parsed.kb_name {
                check_kb_access(&self.allowed_kbs, &name)?;
                let report = km.sync_kb(&name).await.map_err(string_err)?;
                info!(kb = %name, added = report.added, updated = report.updated, removed = report.removed, "KB synced via tool");
                Ok(Content::Text(format!(
                    "Knowledge base '{}' synced:\n- Added: {} files\n- Updated: {} files\n- Removed: {} files\n- Skipped: {} files\n- Duration: {}ms",
                    report.kb_name,
                    report.added,
                    report.updated,
                    report.removed,
                    report.skipped,
                    report.duration_ms
                )))
            } else {
                // 仅在允许列表中的 KB 上执行同步
                let mut reports = Vec::new();
                let mut errors = Vec::new();
                for kb_name in &self.allowed_kbs {
                    match km.sync_kb(kb_name).await {
                        Ok(report) => reports.push((kb_name.clone(), report)),
                        Err(e) => {
                            warn!(kb = %kb_name, error = %e, "KB sync failed");
                            errors.push((kb_name.clone(), e.to_string()));
                        }
                    }
                }
                let mut lines: Vec<String> = reports
                    .iter()
                    .map(|(name, report)| {
                        format!(
                            "  '{}': +{}/~{} skipped {}",
                            name, report.added, report.updated, report.skipped
                        )
                    })
                    .collect();
                if !errors.is_empty() {
                    lines.push(String::new());
                    lines.push("The following knowledge bases failed to sync:".to_string());
                    for (name, err) in &errors {
                        lines.push(format!("  '{}': {err}", name));
                    }
                }
                info!(
                    kb_count = self.allowed_kbs.len(),
                    synced = reports.len(),
                    failed = errors.len(),
                    "All KBs synced via tool"
                );
                Ok(Content::Text(format!(
                    "Knowledge bases synced:\n{}",
                    lines.join("\n")
                )))
            }
        })
    }
}

// ============================================================================
// GetKnowledgeBaseDocs
// ============================================================================

pub struct GetKnowledgeBaseDocs {
    access: Arc<dyn KnowledgeAccess>,
    allowed_kbs: Vec<String>,
}

impl GetKnowledgeBaseDocs {
    pub fn new(access: Arc<dyn KnowledgeAccess>, allowed_kbs: Vec<String>) -> Self {
        Self {
            access,
            allowed_kbs,
        }
    }
}

impl ToolDyn for GetKnowledgeBaseDocs {
    fn name(&self) -> String {
        "get_knowledge_base_docs".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "get_knowledge_base_docs".to_string(),
            description: "List documents in the specified knowledge base with pagination. \
Use offset/limit to page through large knowledge bases (has_more tells you to keep going), \
and source_filter to restrict to entries whose source starts with the given prefix \
(e.g. \"ppa_episodic\")."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": { "type": "string", "description": "Knowledge base name" },
                    "offset": { "type": "integer", "description": "Number of matching documents to skip (default 0)" },
                    "limit": { "type": "integer", "description": "Max documents to return (default 50, max 200)" },
                    "source_filter": { "type": "string", "description": "Only return documents whose source starts with this prefix" }
                },
                "required": ["kb_name"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args {
                kb_name: String,
                #[serde(default)]
                offset: Option<usize>,
                #[serde(default)]
                limit: Option<usize>,
                #[serde(default)]
                source_filter: Option<String>,
            }

            const DEFAULT_LIMIT: usize = 50;
            const MAX_LIMIT: usize = 200;
            const RAW_PAGE: usize = 200;

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            check_kb_access(&self.allowed_kbs, &parsed.kb_name)?;
            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(string_err)?;

            let limit = parsed.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
            let offset = parsed.offset.unwrap_or(0);

            // source 过滤在工具层实现（list_documents 本身无此参数）：
            // offset / has_more 均按「过滤后」的条目计数，内部按原始列表
            // 分页推进，直到凑满一页或扫完全库。这样调用方对过滤结果的
            // 翻页语义与无过滤时完全一致。
            let mut docs_out = Vec::new();
            let mut skipped = 0usize;
            let mut has_more = false;
            let mut raw_offset = 0usize;
            loop {
                let batch = km
                    .list_documents(&parsed.kb_name, raw_offset, RAW_PAGE)
                    .await
                    .map_err(string_err)?;
                let batch_len = batch.len();
                raw_offset += batch_len;
                for d in batch {
                    let hit = parsed
                        .source_filter
                        .as_ref()
                        .is_none_or(|f| d.source_path.starts_with(f.as_str()));
                    if !hit {
                        continue;
                    }
                    if skipped < offset {
                        skipped += 1;
                        continue;
                    }
                    if docs_out.len() < limit {
                        docs_out.push(d);
                    } else {
                        has_more = true;
                        break;
                    }
                }
                if has_more || batch_len < RAW_PAGE {
                    break;
                }
            }

            let display: Vec<_> = docs_out
                .iter()
                .map(|d| {
                    json!({
                        "id": d.id, "title": d.title, "source": d.source_path,
                    })
                })
                .collect();

            let body = json!({
                "kb_name": parsed.kb_name,
                "offset": offset,
                "limit": limit,
                "count": display.len(),
                "has_more": has_more,
                "documents": display,
            });

            serde_json::to_string_pretty(&body)
                .map_err(string_err)
                .map(Content::Text)
        })
    }
}

// ============================================================================
// AddFactsToKnowledgeBase
// ============================================================================

pub struct AddFactsToKnowledgeBase {
    access: Arc<dyn KnowledgeAccess>,
    allowed_kbs: Vec<String>,
}

impl AddFactsToKnowledgeBase {
    pub fn new(access: Arc<dyn KnowledgeAccess>, allowed_kbs: Vec<String>) -> Self {
        Self {
            access,
            allowed_kbs,
        }
    }
}

impl ToolDyn for AddFactsToKnowledgeBase {
    fn name(&self) -> String {
        "add_facts_to_knowledge_base".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "add_facts_to_knowledge_base".to_string(),
            description: "Write structured facts (triples) directly into the knowledge graph, \
                          skipping document chunking and embedding. Suited for storing \
                          discrete knowledge such as user preferences, relations, and events."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": {
                        "type": "string",
                        "description": "Name of the target knowledge base"
                    },
                    "facts": {
                        "type": "array",
                        "description": "List of facts",
                        "items": {
                            "type": "object",
                            "properties": {
                                "subject": {
                                    "type": "string",
                                    "description": "Subject entity name (e.g. 'user', 'Alice')"
                                },
                                "predicate": {
                                    "type": "string",
                                    "description": "Predicate / relation type (e.g. 'prefers', 'works_for')"
                                },
                                "object": {
                                    "type": "string",
                                    "description": "Object entity name"
                                },
                                "confidence": {
                                    "type": "number",
                                    "description": "Confidence 0.0-1.0, default 0.8"
                                }
                            },
                            "required": ["subject", "predicate", "object"]
                        }
                    },
                    "index_text": {
                        "type": "boolean",
                        "description": "Whether to also build the full-text index, default true"
                    }
                },
                "required": ["kb_name", "facts"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args {
                kb_name: String,
                facts: Vec<FactInput>,
                #[serde(default = "default_true")]
                index_text: bool,
            }
            #[derive(Deserialize)]
            struct FactInput {
                subject: String,
                predicate: String,
                object: String,
                #[serde(default = "default_confidence")]
                confidence: f32,
            }
            fn default_true() -> bool {
                true
            }
            fn default_confidence() -> f32 {
                0.8
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let facts: Vec<knowledge_base::Fact> = parsed
                .facts
                .into_iter()
                .map(|f| {
                    knowledge_base::Fact::new(
                        f.subject,
                        f.predicate,
                        f.object,
                        f.confidence.clamp(0.0, 1.0),
                    )
                })
                .collect();

            check_kb_access(&self.allowed_kbs, &parsed.kb_name)?;

            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(string_err)?;
            let stored = km
                .add_facts_to_kb(&parsed.kb_name, &facts, parsed.index_text)
                .await
                .map_err(string_err)?;

            info!(kb = %parsed.kb_name, fact_count = stored.len(), "Facts added via tool");
            Ok(Content::Text(format!(
                "Added {} facts to the knowledge base",
                stored.len()
            )))
        })
    }
}

// ============================================================================
// QueryEntityFacts
// ============================================================================

pub struct QueryEntityFacts {
    access: Arc<dyn KnowledgeAccess>,
    allowed_kbs: Vec<String>,
}

impl QueryEntityFacts {
    pub fn new(access: Arc<dyn KnowledgeAccess>, allowed_kbs: Vec<String>) -> Self {
        Self {
            access,
            allowed_kbs,
        }
    }
}

impl ToolDyn for QueryEntityFacts {
    fn name(&self) -> String {
        "query_entity_facts".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "query_entity_facts".to_string(),
            description: "Query facts related to an entity in the knowledge graph. Traverses \
                          the graph along edges from the specified entity and returns \
                          reachable nodes with their relation edges. Suited for exploring \
                          relations between entities."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": {
                        "type": "string",
                        "description": "Name of the target knowledge base"
                    },
                    "entity_name": {
                        "type": "string",
                        "description": "Entity name (e.g. 'user', 'Alice')"
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "Max traversal depth (hops), default 2"
                    }
                },
                "required": ["kb_name", "entity_name"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args {
                kb_name: String,
                entity_name: String,
                #[serde(default = "default_max_depth")]
                max_depth: u32,
            }
            fn default_max_depth() -> u32 {
                2
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            check_kb_access(&self.allowed_kbs, &parsed.kb_name)?;

            // `max_depth` 直通遍历的分配与查询次数，必须夹紧（见 MAX_TRAVERSAL_DEPTH）。
            let max_depth = parsed.max_depth.min(knowledge_base::MAX_TRAVERSAL_DEPTH);

            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(string_err)?;

            let steps = km
                .query_entity_facts(&parsed.kb_name, &parsed.entity_name, max_depth)
                .await
                .map_err(string_err)?;

            let display: Vec<_> = steps
                .iter()
                .map(|step| {
                    json!({
                        "node": {
                            "id": step.node.id,
                            "labels": step.node.labels,
                            "properties": step.node.properties,
                            "distance": step.node.distance,
                        },
                        "via_edge": step.via_edge.as_ref().map(|e| e.as_label()),
                    })
                })
                .collect();

            serde_json::to_string_pretty(&display)
                .map_err(string_err)
                .map(Content::Text)
        })
    }
}

// ============================================================================
// 图删除 — DeleteEntityFact / DeleteEntityFacts / DeleteEntity
// ============================================================================
//
// 与文档删除的关键差异：**`ppa_profile` 硬守卫在图上无法复刻**。写路径丢弃
// 边属性（`Fact::new` 不设 `source`，`HelixDbBackend::add_edges` 只写 `weight`），
// 图上读不到来源标签，因此无法判断一条事实边是否属于偏好类记忆。图删除的保护
// 完全由**审计 + 回滚**承担，调用方不得以为它与文档删除受同等保护。
//
// 另一个差异：删除的**存在性判定必须由工具层「先读后删」完成**。HelixDB 的
// 写响应不报告删除条数（`DropEdgeLabeled` 后的 `Count` 数的是流经的源节点数），
// 且删除不存在的边是静默 no-op（HTTP 200）—— 不看前置读就没有任何信号。

/// 图事实在 `memory_audit.source` 上的显式标记。
///
/// 图上无真实来源可填（见本节开头），因此用固定标签把图事实与 `ppa_*` 文档
/// 区分开 —— 回滚重放按这个字段分流（事实走 `add_facts`，文档走文档重建）。
const GRAPH_FACT_SOURCE: &str = "graph_fact";

/// 事实边的审计快照 —— 写进 `MemoryAuditEntry::content`。
///
/// 回滚 = 按 `weight` 逐条重放 `add_facts`，因此必须存**真实 weight**：
/// 三元组本身已经是 `doc_id` / `title` 的内容，再存一遍无法还原置信度。
fn fact_snapshot_json(
    subject: &str,
    predicate: &str,
    object: &str,
    edges: &[knowledge_base::KnowledgeEdge],
) -> serde_json::Value {
    json!({
        "subject": subject,
        "predicate": predicate,
        "object": object,
        "edges": edges
            .iter()
            .map(|e| {
                json!({
                    "source_id": e.source_id,
                    "target_id": e.target_id,
                    "weight": e.weight,
                    "properties": e.properties,
                })
            })
            .collect::<Vec<_>>(),
    })
}

/// 实体级联删除的审计快照 —— 写进 `MemoryAuditEntry::content`。
///
/// 级联删除要重建的远不止一条边，因此快照必须包含删除前的完整状况：
/// 节点属性 + **两个方向**的全部关联边。`edges[]` 里每条边都带 `weight`，
/// 回滚按 `(source_id 对应的实体名, via 标签, target_id)` 逐条重放。
fn entity_snapshot_json(
    entity_name: &str,
    entity_id: &str,
    node: &Option<knowledge_base::GraphNode>,
    edges: &[knowledge_base::KnowledgeEdge],
) -> serde_json::Value {
    json!({
        "entity_name": entity_name,
        "entity_id": entity_id,
        "node": node.as_ref().map(|n| json!({
            "labels": n.labels,
            "properties": n.properties,
        })),
        "edges": edges
            .iter()
            .map(|e| {
                json!({
                    "source_id": e.source_id,
                    "target_id": e.target_id,
                    "predicate": e.edge_type.as_label(),
                    "weight": e.weight,
                    "properties": e.properties,
                })
            })
            .collect::<Vec<_>>(),
    })
}

// ----------------------------------------------------------------------------
// DeleteEntityFact
// ----------------------------------------------------------------------------

pub struct DeleteEntityFact {
    access: Arc<dyn KnowledgeAccess>,
    allowed_kbs: Vec<String>,
    memory_audit: Option<Arc<dyn MemoryAuditAccess>>,
}

impl DeleteEntityFact {
    pub fn new(
        access: Arc<dyn KnowledgeAccess>,
        allowed_kbs: Vec<String>,
        memory_audit: Option<Arc<dyn MemoryAuditAccess>>,
    ) -> Self {
        Self {
            access,
            allowed_kbs,
            memory_audit,
        }
    }
}

impl ToolDyn for DeleteEntityFact {
    fn name(&self) -> String {
        "delete_entity_fact".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delete_entity_fact".to_string(),
            description: "Delete a single fact (subject →predicate→ object) from the knowledge \
                          graph. The three arguments are exactly the fields returned by \
                          query_entity_facts: subject = the queried entity, predicate = via_edge, \
                          object = the neighbour's name. An audit record with a restorable \
                          snapshot is written before deletion. Deleting a fact that does not \
                          exist is an error, not a silent success."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": { "type": "string", "description": "Name of the target knowledge base" },
                    "subject": { "type": "string", "description": "Subject entity name (e.g. 'user', 'Alice')" },
                    "predicate": { "type": "string", "description": "Predicate / relation type (e.g. 'prefers', 'works_for')" },
                    "object": { "type": "string", "description": "Object entity name" }
                },
                "required": ["kb_name", "subject", "predicate", "object"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args {
                kb_name: String,
                subject: String,
                predicate: String,
                object: String,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            check_kb_access(&self.allowed_kbs, &parsed.kb_name)?;
            let audit = require_audit(&self.memory_audit)?;

            let (audit_id, deleted_edges) = delete_fact_one(
                &self.access,
                audit,
                &parsed.kb_name,
                &parsed.subject,
                &parsed.predicate,
                &parsed.object,
            )
            .await?;

            serde_json::to_string_pretty(&json!({
                "kb_name": parsed.kb_name,
                "subject": parsed.subject,
                "predicate": parsed.predicate,
                "object": parsed.object,
                "audit_id": audit_id,
                "deleted_edges": deleted_edges,
            }))
            .map_err(string_err)
            .map(Content::Text)
        })
    }
}

// ----------------------------------------------------------------------------
// DeleteEntityFacts
// ----------------------------------------------------------------------------

/// 批量删除的单个事实条目。
#[derive(Deserialize)]
struct FactArg {
    subject: String,
    predicate: String,
    object: String,
}

pub struct DeleteEntityFacts {
    access: Arc<dyn KnowledgeAccess>,
    allowed_kbs: Vec<String>,
    memory_audit: Option<Arc<dyn MemoryAuditAccess>>,
}

impl DeleteEntityFacts {
    pub fn new(
        access: Arc<dyn KnowledgeAccess>,
        allowed_kbs: Vec<String>,
        memory_audit: Option<Arc<dyn MemoryAuditAccess>>,
    ) -> Self {
        Self {
            access,
            allowed_kbs,
            memory_audit,
        }
    }
}

impl ToolDyn for DeleteEntityFacts {
    fn name(&self) -> String {
        "delete_entity_facts".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delete_entity_facts".to_string(),
            description: format!(
                "Delete multiple facts from the knowledge graph in one call (up to {MAX_BATCH_DELETIONS} \
                 per call). Each entry is a (subject, predicate, object) triple in the same shape as \
                 add_facts_to_knowledge_base. An audit record is written per fact before deletion. \
                 A single failure does not roll back the others; the result reports deleted and \
                 failed entries separately."
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": { "type": "string", "description": "Name of the target knowledge base" },
                    "facts": {
                        "type": "array",
                        "maxItems": MAX_BATCH_DELETIONS,
                        "description": "List of facts to delete",
                        "items": {
                            "type": "object",
                            "properties": {
                                "subject": { "type": "string" },
                                "predicate": { "type": "string" },
                                "object": { "type": "string" }
                            },
                            "required": ["subject", "predicate", "object"]
                        }
                    }
                },
                "required": ["kb_name", "facts"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args {
                kb_name: String,
                facts: Vec<FactArg>,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            if parsed.facts.len() > MAX_BATCH_DELETIONS {
                return Err(string_err(format!(
                    "Batch deletion accepts at most {} facts per call, got {}. Split into multiple calls.",
                    MAX_BATCH_DELETIONS,
                    parsed.facts.len()
                )));
            }
            check_kb_access(&self.allowed_kbs, &parsed.kb_name)?;
            let audit = require_audit(&self.memory_audit)?;

            // 逐条独立编排：单条失败不影响其余条目（各自的审计行已收口），
            // 全部结束后把已删与未删明细一起交还调用方。
            let mut deleted = Vec::new();
            let mut failed = Vec::new();
            for fact in &parsed.facts {
                match delete_fact_one(
                    &self.access,
                    audit,
                    &parsed.kb_name,
                    &fact.subject,
                    &fact.predicate,
                    &fact.object,
                )
                .await
                {
                    Ok((audit_id, deleted_edges)) => deleted.push(json!({
                        "subject": fact.subject,
                        "predicate": fact.predicate,
                        "object": fact.object,
                        "audit_id": audit_id,
                        "deleted_edges": deleted_edges,
                    })),
                    Err(e) => failed.push(json!({
                        "subject": fact.subject,
                        "predicate": fact.predicate,
                        "object": fact.object,
                        "error": e.to_string(),
                    })),
                }
            }

            let summary = json!({
                "kb_name": parsed.kb_name,
                "deleted": deleted,
                "failed": failed,
            });
            if failed.is_empty() {
                serde_json::to_string_pretty(&summary)
                    .map_err(string_err)
                    .map(Content::Text)
            } else {
                Err(string_err(summary))
            }
        })
    }
}

// ----------------------------------------------------------------------------
// DeleteEntity
// ----------------------------------------------------------------------------

pub struct DeleteEntity {
    access: Arc<dyn KnowledgeAccess>,
    allowed_kbs: Vec<String>,
    memory_audit: Option<Arc<dyn MemoryAuditAccess>>,
}

impl DeleteEntity {
    pub fn new(
        access: Arc<dyn KnowledgeAccess>,
        allowed_kbs: Vec<String>,
        memory_audit: Option<Arc<dyn MemoryAuditAccess>>,
    ) -> Self {
        Self {
            access,
            allowed_kbs,
            memory_audit,
        }
    }
}

impl ToolDyn for DeleteEntity {
    fn name(&self) -> String {
        "delete_entity".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delete_entity".to_string(),
            description: "Delete an entity node from the knowledge graph, optionally cascading \
                          all its relations. Deletion is refused while relations remain unless \
                          cascade is explicitly set to true — use delete_entity_fact for \
                          'forget one relation', and reserve cascade for 'forget the entity \
                          entirely'. An audit record with a full restore snapshot (node + both \
                          directions of edges) is written before deletion."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": { "type": "string", "description": "Name of the target knowledge base" },
                    "entity_name": { "type": "string", "description": "Entity name (e.g. 'user', 'Alice')" },
                    "cascade": {
                        "type": "boolean",
                        "description": "Must be true to delete an entity that still has relations. Default false."
                    }
                },
                "required": ["kb_name", "entity_name"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args {
                kb_name: String,
                entity_name: String,
                #[serde(default)]
                cascade: bool,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            check_kb_access(&self.allowed_kbs, &parsed.kb_name)?;
            let audit = require_audit(&self.memory_audit)?;

            let outcome = delete_entity_one(
                &self.access,
                audit,
                &parsed.kb_name,
                &parsed.entity_name,
                parsed.cascade,
            )
            .await?;

            serde_json::to_string_pretty(&json!({
                "kb_name": parsed.kb_name,
                "entity_name": parsed.entity_name,
                "entity_id": outcome.entity_id,
                "audit_id": outcome.audit_id,
                "removed_edges": outcome.removed_edges,
                "removed_nodes": outcome.removed_nodes,
            }))
            .map_err(string_err)
            .map(Content::Text)
        })
    }
}

// ============================================================================
// 删除编排（DeleteKbDocument / DeleteKbDocuments 共用）
// ============================================================================

/// 偏好类记忆的硬守卫标签 — source_path 为该值的文档一律拒绝删除，
/// 不接受参数绕过（用户显式删除 profile 走独立 REST 端点 + 二次确认）。
const PROFILE_SOURCE: &str = "ppa_profile";

/// agent 路径删除的执行者标识（'agent:@memory' | 'worker' | 'user'）。
const AGENT_DELETED_BY: &str = "agent:@memory";

/// agent 路径删除的默认原因（工具删除是人工整理动作）。
const AGENT_DELETE_REASON: &str = "manual_organize";

/// 批量删除的单次上限。
const MAX_BATCH_DELETIONS: usize = 50;

/// 单条删除的收口结果（outbox 已迁移为 done）。
struct DeletedDoc {
    doc_id: String,
    title: String,
    audit_id: i64,
    removed_chunks: usize,
}

impl DeletedDoc {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "doc_id": self.doc_id,
            "title": self.title,
            "audit_id": self.audit_id,
            "removed_chunks": self.removed_chunks,
        })
    }
}

/// 单条删除编排（outbox：pending → done / cancelled）。
///
/// 取原文（title / content / source 是审计记录与回滚重放的依据）→ profile
/// 硬守卫 → 写 pending 审计 → 删除 → 成功 mark_done / 失败 mark_cancelled
/// 并返回错误。删除失败时 pending 行必须收口为 cancelled，否则会留下
/// 永远无法回滚的假成功记录；文档不存在时不产生审计行。
///
/// 文档删除成功后 mark_done 失败只降级为 warn：删除已是既成事实，
/// 向调用方报错会得到「文档已删却报告失败」的假失败（模型可能重试并撞上
/// 文档不存在）；残留的 pending 行保留完整原文，可人工恢复，等待启动期对账。
async fn delete_one(
    access: &Arc<dyn KnowledgeAccess>,
    audit: &Arc<dyn MemoryAuditAccess>,
    kb_name: &str,
    doc_id: &str,
) -> Result<DeletedDoc, ToolError> {
    let km = access.knowledge_manager();
    km.ensure_loaded().await.map_err(string_err)?;

    let doc = km
        .get_document(kb_name, doc_id)
        .await
        .map_err(string_err)?
        .ok_or_else(|| {
            string_err(format!(
                "Document not found: '{doc_id}' (knowledge base '{kb_name}')"
            ))
        })?;

    if doc.source_path == PROFILE_SOURCE {
        return Err(string_err(format!(
            "Deletion rejected: document '{doc_id}' is a profile memory ({PROFILE_SOURCE}) and cannot be deleted via tools."
        )));
    }

    let audit_id = audit
        .record_pending(MemoryAuditEntry {
            user_id: access.user_id().to_string(),
            kb_name: kb_name.to_string(),
            doc_id: doc.id.clone(),
            title: doc.title.clone(),
            content: doc.content.clone(),
            source: doc.source_path.clone(),
            reason: AGENT_DELETE_REASON.to_string(),
            deleted_by: AGENT_DELETED_BY.to_string(),
            deleted_at: now_iso8601(),
            topic_key: None,
            successor_doc_id: None,
        })
        .await
        .map_err(string_err)?;

    match km.delete_document(kb_name, &doc.id).await {
        Ok(report) => {
            if let Err(e) = audit.mark_done(audit_id).await {
                warn!(
                    audit_id,
                    error = %e,
                    "Failed to mark audit row done; a pending row remains (original content retained for manual restore)"
                );
            }
            info!(
                kb = %kb_name,
                doc_id = %doc.id,
                audit_id,
                removed_chunks = report.removed_chunks,
                "Document deleted via tool"
            );
            Ok(DeletedDoc {
                doc_id: doc.id,
                title: doc.title,
                audit_id,
                removed_chunks: report.removed_chunks,
            })
        }
        Err(e) => {
            if let Err(cancel_err) = audit.mark_cancelled(audit_id).await {
                warn!(audit_id, error = %cancel_err, "Failed to mark audit row cancelled; a pending row may remain");
            }
            Err(string_err(e))
        }
    }
}

/// 单条事实删除编排（outbox：pending → done / cancelled），返回
/// `(audit_id, deleted_edges)`。
///
/// 顺序严格对齐 [`delete_one`]，唯一的结构性差异是**第 2 步的「先读」不可省略**：
/// 文档删除可以用「返回 `None`」判存在，图删除却必须显式读一次 —— HelixDB 删不
/// 存在的边是静默 no-op（HTTP 200、无错误、无信号），不看前置读就没有任何办法
/// 区分「删掉了」与「本来就没有」。
///
/// 因此「事实不存在」是**错误**而非幂等成功：报错是模型发现「我把 subject 拼错了」
/// 的唯一信号（`compute_entity_id` 会 trim + 小写，`小C` 与 `小c` 算出的 id 相同，
/// 但 `小 C` 不同），静默成功会让模型以为清理完成。
async fn delete_fact_one(
    access: &Arc<dyn KnowledgeAccess>,
    audit: &Arc<dyn MemoryAuditAccess>,
    kb_name: &str,
    subject: &str,
    predicate: &str,
    object: &str,
) -> Result<(i64, usize), ToolError> {
    let km = access.knowledge_manager();
    km.ensure_loaded().await.map_err(string_err)?;

    // 先读 — 既做存在性判定，也拿到审计需要的边快照（含真实 weight）。
    let edges = km
        .read_fact(kb_name, subject, predicate, object)
        .await
        .map_err(string_err)?;
    if edges.is_empty() {
        return Err(string_err(format!(
            "Fact not found: '{subject}' -[{predicate}]-> '{object}' (knowledge base '{kb_name}')"
        )));
    }

    let audit_id = audit
        .record_pending(MemoryAuditEntry {
            user_id: access.user_id().to_string(),
            kb_name: kb_name.to_string(),
            // 复用既有确定性 id 算法，不新造格式。
            doc_id: knowledge_base::Fact::compute_id(subject, predicate, object),
            title: format!("{subject} -[{predicate}]-> {object}"),
            content: fact_snapshot_json(subject, predicate, object, &edges).to_string(),
            source: GRAPH_FACT_SOURCE.to_string(),
            reason: AGENT_DELETE_REASON.to_string(),
            deleted_by: AGENT_DELETED_BY.to_string(),
            deleted_at: now_iso8601(),
            topic_key: None,
            successor_doc_id: None,
        })
        .await
        .map_err(string_err)?;

    match km.delete_fact(kb_name, subject, predicate, object).await {
        Ok(deleted_edges) => {
            if let Err(e) = audit.mark_done(audit_id).await {
                warn!(
                    audit_id,
                    error = %e,
                    "Failed to mark audit row done; a pending row remains (snapshot retained for manual restore)"
                );
            }
            info!(
                kb = %kb_name,
                predicate = %predicate,
                deleted_edges,
                audit_id,
                "Fact deleted via tool"
            );
            Ok((audit_id, deleted_edges))
        }
        Err(e) => {
            if let Err(cancel_err) = audit.mark_cancelled(audit_id).await {
                warn!(audit_id, error = %cancel_err, "Failed to mark audit row cancelled; a pending row may remain");
            }
            Err(string_err(e))
        }
    }
}

/// 实体级联删除的收口结果（outbox 已迁移为 done）。
struct EntityDeleteOutcome {
    entity_id: String,
    audit_id: i64,
    removed_edges: usize,
    removed_nodes: usize,
}

/// 实体删除编排（outbox：pending → done / cancelled）。
///
/// 比 [`delete_fact_one`] 多一道 `cascade` 门，且这道门必须在**写审计之前**：
/// 拒绝时什么都没删，不该留下任何审计行。「有残留边却不带 `cascade`」是最容易
/// 发生的误操作（模型想「忘掉小C的年龄」，却升级成「忘掉小C」），因此拒绝信息
/// 要明确给出边数并把 `cascade: true` 的路指出来。
async fn delete_entity_one(
    access: &Arc<dyn KnowledgeAccess>,
    audit: &Arc<dyn MemoryAuditAccess>,
    kb_name: &str,
    entity_name: &str,
    cascade: bool,
) -> Result<EntityDeleteOutcome, ToolError> {
    let km = access.knowledge_manager();
    km.ensure_loaded().await.map_err(string_err)?;

    let (entity_id, node, edges) = km
        .read_entity(kb_name, entity_name)
        .await
        .map_err(string_err)?;

    if node.is_none() && edges.is_empty() {
        return Err(string_err(format!(
            "Entity not found: '{entity_name}' (knowledge base '{kb_name}')"
        )));
    }

    if !cascade && !edges.is_empty() {
        return Err(string_err(format!(
            "Deletion rejected: entity '{entity_name}' still has {} relation(s). \
             Use delete_entity_fact to remove a single relation, or pass cascade: true \
             to delete the entity together with all its relations.",
            edges.len()
        )));
    }

    let audit_id = audit
        .record_pending(MemoryAuditEntry {
            user_id: access.user_id().to_string(),
            kb_name: kb_name.to_string(),
            doc_id: entity_id.clone(),
            title: format!("entity:{entity_name}"),
            content: entity_snapshot_json(entity_name, &entity_id, &node, &edges).to_string(),
            source: GRAPH_FACT_SOURCE.to_string(),
            reason: AGENT_DELETE_REASON.to_string(),
            deleted_by: AGENT_DELETED_BY.to_string(),
            deleted_at: now_iso8601(),
            topic_key: None,
            successor_doc_id: None,
        })
        .await
        .map_err(string_err)?;

    match km.delete_entity(kb_name, entity_name, cascade).await {
        Ok((removed_edges, removed_nodes)) => {
            if let Err(e) = audit.mark_done(audit_id).await {
                warn!(
                    audit_id,
                    error = %e,
                    "Failed to mark audit row done; a pending row remains (snapshot retained for manual restore)"
                );
            }
            info!(
                kb = %kb_name,
                removed_edges,
                removed_nodes,
                audit_id,
                "Entity deleted via tool"
            );
            Ok(EntityDeleteOutcome {
                entity_id,
                audit_id,
                removed_edges,
                removed_nodes,
            })
        }
        Err(e) => {
            if let Err(cancel_err) = audit.mark_cancelled(audit_id).await {
                warn!(audit_id, error = %cancel_err, "Failed to mark audit row cancelled; a pending row may remain");
            }
            Err(string_err(e))
        }
    }
}

/// 审计访问的 fail-closed 门：审计存储不可用时删除必须拒绝执行。
///
/// 与 workflow_access 等 Optional 依赖的 warn + skip 不同 — 删除工具始终注册，
/// 只在执行时被拒（审计不可用 ≠ 工具不存在）。
fn require_audit(
    memory_audit: &Option<Arc<dyn MemoryAuditAccess>>,
) -> Result<&Arc<dyn MemoryAuditAccess>, ToolError> {
    memory_audit
        .as_ref()
        .ok_or_else(|| string_err("Deletion rejected: audit storage is unavailable (deletions must be auditable and restorable)."))
}

// ============================================================================
// DeleteKbDocument
// ============================================================================

pub struct DeleteKbDocument {
    access: Arc<dyn KnowledgeAccess>,
    allowed_kbs: Vec<String>,
    memory_audit: Option<Arc<dyn MemoryAuditAccess>>,
}

impl DeleteKbDocument {
    pub fn new(
        access: Arc<dyn KnowledgeAccess>,
        allowed_kbs: Vec<String>,
        memory_audit: Option<Arc<dyn MemoryAuditAccess>>,
    ) -> Self {
        Self {
            access,
            allowed_kbs,
            memory_audit,
        }
    }
}

impl ToolDyn for DeleteKbDocument {
    fn name(&self) -> String {
        "delete_kb_document".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delete_kb_document".to_string(),
            description: "Delete a single document from a knowledge base. An audit record is \
                          written before deletion so the document can be restored by audit id; \
                          profile memories (ppa_profile) can never be deleted."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": { "type": "string", "description": "Name of the target knowledge base" },
                    "doc_id": {
                        "type": "string",
                        "description": "Id of the document to delete (query via get_knowledge_base_docs)"
                    }
                },
                "required": ["kb_name", "doc_id"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args {
                kb_name: String,
                doc_id: String,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            check_kb_access(&self.allowed_kbs, &parsed.kb_name)?;
            let audit = require_audit(&self.memory_audit)?;

            let deleted = delete_one(&self.access, audit, &parsed.kb_name, &parsed.doc_id).await?;

            serde_json::to_string_pretty(&json!({
                "kb_name": parsed.kb_name,
                "deleted": [deleted.to_json()],
            }))
            .map_err(string_err)
            .map(Content::Text)
        })
    }
}

// ============================================================================
// DeleteKbDocuments
// ============================================================================

pub struct DeleteKbDocuments {
    access: Arc<dyn KnowledgeAccess>,
    allowed_kbs: Vec<String>,
    memory_audit: Option<Arc<dyn MemoryAuditAccess>>,
}

impl DeleteKbDocuments {
    pub fn new(
        access: Arc<dyn KnowledgeAccess>,
        allowed_kbs: Vec<String>,
        memory_audit: Option<Arc<dyn MemoryAuditAccess>>,
    ) -> Self {
        Self {
            access,
            allowed_kbs,
            memory_audit,
        }
    }
}

impl ToolDyn for DeleteKbDocuments {
    fn name(&self) -> String {
        "delete_kb_documents".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delete_kb_documents".to_string(),
            description: "Delete multiple documents from a knowledge base (up to 50 per call). \
                          An audit record is written for each document before deletion so they \
                          can be restored by audit id; profile memories (ppa_profile) can never \
                          be deleted."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": { "type": "string", "description": "Name of the target knowledge base" },
                    "doc_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "maxItems": MAX_BATCH_DELETIONS,
                        "description": "Ids of the documents to delete"
                    }
                },
                "required": ["kb_name", "doc_ids"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args {
                kb_name: String,
                doc_ids: Vec<String>,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            if parsed.doc_ids.len() > MAX_BATCH_DELETIONS {
                return Err(string_err(format!(
                    "Batch deletion accepts at most {} documents per call, got {}. Split into multiple calls.",
                    MAX_BATCH_DELETIONS,
                    parsed.doc_ids.len()
                )));
            }
            check_kb_access(&self.allowed_kbs, &parsed.kb_name)?;
            let audit = require_audit(&self.memory_audit)?;

            // 逐条独立编排：单条失败不影响其余条目（各自的审计行已收口），
            // 全部结束后把已删与未删明细一起交还调用方。
            let mut deleted = Vec::new();
            let mut failed = Vec::new();
            for doc_id in &parsed.doc_ids {
                match delete_one(&self.access, audit, &parsed.kb_name, doc_id).await {
                    Ok(d) => deleted.push(d.to_json()),
                    Err(e) => failed.push(json!({ "doc_id": doc_id, "error": e.to_string() })),
                }
            }

            let summary = json!({
                "kb_name": parsed.kb_name,
                "deleted": deleted,
                "failed": failed,
            });
            if failed.is_empty() {
                serde_json::to_string_pretty(&summary)
                    .map_err(string_err)
                    .map(Content::Text)
            } else {
                Err(string_err(summary))
            }
        })
    }
}

// ============================================================================
// 测试 — outbox 顺序 / profile 硬守卫 / fail-closed / 批量上限
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::KnowledgeManager;
    use async_trait::async_trait;
    use knowledge_base::{BackendType, ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, Ordering};

    const KB: &str = "@private_memory";

    fn make_test_config(name: &str) -> KbConfig {
        KbConfig {
            name: name.to_string(),
            description: "测试记忆库".into(),
            embedding_model: FastembedModelTypeSerde::AllMiniLML6V2Q,
            chunking: ChunkingStrategySerde::FixedSize { size: 100 },
            backend: BackendType::InMemory,
            storage_path: None,
            default_storage_mode: Default::default(),
            helix_url: None,
        }
    }

    /// 建好 KB 的测试管理器（TempDir 由调用方持有保活）。
    async fn make_km() -> (tempfile::TempDir, Arc<KnowledgeManager>) {
        let tmp = tempfile::tempdir().unwrap();
        let km = Arc::new(KnowledgeManager::new(tmp.path().to_path_buf()));
        km.ensure_loaded().await.unwrap();
        km.create_kb(make_test_config(KB)).await.unwrap();
        (tmp, km)
    }

    struct StubAccess {
        manager: Arc<KnowledgeManager>,
    }
    impl KnowledgeAccess for StubAccess {
        fn user_id(&self) -> &str {
            "test-user"
        }
        fn knowledge_manager(&self) -> &Arc<KnowledgeManager> {
            &self.manager
        }
    }

    fn access_of(km: &Arc<KnowledgeManager>) -> Arc<dyn KnowledgeAccess> {
        Arc::new(StubAccess {
            manager: Arc::clone(km),
        })
    }

    /// Spy 审计 — 记录 outbox 事件顺序与完整审计行；可选在 pending 落库后立刻
    /// 破坏后续删除的前置条件，覆盖 pending → cancelled 分支。
    struct SpyAudit {
        log: Mutex<Vec<String>>,
        entries: Mutex<Vec<MemoryAuditEntry>>,
        next_id: AtomicI64,
        concurrent_delete: Option<(Arc<KnowledgeManager>, String)>,
        vanished_kb: Option<(Arc<KnowledgeManager>, String)>,
        fail_mark_done: bool,
    }

    impl SpyAudit {
        fn new() -> Self {
            Self {
                log: Mutex::new(Vec::new()),
                entries: Mutex::new(Vec::new()),
                next_id: AtomicI64::new(0),
                concurrent_delete: None,
                vanished_kb: None,
                fail_mark_done: false,
            }
        }

        fn with_concurrent_delete(mut self, km: Arc<KnowledgeManager>, kb_name: &str) -> Self {
            self.concurrent_delete = Some((km, kb_name.to_string()));
            self
        }

        /// pending 落库后立刻删掉整个知识库 — 后续删除必然失败。
        ///
        /// 事实删除没有等价的「并发删掉这条事实」注入点：图删除对不存在的边是
        /// 静默 no-op（返回 0 而非报错），拿它注入只会得到一个「成功」的空删。
        /// 因此改用「后端不可用」这类**真实错误**来触发 cancelled 分支。
        fn with_vanished_kb(mut self, km: Arc<KnowledgeManager>, kb_name: &str) -> Self {
            self.vanished_kb = Some((km, kb_name.to_string()));
            self
        }

        /// `mark_done` 恒返回错误 — 覆盖「删除已成功、收口失败」的降级路径。
        fn failing_mark_done(mut self) -> Self {
            self.fail_mark_done = true;
            self
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }

        fn entries(&self) -> Vec<MemoryAuditEntry> {
            self.entries.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl MemoryAuditAccess for SpyAudit {
        async fn record_pending(&self, entry: MemoryAuditEntry) -> Result<i64, String> {
            self.log
                .lock()
                .unwrap()
                .push(format!("pending:{}", entry.doc_id));
            self.entries.lock().unwrap().push(entry.clone());
            if let Some((km, kb)) = &self.concurrent_delete {
                km.delete_document(kb, &entry.doc_id)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            if let Some((km, kb)) = &self.vanished_kb {
                km.delete_kb(kb).await.map_err(|e| e.to_string())?;
            }
            Ok(self.next_id.fetch_add(1, Ordering::SeqCst) + 1)
        }

        async fn mark_done(&self, id: i64) -> Result<(), String> {
            self.log.lock().unwrap().push(format!("done:{id}"));
            if self.fail_mark_done {
                return Err("audit storage went away".to_string());
            }
            Ok(())
        }

        async fn mark_cancelled(&self, id: i64) -> Result<(), String> {
            self.log.lock().unwrap().push(format!("cancelled:{id}"));
            Ok(())
        }

        async fn get(&self, _id: i64) -> Result<Option<MemoryAuditEntry>, String> {
            Ok(None)
        }
    }

    fn single_tool(
        km: &Arc<KnowledgeManager>,
        audit: Option<Arc<dyn MemoryAuditAccess>>,
    ) -> DeleteKbDocument {
        DeleteKbDocument::new(access_of(km), vec![KB.to_string()], audit)
    }

    fn batch_tool(
        km: &Arc<KnowledgeManager>,
        audit: Option<Arc<dyn MemoryAuditAccess>>,
    ) -> DeleteKbDocuments {
        DeleteKbDocuments::new(access_of(km), vec![KB.to_string()], audit)
    }

    #[tokio::test]
    async fn delete_single_writes_outbox_pending_then_done() {
        let (_tmp, km) = make_km().await;
        let doc = km
            .add_text_to_kb(KB, "memory_1", "用户偏好 Rust 语言", "ppa_semantic")
            .await
            .unwrap();
        let spy = Arc::new(SpyAudit::new());
        let tool = single_tool(&km, Some(spy.clone()));

        let out = tool
            .call(json!({ "kb_name": KB, "doc_id": doc.id }).to_string())
            .await
            .unwrap();
        let text = out.text_view().into_owned();
        assert!(text.contains(&doc.id), "结果应包含被删 doc_id: {text}");

        assert_eq!(
            spy.log(),
            vec![format!("pending:{}", doc.id), "done:1".to_string()]
        );
        assert!(km.get_document(KB, &doc.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_failure_after_pending_marks_cancelled() {
        let (_tmp, km) = make_km().await;
        let doc = km
            .add_text_to_kb(KB, "memory_2", "用户在做 Rust 后端开发", "ppa_episodic")
            .await
            .unwrap();
        let spy = Arc::new(SpyAudit::new().with_concurrent_delete(Arc::clone(&km), KB));
        let tool = single_tool(&km, Some(spy.clone()));

        let err = tool
            .call(json!({ "kb_name": KB, "doc_id": doc.id }).to_string())
            .await
            .unwrap_err();

        assert_eq!(
            spy.log(),
            vec![format!("pending:{}", doc.id), "cancelled:1".to_string()]
        );
        assert!(
            err.to_string().contains("not found"),
            "删除失败信息应透传: {err}"
        );
    }

    #[tokio::test]
    async fn profile_memory_rejected_before_any_audit_write() {
        let (_tmp, km) = make_km().await;
        let doc = km
            .add_text_to_kb(KB, "memory_3", "用户自称小明", "ppa_profile")
            .await
            .unwrap();
        let spy = Arc::new(SpyAudit::new());
        let tool = single_tool(&km, Some(spy.clone()));

        let err = tool
            .call(json!({ "kb_name": KB, "doc_id": doc.id }).to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ppa_profile"), "{err}");
        assert!(spy.log().is_empty(), "profile 守卫不得写审计行");
        assert!(km.get_document(KB, &doc.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn missing_document_rejected_without_audit_row() {
        let (_tmp, km) = make_km().await;
        let spy = Arc::new(SpyAudit::new());
        let tool = single_tool(&km, Some(spy.clone()));

        let err = tool
            .call(json!({ "kb_name": KB, "doc_id": "no-such-doc" }).to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Document not found"), "{err}");
        assert!(spy.log().is_empty());
    }

    #[tokio::test]
    async fn batch_over_limit_rejected() {
        let (_tmp, km) = make_km().await;
        let spy = Arc::new(SpyAudit::new());
        let tool = batch_tool(&km, Some(spy.clone()));

        let ids: Vec<String> = (0..51).map(|i| format!("doc-{i}")).collect();
        let err = tool
            .call(json!({ "kb_name": KB, "doc_ids": ids }).to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("50"), "{err}");
        assert!(spy.log().is_empty(), "超限请求不得产生审计行");
    }

    #[tokio::test]
    async fn batch_delete_writes_outbox_for_each_doc() {
        let (_tmp, km) = make_km().await;
        let d1 = km
            .add_text_to_kb(KB, "memory_4", "用户喜欢黑咖啡", "ppa_semantic")
            .await
            .unwrap();
        let d2 = km
            .add_text_to_kb(KB, "memory_5", "用户周三下午开会", "ppa_episodic")
            .await
            .unwrap();
        let spy = Arc::new(SpyAudit::new());
        let tool = batch_tool(&km, Some(spy.clone()));

        tool.call(json!({ "kb_name": KB, "doc_ids": [d1.id, d2.id] }).to_string())
            .await
            .unwrap();

        assert_eq!(
            spy.log(),
            vec![
                format!("pending:{}", d1.id),
                "done:1".to_string(),
                format!("pending:{}", d2.id),
                "done:2".to_string(),
            ]
        );
        assert!(km.list_documents(KB, 0, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn batch_partial_failure_reports_deleted_and_failed() {
        let (_tmp, km) = make_km().await;
        let ok_doc = km
            .add_text_to_kb(KB, "memory_6", "用户在学日语", "ppa_semantic")
            .await
            .unwrap();
        let spy = Arc::new(SpyAudit::new());
        let tool = batch_tool(&km, Some(spy.clone()));

        let err = tool
            .call(json!({ "kb_name": KB, "doc_ids": [ok_doc.id, "no-such-doc"] }).to_string())
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains(&ok_doc.id) && text.contains("no-such-doc"),
            "汇总应同时包含已删与失败明细: {text}"
        );

        // 成功条目已收口 done；失败条目（文档不存在）不产生审计行
        assert_eq!(
            spy.log(),
            vec![format!("pending:{}", ok_doc.id), "done:1".to_string()]
        );
        assert!(km.get_document(KB, &ok_doc.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_rejected_without_audit_storage() {
        let (_tmp, km) = make_km().await;
        let doc = km
            .add_text_to_kb(KB, "memory_7", "用户住在杭州", "ppa_semantic")
            .await
            .unwrap();
        let tool = single_tool(&km, None);

        let err = tool
            .call(json!({ "kb_name": KB, "doc_id": doc.id }).to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("audit"), "{err}");
        // fail-closed：文档保持原样
        assert!(km.get_document(KB, &doc.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn kb_outside_allowlist_rejected() {
        let (_tmp, km) = make_km().await;
        let doc = km
            .add_text_to_kb(KB, "memory_8", "用户怕狗", "ppa_semantic")
            .await
            .unwrap();
        // 空白名单 = 无权访问任何 KB
        let tool = DeleteKbDocument::new(access_of(&km), vec![], Some(Arc::new(SpyAudit::new())));

        let err = tool
            .call(json!({ "kb_name": KB, "doc_id": doc.id }).to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Access denied"), "{err}");
        assert!(km.get_document(KB, &doc.id).await.unwrap().is_some());
    }

    // ════════════════════════════════════════════════════════════════════════
    // 图删除工具（delete_entity_fact / delete_entity_facts / delete_entity）
    // ════════════════════════════════════════════════════════════════════════

    fn fact_tool(
        km: &Arc<KnowledgeManager>,
        audit: Option<Arc<dyn MemoryAuditAccess>>,
    ) -> DeleteEntityFact {
        DeleteEntityFact::new(access_of(km), vec![KB.to_string()], audit)
    }

    fn facts_tool(
        km: &Arc<KnowledgeManager>,
        audit: Option<Arc<dyn MemoryAuditAccess>>,
    ) -> DeleteEntityFacts {
        DeleteEntityFacts::new(access_of(km), vec![KB.to_string()], audit)
    }

    fn entity_tool(
        km: &Arc<KnowledgeManager>,
        audit: Option<Arc<dyn MemoryAuditAccess>>,
    ) -> DeleteEntity {
        DeleteEntity::new(access_of(km), vec![KB.to_string()], audit)
    }

    /// 写入一组事实，返回管理器的持有句柄（TempDir 由调用方保活）。
    async fn seed_facts(
        facts: &[(&str, &str, &str, f32)],
    ) -> (tempfile::TempDir, Arc<KnowledgeManager>) {
        let (tmp, km) = make_km().await;
        let facts: Vec<knowledge_base::Fact> = facts
            .iter()
            .map(|(s, p, o, w)| knowledge_base::Fact::new(*s, *p, *o, *w))
            .collect();
        km.add_facts_to_kb(KB, &facts, false).await.unwrap();
        (tmp, km)
    }

    /// outbox 顺序必须是 `pending → done`，且删除真实生效。
    #[tokio::test]
    async fn delete_entity_fact_writes_outbox_pending_then_done() {
        let (_tmp, km) = seed_facts(&[("小C", "朋友", "chen", 0.95)]).await;
        let spy = Arc::new(SpyAudit::new());
        let tool = fact_tool(&km, Some(spy.clone()));

        let out = tool
            .call(
                json!({ "kb_name": KB, "subject": "小C", "predicate": "朋友", "object": "chen" })
                    .to_string(),
            )
            .await
            .unwrap();
        let Content::Text(text) = out else {
            panic!("expected text content");
        };
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["deleted_edges"], 1);
        assert_eq!(body["audit_id"], 1);

        // doc_id 复用 Fact 的确定性 id 算法
        assert_eq!(
            spy.log(),
            vec![
                format!(
                    "pending:{}",
                    knowledge_base::Fact::compute_id("小C", "朋友", "chen")
                ),
                "done:1".to_string()
            ]
        );

        // 审计行是可回滚的：source 为图事实标记，content 带真实 weight
        let entry = &spy.entries()[0];
        assert_eq!(entry.source, "graph_fact");
        assert_eq!(entry.title, "小C -[朋友]-> chen");
        let snapshot: serde_json::Value = serde_json::from_str(&entry.content).unwrap();
        assert_eq!(snapshot["subject"], "小C");
        assert!((snapshot["edges"][0]["weight"].as_f64().unwrap() - 0.95).abs() < 1e-6);

        assert!(
            km.read_fact(KB, "小C", "朋友", "chen")
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// 事实不存在 ⇒ 报错且**不产生审计行**（对齐文档删除的同类处理）。
    #[tokio::test]
    async fn delete_entity_fact_missing_fact_rejected_without_audit_row() {
        let (_tmp, km) = seed_facts(&[("小C", "朋友", "chen", 0.95)]).await;
        let spy = Arc::new(SpyAudit::new());
        let tool = fact_tool(&km, Some(spy.clone()));

        // 谓词不匹配 → 图上无此边（谓词是全匹配，无规范化）
        let err = tool
            .call(
                json!({ "kb_name": KB, "subject": "小C", "predicate": "同事", "object": "chen" })
                    .to_string(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Fact not found"), "{err}");
        assert!(spy.log().is_empty(), "不存在的事实不得产生审计行");

        // 原事实没被误删
        assert_eq!(
            km.read_fact(KB, "小C", "朋友", "chen").await.unwrap().len(),
            1
        );
    }

    /// 删除失败 ⇒ pending 必须收口为 cancelled，不得留下假成功记录。
    #[tokio::test]
    async fn delete_entity_fact_failure_after_pending_marks_cancelled() {
        let (_tmp, km) = seed_facts(&[("小C", "朋友", "chen", 0.95)]).await;
        let spy = Arc::new(SpyAudit::new().with_vanished_kb(Arc::clone(&km), KB));
        let tool = fact_tool(&km, Some(spy.clone()));

        let err = tool
            .call(
                json!({ "kb_name": KB, "subject": "小C", "predicate": "朋友", "object": "chen" })
                    .to_string(),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not found") || err.to_string().contains("Not found"),
            "后端错误应透传: {err}"
        );

        let log = spy.log();
        assert_eq!(log.len(), 2, "应为 pending + cancelled: {log:?}");
        assert!(log[0].starts_with("pending:"), "{log:?}");
        assert_eq!(log[1], "cancelled:1");
    }

    /// `mark_done` 失败只降级为 warn — 删除已是既成事实，向调用方报错会得到假失败。
    #[tokio::test]
    async fn delete_entity_fact_mark_done_failure_still_reports_success() {
        let (_tmp, km) = seed_facts(&[("小C", "朋友", "chen", 0.95)]).await;
        let spy = Arc::new(SpyAudit::new().failing_mark_done());
        let tool = fact_tool(&km, Some(spy.clone()));

        let out = tool
            .call(
                json!({ "kb_name": KB, "subject": "小C", "predicate": "朋友", "object": "chen" })
                    .to_string(),
            )
            .await
            .expect("mark_done 失败不得让工具失败");
        let Content::Text(text) = out else {
            panic!("expected text content");
        };
        assert!(text.contains("\"deleted_edges\": 1"), "{text}");
        assert!(
            km.read_fact(KB, "小C", "朋友", "chen")
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// 空 allowed_kbs（= 无权访问任何 KB）时工具在 IO 前拒绝，不写审计。
    #[tokio::test]
    async fn graph_delete_tools_reject_kb_outside_allowlist() {
        let (_tmp, km) = seed_facts(&[("小C", "朋友", "chen", 0.95)]).await;
        let spy = Arc::new(SpyAudit::new());

        let fact = DeleteEntityFact::new(access_of(&km), vec![], Some(spy.clone()));
        let err = fact
            .call(
                json!({ "kb_name": KB, "subject": "小C", "predicate": "朋友", "object": "chen" })
                    .to_string(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Access denied"), "{err}");

        let entity = DeleteEntity::new(access_of(&km), vec![], Some(spy.clone()));
        let err = entity
            .call(json!({ "kb_name": KB, "entity_name": "小C" }).to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Access denied"), "{err}");

        assert!(spy.log().is_empty());
        assert_eq!(
            km.read_fact(KB, "小C", "朋友", "chen").await.unwrap().len(),
            1
        );
    }

    /// 审计存储不可用 ⇒ 三个工具全部 fail-closed 拒绝，且不产生任何副作用。
    #[tokio::test]
    async fn graph_delete_tools_fail_closed_without_audit_storage() {
        let (_tmp, km) = seed_facts(&[("小C", "朋友", "chen", 0.95)]).await;

        let fact_err = fact_tool(&km, None)
            .call(
                json!({ "kb_name": KB, "subject": "小C", "predicate": "朋友", "object": "chen" })
                    .to_string(),
            )
            .await
            .unwrap_err();
        assert!(fact_err.to_string().contains("audit"), "{fact_err}");

        let facts_err = facts_tool(&km, None)
            .call(
                json!({ "kb_name": KB, "facts": [
                    { "subject": "小C", "predicate": "朋友", "object": "chen" }
                ] })
                .to_string(),
            )
            .await
            .unwrap_err();
        assert!(facts_err.to_string().contains("audit"), "{facts_err}");

        let entity_err = entity_tool(&km, None)
            .call(json!({ "kb_name": KB, "entity_name": "小C", "cascade": true }).to_string())
            .await
            .unwrap_err();
        assert!(entity_err.to_string().contains("audit"), "{entity_err}");

        // fail-closed：图保持原样
        assert_eq!(
            km.read_fact(KB, "小C", "朋友", "chen").await.unwrap().len(),
            1
        );
    }

    /// 批量上限：schema 是给模型的提示，运行时断言才是真正的门。
    #[tokio::test]
    async fn delete_entity_facts_over_limit_rejected() {
        let (_tmp, km) = make_km().await;
        let spy = Arc::new(SpyAudit::new());
        let tool = facts_tool(&km, Some(spy.clone()));

        let facts: Vec<serde_json::Value> = (0..51)
            .map(|i| json!({ "subject": format!("s{i}"), "predicate": "p", "object": "o" }))
            .collect();
        let err = tool
            .call(json!({ "kb_name": KB, "facts": facts }).to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("50"), "{err}");
        assert!(spy.log().is_empty(), "超限请求不得产生审计行");
    }

    /// 批量部分失败：成功条目已收口 done，汇总 Err 同时含 deleted 与 failed 明细。
    #[tokio::test]
    async fn delete_entity_facts_partial_failure_reports_both_sides() {
        let (_tmp, km) =
            seed_facts(&[("小C", "朋友", "chen", 0.95), ("小C", "年龄", "30", 0.9)]).await;
        let spy = Arc::new(SpyAudit::new());
        let tool = facts_tool(&km, Some(spy.clone()));

        let err = tool
            .call(
                json!({ "kb_name": KB, "facts": [
                    { "subject": "小C", "predicate": "朋友", "object": "chen" },
                    { "subject": "小C", "predicate": "同事", "object": "chen" }
                ] })
                .to_string(),
            )
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("\"deleted\"")
                && text.contains("chen")
                && text.contains("Fact not found"),
            "汇总应同时包含已删与失败明细: {text}"
        );

        // 成功条目已删且收口 done；失败条目（事实不存在）不产生审计行
        assert!(
            km.read_fact(KB, "小C", "朋友", "chen")
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            spy.log(),
            vec![
                format!(
                    "pending:{}",
                    knowledge_base::Fact::compute_id("小C", "朋友", "chen")
                ),
                "done:1".to_string()
            ]
        );

        // 未在本次请求中的事实不受影响
        assert_eq!(
            km.read_fact(KB, "小C", "年龄", "30").await.unwrap().len(),
            1
        );
    }

    /// `cascade: false` 且实体仍有边 ⇒ 拒绝，且**在写审计之前**就拒绝。
    #[tokio::test]
    async fn delete_entity_without_cascade_rejected_before_audit() {
        let (_tmp, km) =
            seed_facts(&[("小C", "朋友", "chen", 0.95), ("小C", "年龄", "30", 0.9)]).await;
        let spy = Arc::new(SpyAudit::new());
        let tool = entity_tool(&km, Some(spy.clone()));

        let err = tool
            .call(json!({ "kb_name": KB, "entity_name": "小C" }).to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cascade"), "{err}");
        assert!(spy.log().is_empty(), "拒绝时不得写审计行");

        // 什么都没动
        assert_eq!(
            km.read_fact(KB, "小C", "朋友", "chen").await.unwrap().len(),
            1
        );
        let (_, node, _) = km.read_entity(KB, "小C").await.unwrap();
        assert!(node.is_some());
    }

    /// `cascade: true` ⇒ 边与节点俱删，审计快照含节点属性与双向边。
    #[tokio::test]
    async fn delete_entity_with_cascade_writes_restorable_snapshot() {
        let (_tmp, km) = seed_facts(&[
            ("小C", "朋友", "chen", 0.95),
            ("小C", "年龄", "30", 0.9),
            // 反向边：chen 也指向 小C —— 级联必须双向都带走
            ("chen", "同事", "小C", 0.5),
        ])
        .await;
        let spy = Arc::new(SpyAudit::new());
        let tool = entity_tool(&km, Some(spy.clone()));

        let out = tool
            .call(json!({ "kb_name": KB, "entity_name": "小C", "cascade": true }).to_string())
            .await
            .unwrap();
        let Content::Text(text) = out else {
            panic!("expected text content");
        };
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["removed_nodes"], 1);
        assert_eq!(body["removed_edges"], 3, "两条出边 + 一条入边: {text}");
        assert!(
            body["entity_id"]
                .as_str()
                .unwrap()
                .starts_with("entity:Entity:"),
            "{text}"
        );

        // 审计行落的是实体 id，快照里节点属性与三条边都在
        let entry = &spy.entries()[0];
        assert_eq!(entry.doc_id, body["entity_id"].as_str().unwrap());
        assert_eq!(entry.source, "graph_fact");
        let snapshot: serde_json::Value = serde_json::from_str(&entry.content).unwrap();
        assert_eq!(snapshot["node"]["properties"]["name"], "小C");
        assert_eq!(snapshot["edges"].as_array().unwrap().len(), 3);

        assert_eq!(spy.log().len(), 2);
        assert_eq!(spy.log()[1], "done:1");

        // 边全没了，节点也没了；对端实体存活（孤儿语义）
        assert!(
            km.read_fact(KB, "小C", "朋友", "chen")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            km.read_fact(KB, "小C", "年龄", "30")
                .await
                .unwrap()
                .is_empty()
        );
        let (_, chen_node, chen_edges) = km.read_entity(KB, "chen").await.unwrap();
        assert!(chen_node.is_some(), "对端实体应存活");
        assert!(chen_edges.is_empty(), "对端的入射边已被级联带走");
    }

    /// 实体不存在 ⇒ `Entity not found`，不写审计行。
    #[tokio::test]
    async fn delete_entity_missing_entity_rejected_without_audit_row() {
        let (_tmp, km) = seed_facts(&[("小C", "朋友", "chen", 0.95)]).await;
        let spy = Arc::new(SpyAudit::new());
        let tool = entity_tool(&km, Some(spy.clone()));

        let err = tool
            .call(json!({ "kb_name": KB, "entity_name": "查无此人", "cascade": true }).to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Entity not found"), "{err}");
        assert!(spy.log().is_empty());
    }

    fn docs_tool(km: &Arc<KnowledgeManager>) -> GetKnowledgeBaseDocs {
        GetKnowledgeBaseDocs::new(access_of(km), vec![KB.to_string()])
    }

    /// 分页翻页可取全量：120 条 → 50/50/20 三页取完，has_more 按页推进。
    #[tokio::test]
    async fn docs_pagination_walks_all_pages() {
        let (_tmp, km) = make_km().await;
        for i in 0..120 {
            km.add_text_to_kb(
                KB,
                &format!("memory_{i}"),
                &format!("事实 {i}"),
                "ppa_semantic",
            )
            .await
            .unwrap();
        }
        let tool = docs_tool(&km);

        let mut collected = Vec::new();
        let mut offset = 0usize;
        loop {
            let out = tool
                .call(json!({ "kb_name": KB, "offset": offset, "limit": 50 }).to_string())
                .await
                .unwrap();
            let Content::Text(text) = out else {
                panic!("expected text content");
            };
            let body: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(body["offset"], offset);
            assert_eq!(body["limit"], 50);
            for doc in body["documents"].as_array().unwrap() {
                collected.push(doc["id"].as_str().unwrap().to_string());
            }
            if !body["has_more"].as_bool().unwrap() {
                break;
            }
            offset += 50;
        }
        assert_eq!(collected.len(), 120);
        let mut sorted = collected.clone();
        sorted.sort();
        collected.sort();
        assert_eq!(collected, sorted, "翻页结果不得重复或遗漏");
    }

    /// source 过滤只命中前缀匹配条目，且 offset 按「过滤后」计数。
    #[tokio::test]
    async fn docs_source_filter_and_filtered_offset() {
        let (_tmp, km) = make_km().await;
        for i in 0..3 {
            km.add_text_to_kb(KB, &format!("ep_{i}"), &format!("事件 {i}"), "ppa_episodic")
                .await
                .unwrap();
        }
        for i in 0..2 {
            km.add_text_to_kb(KB, &format!("se_{i}"), &format!("事实 {i}"), "ppa_semantic")
                .await
                .unwrap();
        }
        let tool = docs_tool(&km);

        let out = tool
            .call(json!({ "kb_name": KB, "source_filter": "ppa_episodic" }).to_string())
            .await
            .unwrap();
        let Content::Text(text) = out else {
            panic!("expected text content");
        };
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        let sources: Vec<&str> = body["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["source"].as_str().unwrap())
            .collect();
        assert_eq!(sources.len(), 3);
        assert!(sources.iter().all(|s| s.starts_with("ppa_episodic")));
        assert_eq!(body["has_more"], false);

        // offset=1 跳过的是「过滤后」的第一条 episodic，而不是原始列表第一条
        let out = tool
            .call(
                json!({ "kb_name": KB, "source_filter": "ppa_episodic", "offset": 1, "limit": 50 })
                    .to_string(),
            )
            .await
            .unwrap();
        let Content::Text(text) = out else {
            panic!("expected text content");
        };
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["count"], 2);
    }

    /// limit 上限 200：请求更大值被钳制；默认 limit 50。
    #[tokio::test]
    async fn docs_limit_clamped_and_default() {
        let (_tmp, km) = make_km().await;
        for i in 0..7 {
            km.add_text_to_kb(KB, &format!("m_{i}"), &format!("事实 {i}"), "ppa_semantic")
                .await
                .unwrap();
        }
        let tool = docs_tool(&km);

        // 超大 limit 被钳制到 200 — 7 条全返回且 has_more=false
        let out = tool
            .call(json!({ "kb_name": KB, "limit": 100000 }).to_string())
            .await
            .unwrap();
        let Content::Text(text) = out else {
            panic!("expected text content");
        };
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["limit"], 200);
        assert_eq!(body["count"], 7);

        // 缺省 limit = 50
        let out = tool
            .call(json!({ "kb_name": KB }).to_string())
            .await
            .unwrap();
        let Content::Text(text) = out else {
            panic!("expected text content");
        };
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["limit"], 50);
        assert_eq!(body["count"], 7);
    }
}
