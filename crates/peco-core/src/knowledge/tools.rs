//! Agent 工具定义 — 暴露知识库操作给 LLM。
//!
//! 每个工具通过 `#[peco_tool]` 宏定义，生成的零大小结构体
//! 由 [`ToolFactory`](crate::tools::ToolFactory) 注册。

use crate::tools::ToolError;
use peco_derive::peco_tool;

fn string_err(e: impl std::fmt::Display) -> ToolError {
    ToolError::ToolCallError(Box::new(crate::tools::StringError(e.to_string())))
}

// ============================================================================
// search_knowledge — 搜索知识库
// ============================================================================

/// 在知识库中搜索信息。
///
/// 支持跨所有知识库或指定单个知识库的混合检索（语义 + 关键词 + 知识图谱）。
#[peco_tool(
    name = "search_knowledge",
    description = "在知识库中搜索信息。支持跨所有知识库或指定单个知识库的混合检索（语义+关键词+知识图谱）。",
    params(
        query = "搜索查询，支持自然语言描述。示例：'Rust 异步编程怎么做？'",
        kb_name = "指定知识库名称。不指定则搜索所有知识库。示例：'tech-docs'",
        top_k = "返回结果数量，默认 5"
    )
)]
pub async fn search_knowledge(
    query: String,
    kb_name: Option<String>,
    top_k: Option<usize>,
) -> Result<String, ToolError> {
    let km = crate::GlobalHandler::global().knowledge_manager();
    km.ensure_loaded().await.map_err(|e| string_err(e))?;

    let top_k = top_k.unwrap_or(5);

    let formatted = if let Some(name) = kb_name {
        let results = km
            .search_kb(&name, &query, top_k)
            .await
            .map_err(|e| string_err(e))?;
        results
            .iter()
            .map(|r| {
                serde_json::json!({
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
            .search_all(&query, top_k)
            .await
            .map_err(|e| string_err(e))?;
        all.into_iter()
            .flat_map(|(kb_name, hits)| {
                hits.into_iter().map(move |h| {
                    serde_json::json!({
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
}

// ============================================================================
// list_knowledge_bases — 列出知识库
// ============================================================================

/// 列出所有可用的知识库及其统计信息。
#[peco_tool(
    name = "list_knowledge_bases",
    description = "列出所有可用的知识库，包括名称、描述、文档数量、后端类型等信息。"
)]
pub async fn list_knowledge_bases() -> Result<String, ToolError> {
    let km = crate::GlobalHandler::global().knowledge_manager();
    km.ensure_loaded().await.map_err(|e| string_err(e))?;

    let infos = km.list_kbs().await.map_err(|e| string_err(e))?;

    let display: Vec<_> = infos
        .into_iter()
        .map(|i| {
            serde_json::json!({
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
}

// ============================================================================
// sync_knowledge_base — 同步知识库
// ============================================================================

/// 同步知识库：扫描原始文档目录，自动检测新文件、变更文件和已删除文件，更新向量数据库。
#[peco_tool(
    name = "sync_knowledge_base",
    description = "同步知识库：扫描原始文档目录，自动检测新文件、变更文件、已删除文件，增量更新向量数据库。",
    params(kb_name = "知识库名称。不指定则同步所有知识库。")
)]
pub async fn sync_knowledge_base(kb_name: Option<String>) -> Result<String, ToolError> {
    let km = crate::GlobalHandler::global().knowledge_manager();
    km.ensure_loaded().await.map_err(|e| string_err(e))?;

    if let Some(name) = kb_name {
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
}

// ============================================================================
// add_to_knowledge_base — 手动添加内容
// ============================================================================

/// 直接添加文本内容到知识库（不需要文件）。
#[peco_tool(
    name = "add_to_knowledge_base",
    description = "添加文本内容到知识库。适用于保存对话中的重要信息、AI生成的摘要、用户口述的知识点。",
    params(
        kb_name = "目标知识库名称",
        title = "内容标题",
        content = "文本内容",
        source = "来源标识，例如 'ai-generated' / 'user-input' / 'chat-summary'"
    )
)]
pub async fn add_to_knowledge_base(
    kb_name: String,
    title: String,
    content: String,
    source: Option<String>,
) -> Result<String, ToolError> {
    let km = crate::GlobalHandler::global().knowledge_manager();
    km.ensure_loaded().await.map_err(|e| string_err(e))?;

    let source = source.unwrap_or_else(|| "manual".to_string());
    let doc = km
        .add_text_to_kb(&kb_name, &title, &content, &source)
        .await
        .map_err(|e| string_err(e))?;

    Ok(format!("已添加文档: {} (id: {})", doc.title, doc.id))
}

// ============================================================================
// get_knowledge_base_docs — 查看文档列表
// ============================================================================

/// 查看知识库中的文档列表。
#[peco_tool(
    name = "get_knowledge_base_docs",
    description = "查看指定知识库中的文档列表，包括文件名、文档ID、来源等信息。",
    params(kb_name = "知识库名称")
)]
pub async fn get_knowledge_base_docs(kb_name: String) -> Result<String, ToolError> {
    let km = crate::GlobalHandler::global().knowledge_manager();
    km.ensure_loaded().await.map_err(|e| string_err(e))?;

    let docs = km
        .list_documents(&kb_name, 0, 100)
        .await
        .map_err(|e| string_err(e))?;

    let display: Vec<_> = docs
        .into_iter()
        .map(|d| {
            serde_json::json!({
                "id": d.id,
                "title": d.title,
                "source": d.source_path,
            })
        })
        .collect();

    serde_json::to_string_pretty(&display).map_err(|e| string_err(e))
}
