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
            fn default_top_k() -> usize {
                5
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(string_err)?;

            let formatted = if let Some(name) = &parsed.kb_name {
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
                let all = km
                    .search_all(&parsed.query, parsed.top_k)
                    .await
                    .map_err(string_err)?;
                all.into_iter()
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

            serde_json::to_string_pretty(&formatted).map_err(string_err)
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
            km.ensure_loaded().await.map_err(string_err)?;

            let infos = km.list_kbs().await.map_err(string_err)?;
            let display: Vec<_> = infos
                .into_iter()
                .map(|i| {
                    json!({
                        "name": i.name, "description": i.description, "backend": i.backend,
                        "embedding_model": i.embedding_model, "document_count": i.document_count,
                        "chunk_count": i.chunk_count,
                    })
                })
                .collect();

            serde_json::to_string_pretty(&display).map_err(string_err)
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
            description: "添加文本内容到知识库。支持指定存储模式以控制摄入路径。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": { "type": "string", "description": "目标知识库名称" },
                    "title": { "type": "string", "description": "内容标题" },
                    "content": { "type": "string", "description": "文本内容" },
                    "source": { "type": "string", "description": "来源标识" },
                    "storage_mode": {
                        "type": "string",
                        "enum": ["full", "vector_only", "text_only", "graph_only", "vector_and_text", "vector_and_graph", "text_and_graph"],
                        "description": "存储模式：full(全部)、vector_only(仅向量)、text_only(仅全文)、graph_only(仅图谱)、vector_and_text、vector_and_graph、text_and_graph。默认 full"
                    }
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

            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(string_err)?;

            let doc = km
                .add_text_to_kb_with_mode(&parsed.kb_name, &parsed.title, &parsed.content, &source, mode)
                .await
                .map_err(string_err)?;

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
            struct Args {
                kb_name: Option<String>,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(string_err)?;

            if let Some(name) = parsed.kb_name {
                let report = km.sync_kb(&name).await.map_err(string_err)?;
                Ok(format!(
                    "知识库 '{}' 同步完成:\n- 新增: {} 个文件\n- 更新: {} 个文件\n- 删除: {} 个文件\n- 跳过: {} 个文件\n- 耗时: {}ms",
                    report.kb_name,
                    report.added,
                    report.updated,
                    report.removed,
                    report.skipped,
                    report.duration_ms
                ))
            } else {
                let all_reports = km.sync_all().await.map_err(string_err)?;
                let lines: Vec<String> = all_reports
                    .iter()
                    .map(|(name, report)| {
                        format!(
                            "  '{}': +{}/~{} 跳过{}",
                            name, report.added, report.updated, report.skipped
                        )
                    })
                    .collect();
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
            description: "查看指定知识库中的文档列表。".to_string(),
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
            struct Args {
                kb_name: String,
            }

            let parsed: Args = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(string_err)?;

            let docs = km
                .list_documents(&parsed.kb_name, 0, 100)
                .await
                .map_err(string_err)?;

            let display: Vec<_> = docs
                .into_iter()
                .map(|d| {
                    json!({
                        "id": d.id, "title": d.title, "source": d.source_path,
                    })
                })
                .collect();

            serde_json::to_string_pretty(&display).map_err(string_err)
        })
    }
}

// ============================================================================
// AddFactsToKnowledgeBase
// ============================================================================

pub struct AddFactsToKnowledgeBase {
    access: Arc<dyn KnowledgeAccess>,
}

impl AddFactsToKnowledgeBase {
    pub fn new(access: Arc<dyn KnowledgeAccess>) -> Self {
        Self { access }
    }
}

impl ToolDyn for AddFactsToKnowledgeBase {
    fn name(&self) -> String {
        "add_facts_to_knowledge_base".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "add_facts_to_knowledge_base".to_string(),
            description: "将结构化事实（三元组）直接写入知识图谱，跳过文档分块和嵌入。\
                          适合存储用户偏好、关系、事件等离散知识。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": {
                        "type": "string",
                        "description": "目标知识库名称"
                    },
                    "facts": {
                        "type": "array",
                        "description": "事实列表",
                        "items": {
                            "type": "object",
                            "properties": {
                                "subject": {
                                    "type": "string",
                                    "description": "主体实体名称（如 '用户'、'张三'）"
                                },
                                "predicate": {
                                    "type": "string",
                                    "description": "谓词/关系类型（如 'prefers'、'works_for'）"
                                },
                                "object": {
                                    "type": "string",
                                    "description": "客体实体名称"
                                },
                                "confidence": {
                                    "type": "number",
                                    "description": "置信度 0.0-1.0，默认 0.8"
                                }
                            },
                            "required": ["subject", "predicate", "object"]
                        }
                    },
                    "index_text": {
                        "type": "boolean",
                        "description": "是否同时建立全文索引，默认 true"
                    }
                },
                "required": ["kb_name", "facts"]
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

            let km = self.access.knowledge_manager();
            km.ensure_loaded().await.map_err(string_err)?;
            let stored = km
                .add_facts_to_kb(&parsed.kb_name, &facts, parsed.index_text)
                .await
                .map_err(string_err)?;

            Ok(format!("已添加 {} 条事实到知识库", stored.len()))
        })
    }
}
