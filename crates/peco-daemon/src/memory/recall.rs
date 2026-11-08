//! 记忆读路径：每次新用户 query 前召回本空间记忆并格式化注入。
//!
//! 隔离靠 metadata 过滤，而不是检索过滤：`SearchResult` 不含 metadata，候选
//! 一定是跨空间的，必须逐条 `get_document` 读回标签后才能判断归属。多出来的
//! 读放大是为了 fail-closed —— 读不到标签就丢弃，宁可少注入也不跨空间串记忆。
//!
//! 已知限制（接受）：注入落在 instructions 尾部，内容随 query 变化会击穿
//! provider 前缀缓存；挪到 user 消息前缀需要改 core 的循环，不在本轮范围。

use std::sync::Arc;

use async_trait::async_trait;
use peco_core::agent::{DynamicContext, estimate_str_tokens};
use peco_core::knowledge::KnowledgeManager;
use tracing::warn;

use super::WS_ID_TAG;
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

    /// 检索候选并只留下属于本空间的命中。
    ///
    /// 候选按 `top_k` 的两倍取：跨空间命中会占掉名额，取一倍等于让别的空间的
    /// 记忆把本空间挤出窗口。
    async fn recall(&self, query: &str) -> Vec<knowledge_base::SearchResult> {
        let hits = match self
            .km
            .search_kb(&self.config.kb_name, query, self.config.top_k * 2)
            .await
        {
            Ok(hits) => hits,
            // KB 缺失（模板没装 / 被删）按无记忆处理，不影响对话
            Err(e) => {
                warn!(
                    kb = %self.config.kb_name,
                    error = %e,
                    "memory recall search failed; this turn runs without memory"
                );
                return Vec::new();
            }
        };

        let mut kept = Vec::new();
        for hit in hits {
            if kept.len() >= self.config.top_k {
                break;
            }
            match self
                .km
                .get_document(&self.config.kb_name, &hit.document_id)
                .await
            {
                Ok(Some(doc)) => {
                    if doc.metadata.extra.get(WS_ID_TAG).map(String::as_str)
                        == Some(self.ws_id.as_str())
                    {
                        kept.push(hit);
                    }
                }
                // 标签读不回来就无法证明归属：一律丢弃（fail-closed）
                Ok(None) => warn!(
                    doc_id = %hit.document_id,
                    "recalled memory document vanished; dropping it"
                ),
                Err(e) => warn!(
                    doc_id = %hit.document_id,
                    error = %e,
                    "cannot read recalled memory metadata; dropping it (fail-closed)"
                ),
            }
        }
        kept
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
    use crate::memory::test_support::memory_kb;
    use knowledge_base::DocumentMetadata;

    fn config() -> MemoryConfig {
        MemoryConfig::default()
    }

    /// 写入一条带空间标签的记忆。
    async fn seed(km: &KnowledgeManager, ws_id: &str, text: &str, source: &str) {
        let mut metadata = DocumentMetadata::default();
        metadata
            .extra
            .insert(WS_ID_TAG.to_string(), ws_id.to_string());
        km.add_text_to_kb_with_metadata("@private_memory", "memory", text, source, metadata)
            .await
            .unwrap();
    }

    /// 写入一条**没有**空间标签的旧记忆。
    async fn seed_legacy(km: &KnowledgeManager, text: &str, source: &str) {
        km.add_text_to_kb("@private_memory", "memory", text, source)
            .await
            .unwrap();
    }

    /// 跨空间隔离，双向各验一次：两边的记忆内容互相可见就等于隔离没做。
    #[tokio::test]
    async fn recall_stays_inside_its_own_space_in_both_directions() {
        let (km, _dir) = memory_kb().await;
        // 两条记忆对同一个 query 同样相关：检索会把它们一起交出来，
        // 隔离只能由 ws_id 过滤完成（否则本用例的变异自证会假绿）。
        seed(&km, "ws_a", "用户的主开发语言是 Rust", "ppa_semantic").await;
        seed(&km, "ws_b", "用户的主开发语言是 Python", "ppa_semantic").await;

        let from_a = MemoryRecallContext::new(Arc::clone(&km), config(), "ws_a")
            .query("用户的主开发语言是什么")
            .await
            .expect("ws_a 有自己的记忆，必须召回");
        assert!(from_a.contains("Rust"), "应召回本空间记忆: {from_a}");
        assert!(!from_a.contains("Python"), "不得召回 ws_b 的记忆: {from_a}");

        let from_b = MemoryRecallContext::new(Arc::clone(&km), config(), "ws_b")
            .query("用户的主开发语言是什么")
            .await
            .expect("ws_b 有自己的记忆，必须召回");
        assert!(from_b.contains("Python"), "应召回本空间记忆: {from_b}");
        assert!(!from_b.contains("Rust"), "不得召回 ws_a 的记忆: {from_b}");
    }

    /// 没有空间标签的旧记忆永远不会被召回：无法证明归属就不能注入。
    #[tokio::test]
    async fn legacy_memory_without_ws_id_is_never_recalled() {
        let (km, _dir) = memory_kb().await;
        seed_legacy(&km, "用户的主开发语言是 Rust 和编译优化", "ppa_semantic").await;

        let out = MemoryRecallContext::new(Arc::clone(&km), config(), "ws_a")
            .query("用户的主开发语言是什么")
            .await;
        assert!(out.is_none(), "旧记忆不得进入召回: {out:?}");
    }

    /// 别的空间的记忆占满候选窗口时，本空间仍要召回 —— 候选按两倍取的理由。
    #[tokio::test]
    async fn foreign_hits_do_not_crowd_out_own_memory() {
        let (km, _dir) = memory_kb().await;
        for i in 0..6 {
            seed(
                &km,
                "ws_b",
                &format!("用户的开发语言是 Rust 项目 {i}"),
                "ppa_semantic",
            )
            .await;
        }
        seed(
            &km,
            "ws_a",
            "用户的开发语言是 Rust 和编译优化",
            "ppa_semantic",
        )
        .await;

        let out = MemoryRecallContext::new(Arc::clone(&km), config(), "ws_a")
            .query("用户的开发语言是什么")
            .await
            .expect("本空间的记忆必须召回");
        assert!(out.contains("编译优化"), "召回内容应来自 ws_a: {out}");
    }

    /// 空结果不产出空壳文本（`Some("")` 会让 core 注入一段空标题）。
    #[tokio::test]
    async fn empty_space_yields_none() {
        let (km, _dir) = memory_kb().await;
        seed(&km, "ws_b", "用户的主开发语言是 Python", "ppa_semantic").await;

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
