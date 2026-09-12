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

            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(string_err)?;

            let steps = km
                .query_entity_facts(&parsed.kb_name, &parsed.entity_name, parsed.max_depth)
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

    /// Spy 审计 — 记录 outbox 事件顺序；可选在 pending 落库后立即删掉该文档，
    /// 模拟「审计写入后文档被并发删除」，覆盖 pending → cancelled 分支。
    struct SpyAudit {
        log: Mutex<Vec<String>>,
        next_id: AtomicI64,
        concurrent_delete: Option<(Arc<KnowledgeManager>, String)>,
    }

    impl SpyAudit {
        fn new() -> Self {
            Self {
                log: Mutex::new(Vec::new()),
                next_id: AtomicI64::new(0),
                concurrent_delete: None,
            }
        }

        fn with_concurrent_delete(mut self, km: Arc<KnowledgeManager>, kb_name: &str) -> Self {
            self.concurrent_delete = Some((km, kb_name.to_string()));
            self
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl MemoryAuditAccess for SpyAudit {
        async fn record_pending(&self, entry: MemoryAuditEntry) -> Result<i64, String> {
            self.log
                .lock()
                .unwrap()
                .push(format!("pending:{}", entry.doc_id));
            if let Some((km, kb)) = &self.concurrent_delete {
                km.delete_document(kb, &entry.doc_id)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Ok(self.next_id.fetch_add(1, Ordering::SeqCst) + 1)
        }

        async fn mark_done(&self, id: i64) -> Result<(), String> {
            self.log.lock().unwrap().push(format!("done:{id}"));
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
