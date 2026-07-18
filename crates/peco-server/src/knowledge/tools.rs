// ============================================================================
// Web 知识工具 — 替换全局知识工具实现用户隔离
// ============================================================================
//
// 全局知识工具（peco-core::knowledge::tools）使用 GlobalHandler 单例，
// 不区分用户。这些 Web 版本在构造时注入 user_id，通过 WebKnowledgeManager
// 路由到用户专属的知识库实例。
//
// 每个工具直接实现 ToolDyn trait（不走 #[peco_tool] 宏），与 Phase 4
// 的 WebDelegateSubAgentTool 模式一致。

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use peco_core::tools::{StringError, ToolDyn, ToolError};
use serde::Deserialize;
use serde_json::json;

use super::manager::WebKnowledgeManager;

// ============================================================================
// 辅助函数
// ============================================================================

fn string_err(msg: impl ToString) -> ToolError {
    ToolError::ToolCallError(Box::new(StringError(msg.to_string())))
}

// ============================================================================
// WebSearchKnowledge
// ============================================================================

/// Web 版 `search_knowledge` 工具。
///
/// 搜索用户专属的知识库，而非全局知识库。
pub struct WebSearchKnowledge {
    manager: Arc<WebKnowledgeManager>,
    user_id: String,
}

impl WebSearchKnowledge {
    pub fn new(manager: Arc<WebKnowledgeManager>, user_id: String) -> Self {
        Self { manager, user_id }
    }
}

impl ToolDyn for WebSearchKnowledge {
    fn name(&self) -> String {
        "search_knowledge".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "search_knowledge".to_string(),
            description: "在知识库中搜索信息。支持跨所有知识库或指定单个知识库的混合检索（语义+关键词+知识图谱）。"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "搜索查询，支持自然语言描述。示例：'Rust 异步编程怎么做？'"
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
            let parsed: SearchKnowledgeArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let top_k = parsed.top_k.unwrap_or(5);
            let km = self
                .manager
                .get_manager(&self.user_id)
                .await
                .map_err(|e| string_err(e))?;

            let formatted = if let Some(name) = &parsed.kb_name {
                let results = km
                    .search_kb(name, &parsed.query, top_k)
                    .await
                    .map_err(|e| string_err(e))?;
                results
                    .iter()
                    .map(|r| {
                        json!({
                            "kb": name.clone(),
                            "title": r.title,
                            "snippet": r.snippet,
                            "score": r.score,
                            "source": r.source_path,
                        })
                    })
                    .collect::<Vec<_>>()
            } else {
                let all = km
                    .search_all(&parsed.query, top_k)
                    .await
                    .map_err(|e| string_err(e))?;
                all.into_iter()
                    .flat_map(|(kb_name, hits)| {
                        hits.into_iter().map(move |h| {
                            json!({
                                "kb": kb_name.clone(),
                                "title": h.title,
                                "snippet": h.snippet,
                                "score": h.score,
                                "source": h.source_path,
                            })
                        })
                    })
                    .collect::<Vec<_>>()
            };

            serde_json::to_string_pretty(&formatted).map_err(|e| string_err(e))
        })
    }
}

#[derive(Debug, Deserialize)]
struct SearchKnowledgeArgs {
    query: String,
    kb_name: Option<String>,
    top_k: Option<usize>,
}

// ============================================================================
// WebListKnowledgeBases
// ============================================================================

/// Web 版 `list_knowledge_bases` 工具。
pub struct WebListKnowledgeBases {
    manager: Arc<WebKnowledgeManager>,
    user_id: String,
}

impl WebListKnowledgeBases {
    pub fn new(manager: Arc<WebKnowledgeManager>, user_id: String) -> Self {
        Self { manager, user_id }
    }
}

impl ToolDyn for WebListKnowledgeBases {
    fn name(&self) -> String {
        "list_knowledge_bases".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_knowledge_bases".to_string(),
            description: "列出所有可用的知识库，包括名称、描述、文档数量、后端类型等信息。"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    fn call<'a>(
        &'a self,
        _args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let km = self
                .manager
                .get_manager(&self.user_id)
                .await
                .map_err(|e| string_err(e))?;

            let infos = km.list_kbs().await.map_err(|e| string_err(e))?;

            let display: Vec<_> = infos
                .into_iter()
                .map(|i| {
                    json!({
                        "name": i.name,
                        "description": i.description,
                        "backend": i.backend,
                        "embedding_model": i.embedding_model,
                        "document_count": i.document_count,
                        "chunk_count": i.chunk_count,
                    })
                })
                .collect();

            serde_json::to_string_pretty(&display).map_err(|e| string_err(e))
        })
    }
}

// ============================================================================
// WebAddToKnowledgeBase
// ============================================================================

