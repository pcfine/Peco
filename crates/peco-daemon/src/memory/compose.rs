//! 多个 `DynamicContext` 的组合：按顺序查询，跳过无内容的那几段。
//!
//! Peco 空间既要注入可派发的空间清单，又要注入记忆召回；core 只接受一个
//! `dynamic_context`，两段因此只能在 daemon 侧拼起来。顺序即优先级：越靠前
//! 的内容在 system prompt 里越靠前。

use std::sync::Arc;

use async_trait::async_trait;
use peco_core::agent::DynamicContext;

/// 分隔符：两段内容各自是 Markdown 片段，空一行才不会黏成一段。
const SEPARATOR: &str = "\n\n";

pub struct ComposedDynamicContext(Vec<Arc<dyn DynamicContext>>);

impl ComposedDynamicContext {
    pub fn new(contexts: Vec<Arc<dyn DynamicContext>>) -> Self {
        Self(contexts)
    }
}

#[async_trait]
impl DynamicContext for ComposedDynamicContext {
    async fn query(&self, query: &str) -> Option<String> {
        let mut parts: Vec<String> = Vec::with_capacity(self.0.len());
        // 逐段顺序等待：并发会让「谁先注入」变成不可控，而顺序是本结构体的
        // 全部语义。
        for context in &self.0 {
            let Some(text) = context.query(query).await else {
                continue;
            };
            // 空串要当作缺省：只留分隔符的注入块会把 system prompt 撑出空行，
            // 模型看到的是「有一段什么都没有」。
            if text.trim().is_empty() {
                continue;
            }
            parts.push(text);
        }
        if parts.is_empty() {
            return None;
        }
        Some(parts.join(SEPARATOR))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peco_core::agent::DynamicContext;

    /// 固定返回值的上下文，`None` 表示这一段无内容。
    struct Fixed(Option<&'static str>);

    #[async_trait]
    impl DynamicContext for Fixed {
        async fn query(&self, _query: &str) -> Option<String> {
            self.0.map(str::to_string)
        }
    }

    /// 记录被问过的 query，用来验证顺序与透传。
    struct Echo(&'static str);

    #[async_trait]
    impl DynamicContext for Echo {
        async fn query(&self, query: &str) -> Option<String> {
            Some(format!("{}-{query}", self.0))
        }
    }

    fn boxed(context: impl DynamicContext + 'static) -> Arc<dyn DynamicContext> {
        Arc::new(context)
    }

    /// 顺序由构造顺序决定，`None` 与空白段整段跳过，其余用空行连接。
    #[tokio::test]
    async fn keeps_order_and_skips_empty_parts() {
        let composed = ComposedDynamicContext::new(vec![
            boxed(Echo("first")),
            boxed(Fixed(None)),
            boxed(Fixed(Some("   "))),
            boxed(Echo("second")),
        ]);

        assert_eq!(
            composed.query("q").await.as_deref(),
            Some("first-q\n\nsecond-q")
        );
    }

    #[tokio::test]
    async fn all_empty_yields_none() {
        let composed =
            ComposedDynamicContext::new(vec![boxed(Fixed(None)), boxed(Fixed(Some("")))]);
        assert!(composed.query("q").await.is_none());
    }

    #[tokio::test]
    async fn single_context_passes_through() {
        let composed = ComposedDynamicContext::new(vec![boxed(Echo("only"))]);
        assert_eq!(composed.query("q").await.as_deref(), Some("only-q"));
    }
}
