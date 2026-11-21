//! 记忆读路径：每次新用户 query 前召回本空间记忆并格式化注入。
//!
//! 隔离由「读的是哪个库」保证：调用方传进来的必须是本空间自己的记忆管理器
//! （`$PECO_HOME/memory/<ws_id>/`）。因此这里不再按文档标签过滤 —— 迁移过来的
//! 老记忆没有标签，过滤会把它们全部藏掉，而按库隔离已经覆盖了跨空间这一层。
//!
//! 已知限制（接受）：注入落在 instructions 尾部，内容随 query 变化会击穿
//! provider 前缀缓存；挪到 user 消息前缀需要改 core 的循环，不在本轮范围。

use std::sync::Arc;

use async_trait::async_trait;
use peco_core::agent::{DynamicContext, estimate_str_tokens};
use peco_core::knowledge::KnowledgeManager;
use tracing::warn;

use super::config::MemoryConfig;

/// 记忆召回读路径。
pub struct MemoryRecallContext {
    km: Arc<KnowledgeManager>,
    config: MemoryConfig,
    ws_id: String,
}

impl MemoryRecallContext {
    pub fn new(km: Arc<KnowledgeManager>, config: MemoryConfig, ws_id: impl Into<String>) -> Self {
        Self {
            km,
            config,
            ws_id: ws_id.into(),
        }
    }

    /// 本库里与 query 命中的前 `top_k` 条。
    async fn recall(&self, query: &str) -> Vec<knowledge_base::SearchResult> {
        match self
            .km
            .search_kb(&self.config.kb_name, query, self.config.top_k)
            .await
        {
            Ok(hits) => hits,
            // KB 缺失（还没建 / 被删）按无记忆处理，不影响对话
            Err(e) => {
                warn!(
                    ws_id = %self.ws_id,
                    kb = %self.config.kb_name,
                    error = %e,
                    "memory recall search failed; this turn runs without memory"
                );
                Vec::new()
            }
        }
    }
}

#[async_trait]
impl DynamicContext for MemoryRecallContext {
    async fn query(&self, query: &str) -> Option<String> {
        if !self.config.enabled {
            return None;
        }
        let hits = self.recall(query).await;
        format_memories(&hits, self.config.token_cap)
    }
}

/// 从 KB 文档的 source 标签（`ppa_{category}`）解析展示用类别名。
fn category_label(source_path: &str) -> &'static str {
    match source_path.strip_prefix("ppa_") {
        Some("profile") => "偏好",
        Some("semantic") => "事实",
        Some("episodic") => "事项",
        _ => "记忆",
    }
}