/// Web 版 `add_to_knowledge_base` 工具。
pub struct WebAddToKnowledgeBase {
    manager: Arc<WebKnowledgeManager>,
    user_id: String,
}

impl WebAddToKnowledgeBase {
    pub fn new(manager: Arc<WebKnowledgeManager>, user_id: String) -> Self {
        Self { manager, user_id }
    }
}

impl ToolDyn for WebAddToKnowledgeBase {
    fn name(&self) -> String {
        "add_to_knowledge_base".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "add_to_knowledge_base".to_string(),
            description: "添加文本内容到知识库。适用于保存对话中的重要信息、AI生成的摘要、用户口述的知识点。"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": {
                        "type": "string",
                        "description": "目标知识库名称"
                    },
                    "title": {
                        "type": "string",
                        "description": "内容标题"
                    },
                    "content": {
                        "type": "string",
                        "description": "文本内容"
                    },
                    "source": {
                        "type": "string",
                        "description": "来源标识，例如 'ai-generated' / 'user-input' / 'chat-summary'"
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
            let parsed: AddToKbArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let source = parsed.source.unwrap_or_else(|| "manual".to_string());
            let km = self
                .manager
                .get_manager(&self.user_id)
                .await
                .map_err(|e| string_err(e))?;

            let doc = km
                .add_text_to_kb(&parsed.kb_name, &parsed.title, &parsed.content, &source)
                .await
                .map_err(|e| string_err(e))?;

            Ok(format!("已添加文档: {} (id: {})", doc.title, doc.id))
        })
    }
}

#[derive(Debug, Deserialize)]
struct AddToKbArgs {
    kb_name: String,
    title: String,
    content: String,
    source: Option<String>,
}

// ============================================================================
// WebSyncKnowledgeBase
// ============================================================================

/// Web 版 `sync_knowledge_base` 工具。
pub struct WebSyncKnowledgeBase {
    manager: Arc<WebKnowledgeManager>,
    user_id: String,
}

impl WebSyncKnowledgeBase {
    pub fn new(manager: Arc<WebKnowledgeManager>, user_id: String) -> Self {
        Self { manager, user_id }
    }
}

impl ToolDyn for WebSyncKnowledgeBase {
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
                    "kb_name": {
                        "type": "string",
                        "description": "知识库名称。不指定则同步所有知识库。"
                    }
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
            let parsed: SyncKbArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let km = self
                .manager
                .get_manager(&self.user_id)
                .await
                .map_err(|e| string_err(e))?;

            if let Some(name) = parsed.kb_name {
                let report = km.sync_kb(&name).await.map_err(|e| string_err(e))?;
                Ok(format!(
                    "知识库 '{}' 同步完成:\n- 新增: {} 个文件\n- 更新: {} 个文件\n- 删除: {} 个文件\n- 跳过: {} 个文件\n- 耗时: {}ms{}",
                    report.kb_name,
                    report.added,
                    report.updated,
                    report.removed,
                    report.skipped,
                    report.duration_ms,
                    if report.has_errors() {
                        format!("\n- 错误: {} 个", report.errors.len())
                    } else {
                        String::new()
                    }
                ))
            } else {
                let all_reports = km.sync_all().await.map_err(|e| string_err(e))?;
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

#[derive(Debug, Deserialize)]
struct SyncKbArgs {
    kb_name: Option<String>,
}

// ============================================================================
// WebGetKnowledgeBaseDocs
// ============================================================================

/// Web 版 `get_knowledge_base_docs` 工具。
pub struct WebGetKnowledgeBaseDocs {
    manager: Arc<WebKnowledgeManager>,
    user_id: String,
}

impl WebGetKnowledgeBaseDocs {
    pub fn new(manager: Arc<WebKnowledgeManager>, user_id: String) -> Self {
        Self { manager, user_id }
    }
}

impl ToolDyn for WebGetKnowledgeBaseDocs {
    fn name(&self) -> String {
        "get_knowledge_base_docs".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "get_knowledge_base_docs".to_string(),
            description: "查看指定知识库中的文档列表，包括文件名、文档ID、来源等信息。"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kb_name": {
                        "type": "string",
                        "description": "知识库名称"
                    }
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
            let parsed: GetDocsArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let km = self
                .manager
                .get_manager(&self.user_id)
                .await
                .map_err(|e| string_err(e))?;

            let docs = km
                .list_documents(&parsed.kb_name, 0, 100)
                .await
                .map_err(|e| string_err(e))?;

            let display: Vec<_> = docs
                .into_iter()
                .map(|d| {
                    json!({
                        "id": d.id,
                        "title": d.title,
                        "source": d.source_path,
                    })
                })
                .collect();

            serde_json::to_string_pretty(&display).map_err(|e| string_err(e))
        })
    }
}

#[derive(Debug, Deserialize)]
struct GetDocsArgs {
    kb_name: String,
}
