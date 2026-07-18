// ============================================================================
// DynamicContext — 根据用户 query 动态生成上下文信息
// ============================================================================
//
// 在每轮对话的 PreparingRequest 阶段，若检测到新用户 query，
// 则调用 DynamicContext::query() 获取动态上下文，注入到 system prompt 中。
// 同一 turn 内的后续 ReAct 迭代复用已缓存的上下文。

use async_trait::async_trait;

/// 动态上下文：根据用户 query 生成额外上下文信息，注入到 system prompt 中。
///
/// 典型场景：RAG 检索 — 用户提问时从向量数据库检索相关文档片段。
///
/// # 示例
///
/// ```ignore
/// use std::sync::Arc;
/// use peco_core::agent::DynamicContext;
///
/// struct MyRetriever { db: Arc<VectorDb> }
///
/// #[async_trait::async_trait]
/// impl DynamicContext for MyRetriever {
///     async fn query(&self, query: &str) -> Option<String> {
///         let docs = self.db.search(query, 5).await;
///         if docs.is_empty() {
///             None
///         } else {
///             Some(docs.join("\n\n"))
///         }
///     }
/// }
/// ```
#[async_trait]
pub trait DynamicContext: Send + Sync {
    /// 根据用户 query 返回动态上下文字符串。
    ///
    /// 返回 `None` 表示无额外上下文可提供（例如检索结果为空），
    /// 此时 system prompt 保持原样。
    async fn query(&self, query: &str) -> Option<String>;
}