/// 格式化为注入文本；超出 token 上限时整行丢弃。
///
/// 整行而不是截断：半条记忆比没有记忆更容易误导模型。返回 `None` 表示没有
/// 可注入的内容。
fn format_memories(results: &[knowledge_base::SearchResult], token_cap: usize) -> Option<String> {
    const HEADER: &str = "关于用户的相关记忆:";
    if results.is_empty() {
        return None;
    }
    let mut lines: Vec<String> = Vec::with_capacity(results.len());
    let mut total = estimate_str_tokens(HEADER);
    for result in results {
        let snippet = result.snippet.trim();
        if snippet.is_empty() {
            continue;
        }
        let line = format!("- [{}] {}", category_label(&result.source_path), snippet);
        let cost = estimate_str_tokens(&line) + 1; // +1 换行
        if total + cost > token_cap {
            continue; // 后续更短的行仍可入选
        }
        total += cost;
        lines.push(line);
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!("{HEADER}\n{}", lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::WS_ID_TAG;
    use crate::memory::test_support::memory_kb;
    use knowledge_base::DocumentMetadata;

    fn config() -> MemoryConfig {
        MemoryConfig::default()
    }

    /// 写入一条带空间标签的记忆（升级前写路径的形态）。
    async fn seed(km: &KnowledgeManager, ws_id: &str, text: &str, source: &str) {
        let mut metadata = DocumentMetadata::default();
        metadata
            .extra
            .insert(WS_ID_TAG.to_string(), ws_id.to_string());
        km.add_text_to_kb_with_metadata("@private_memory", "memory", text, source, metadata)
            .await
            .unwrap();
    }

    /// 隔离由「读哪个库」决定，不由文档标签决定 ⇒ 本库里的一切都能召回，
    /// 包括迁移过来、没有标签的老记忆。按标签过滤会让这些记忆永远读不到。
    #[tokio::test]
    async fn recall_reads_everything_in_the_given_library() {
        let (km, _dir) = memory_kb().await;
        // 一条带标签（升级前写法），一条没有标签（迁移过来的形态）。
        seed(&km, "ws_a", "用户的主开发语言是 Rust", "ppa_semantic").await;
        km.add_text_to_kb(
            "@private_memory",
            "memory_legacy",
            "用户偏好简洁的回答",
            "ppa_profile",
        )
        .await
        .unwrap();

        let out = MemoryRecallContext::new(Arc::clone(&km), config(), "ws_a")
            .query("用户的主开发语言是什么")
            .await
            .expect("本库里有记忆就必须召回");
        assert!(out.contains("Rust"), "带标签的记忆要召回: {out}");

        let legacy = MemoryRecallContext::new(Arc::clone(&km), config(), "ws_a")
            .query("用户偏好什么样的回答")
            .await
            .expect("没有标签的老记忆同样在库里，也必须召回");
        assert!(legacy.contains("简洁"), "迁移来的记忆必须召回: {legacy}");
    }

    /// 空库不产出空壳文本（`Some("")` 会让 core 注入一段空标题）。
    #[tokio::test]
    async fn an_empty_library_yields_none() {
        let (km, _dir) = memory_kb().await;

        let out = MemoryRecallContext::new(Arc::clone(&km), config(), "ws_a")
            .query("用户的主开发语言是什么")
            .await;
        assert!(out.is_none(), "没有可注入的记忆时应返回 None: {out:?}");
    }

    /// 总开关关闭时读路径也不注入：关掉记忆的用户不该在 prompt 里看到旧记忆。
    #[tokio::test]
    async fn disabled_config_yields_none() {
        let (km, _dir) = memory_kb().await;
        seed(&km, "ws_a", "用户的主开发语言是 Rust", "ppa_semantic").await;

        let out = MemoryRecallContext::new(
            Arc::clone(&km),
            MemoryConfig {
                enabled: false,
                ..config()
            },
            "ws_a",
        )
        .query("用户的主开发语言是什么")
        .await;
        assert!(out.is_none(), "关闭记忆后不得注入: {out:?}");
    }

    #[tokio::test]
    async fn missing_kb_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let km = Arc::new(KnowledgeManager::new(dir.path().to_path_buf()));
        km.ensure_loaded().await.unwrap();

        let out = MemoryRecallContext::new(km, config(), "ws_a")
            .query("这是一个需要检索记忆的正常提问")
            .await;
        assert!(out.is_none(), "KB 缺失按无记忆处理，不得报错");
    }

    #[test]
    fn formatting_respects_the_token_cap() {
        let hit = |snippet: &str, source: &str| knowledge_base::SearchResult {
            document_id: "d".into(),
            title: "t".into(),
            snippet: snippet.into(),
            score: 1.0,
            source_path: source.into(),
            match_sources: vec![],
            confidence: knowledge_base::ConfidenceLevel::High,
            diagnostic: None,
        };
        let results = vec![
            hit("第一条记忆内容", "ppa_profile"),
            hit("第二条记忆内容", "ppa_semantic"),
            hit("第三条记忆内容", "ppa_episodic"),
        ];

        let all = format_memories(&results, 10_000).unwrap();
        assert_eq!(all.lines().count(), 4, "标题 + 3 行");
        assert!(all.contains("- [偏好] 第一条记忆内容"));
        assert!(all.contains("- [事实] 第二条记忆内容"));
        assert!(all.starts_with("关于用户的相关记忆:"));

        assert!(
            format_memories(&results, 1).is_none(),
            "一行都放不下时返回 None"
        );
        let partial = format_memories(&results, 20).unwrap();
        let lines = partial.lines().count();
        assert!((2..=3).contains(&lines), "cap=20 应留 1~2 行，实际 {lines}");
    }
}
