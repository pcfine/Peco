// ============================================================================
// Knowledge Tools — 5 个知识库工具（依赖注入版）
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

use crate::workspace::KnowledgeAccess;

use super::{StringError, ToolDyn, ToolError};

fn string_err(msg: impl ToString) -> ToolError {
    ToolError::ToolCallError(Box::new(StringError(msg.to_string())))
}

// ============================================================================
// SearchKnowledge
// ============================================================================

pub struct SearchKnowledge {
    access: Arc<dyn KnowledgeAccess>,
}

impl SearchKnowledge {
    pub fn new(access: Arc<dyn KnowledgeAccess>) -> Self {
        Self { access }
    }
}

impl ToolDyn for SearchKnowledge {
    fn name(&self) -> String {
        "search_knowledge".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "search_knowledge".to_string(),
            description: "在知识库中搜索信息。支持跨所有知识库或指定单个知识库的混合检索。"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "搜索查询，支持自然语言描述。"
                    },
                    "kb_name": {
                        "type": "string",
                        "description": "指定知识库名称。不指定则搜索所有知识库。"
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "返回结果数量，默认 5"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args {
                query: String,
                kb_name: Option<String>,
                #[serde(default = "default_top_k")]
                top_k: usize,
            }
            fn default_top_k() -> usize { 5 }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(|e| string_err(e))?;

            let formatted = if let Some(name) = &parsed.kb_name {
                let results = km.search_kb(name, &parsed.query, parsed.top_k)
                    .await.map_err(|e| string_err(e))?;
                results.iter().map(|r| json!({
                    "kb": name.clone(), "title": r.title, "snippet": r.snippet,
                    "score": r.score, "source": r.source_path,
                })).collect::<Vec<_>>()
            } else {
                let all = km.search_all(&parsed.query, parsed.top_k)
                    .await.map_err(|e| string_err(e))?;
                all.into_iter().flat_map(|(kb_name, hits)| {
                    hits.into_iter().map(move |h| json!({
                        "kb": kb_name.clone(), "title": h.title, "snippet": h.snippet,
                        "score": h.score, "source": h.source_path,
                    }))
                }).collect::<Vec<_>>()
            };

            serde_json::to_string_pretty(&formatted).map_err(|e| string_err(e))
        })
    }
}

// ============================================================================
// ListKnowledgeBases
// ============================================================================

pub struct ListKnowledgeBases {
    access: Arc<dyn KnowledgeAccess>,
}

impl ListKnowledgeBases {
    pub fn new(access: Arc<dyn KnowledgeAccess>) -> Self {
        Self { access }
    }
}

impl ToolDyn for ListKnowledgeBases {
    fn name(&self) -> String {
        "list_knowledge_bases".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_knowledge_bases".to_string(),
            description: "列出所有可用的知识库，包括名称、描述、文档数量、后端类型等信息。"
                .to_string(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }

    fn call<'a>(
        &'a self,
        _args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(|e| string_err(e))?;

            let infos = km.list_kbs().await.map_err(|e| string_err(e))?;
            let display: Vec<_> = infos.into_iter().map(|i| json!({
                "name": i.name, "description": i.description, "backend": i.backend,
                "embedding_model": i.embedding_model, "document_count": i.document_count,
                "chunk_count": i.chunk_count,
            })).collect();

            serde_json::to_string_pretty(&display).map_err(|e| string_err(e))
        })
    }
}

// ============================================================================
// AddToKnowledgeBase
// ============================================================================

pub struct AddToKnowledgeBase {
    access: Arc<dyn KnowledgeAccess>,
}

impl AddToKnowledgeBase {
    pub fn new(access: Arc<dyn KnowledgeAccess>) -> Self {
        Self { access }
    }
}

impl ToolDyn for AddToKnowledgeBase {
    fn name(&self) -> String {
        "add_to_knowledge_base".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "add_to_knowledge_base".to_string(),
            description: "添加文本内容到知识库。"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": { "type": "string", "description": "目标知识库名称" },
                    "title": { "type": "string", "description": "内容标题" },
                    "content": { "type": "string", "description": "文本内容" },
                    "source": { "type": "string", "description": "来源标识" }
                },
                "required": ["kb_name", "title", "content"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args {
                kb_name: String,
                title: String,
                content: String,
                #[serde(default)]
                source: Option<String>,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let source = parsed.source.unwrap_or_else(|| "manual".to_string());

            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(|e| string_err(e))?;

            let doc = km.add_text_to_kb(&parsed.kb_name, &parsed.title, &parsed.content, &source)
                .await.map_err(|e| string_err(e))?;

            Ok(format!("已添加文档: {} (id: {})", doc.title, doc.id))
        })
    }
}

// ============================================================================
// SyncKnowledgeBase
// ============================================================================

pub struct SyncKnowledgeBase {
    access: Arc<dyn KnowledgeAccess>,
}

impl SyncKnowledgeBase {
    pub fn new(access: Arc<dyn KnowledgeAccess>) -> Self {
        Self { access }
    }
}

impl ToolDyn for SyncKnowledgeBase {
    fn name(&self) -> String {
        "sync_knowledge_base".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "sync_knowledge_base".to_string(),
            description: "同步知识库：扫描原始文档目录，自动检测新文件、变更文件、已删除文件，增量更新向量数据库。"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": { "type": "string", "description": "知识库名称。不指定则同步所有知识库。" }
                },
                "required": []
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args { kb_name: Option<String> }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(|e| string_err(e))?;

            if let Some(name) = parsed.kb_name {
                let report = km.sync_kb(&name).await.map_err(|e| string_err(e))?;
                Ok(format!(
                    "知识库 '{}' 同步完成:\n- 新增: {} 个文件\n- 更新: {} 个文件\n- 删除: {} 个文件\n- 跳过: {} 个文件\n- 耗时: {}ms",
                    report.kb_name, report.added, report.updated, report.removed, report.skipped, report.duration_ms
                ))
            } else {
                let all_reports = km.sync_all().await.map_err(|e| string_err(e))?;
                let lines: Vec<String> = all_reports.iter().map(|(name, report)| {
                    format!("  '{}': +{}/~{} 跳过{}", name, report.added, report.updated, report.skipped)
                }).collect();
                Ok(format!("所有知识库同步完成:\n{}", lines.join("\n")))
            }
        })
    }
}

// ============================================================================
// GetKnowledgeBaseDocs
// ============================================================================

pub struct GetKnowledgeBaseDocs {
    access: Arc<dyn KnowledgeAccess>,
}

impl GetKnowledgeBaseDocs {
    pub fn new(access: Arc<dyn KnowledgeAccess>) -> Self {
        Self { access }
    }
}

impl ToolDyn for GetKnowledgeBaseDocs {
    fn name(&self) -> String {
        "get_knowledge_base_docs".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "get_knowledge_base_docs".to_string(),
            description: "查看指定知识库中的文档列表。"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": { "type": "string", "description": "知识库名称" }
                },
                "required": ["kb_name"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Args { kb_name: String }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(|e| string_err(e))?;

            let docs = km.list_documents(&parsed.kb_name, 0, 100)
                .await.map_err(|e| string_err(e))?;

            let display: Vec<_> = docs.into_iter().map(|d| json!({
                "id": d.id, "title": d.title, "source": d.source_path,
            })).collect();

            serde_json::to_string_pretty(&display).map_err(|e| string_err(e))
        })
    }
}
