// ============================================================================
// MemoryRecallContext — 记忆读路径（DynamicContext）
// ============================================================================
//
// 每个新用户 query 时从 @private_memory 检索相关记忆，格式化为
// "关于用户的相关记忆" 块。由既有 DynamicContext 机制注入到
// instructions 尾部 [Dynamic Context] 段（同一轮 ReAct 迭代复用缓存）。
//
// 已知限制（接受）：注入在 instructions 尾部，内容随 query 变化，
// 会击穿 provider 前缀缓存 — 迁移到 user 消息前缀注入需改 peco-core
// 循环，见设计文档。

use std::sync::Arc;

use async_trait::async_trait;
use peco_core::agent::{DynamicContext, estimate_str_tokens};
use peco_core::knowledge::KnowledgeManager;
use sqlx::SqlitePool;
use tracing::warn;

use super::config::MemoryConfig;

/// 记忆召回读路径。
///
/// 检索命中后经 `tokio::spawn` 批量记账到 `memory_recall_stats`
/// （巩固流水线「无近期召回」判定的数据来源）；统计写入失败仅
/// warn，不影响召回主链路。`db` 为 `None` 时（CLI/测试）不记账。
pub struct MemoryRecallContext {
    km: Arc<KnowledgeManager>,
    config: MemoryConfig,
    user_id: String,
    db: Option<SqlitePool>,
}

impl MemoryRecallContext {
    pub fn new(
        km: Arc<KnowledgeManager>,
        config: MemoryConfig,
        user_id: impl Into<String>,
        db: Option<SqlitePool>,
    ) -> Self {
        Self {
            km,
            config,
            user_id: user_id.into(),
            db,
        }
    }

    /// 后台记账本轮命中的 doc_id 集合（零阻塞，失败仅 warn）。
    fn record_hits(&self, doc_ids: Vec<String>) {
        let Some(db) = self.db.clone() else {
            return;
        };
        let user_id = self.user_id.clone();
        let recalled_at = chrono::Utc::now().to_rfc3339();
        tokio::spawn(async move {
            if let Err(e) =
                crate::db::memory_recall_stats::record_recalls_batch(&db, &user_id, &doc_ids, &recalled_at)
                    .await
            {
                warn!(user_id = %user_id, error = %e, "Failed to record recall stats");
            }
        });
    }
}

/// 零成本闲聊门控：纯问候/感谢关键词规则，命中则不检索不调 LLM。
///
/// 不按 query 长度门控 — 短 query（"我之前说过什么"）恰恰最需要记忆召回。
fn is_casual(query: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "你好",
        "您好",
        "hi",
        "hello",
        "嗨",
        "谢谢",
        "感谢",
        "thanks",
        "thank you",
        "再见",
        "拜拜",
        "晚安",
        "早安",
        "好的",
        "ok",
        "嗯",
    ];
    let q = query.trim().to_lowercase();
    PATTERNS
        .iter()
        .any(|p| q == *p || (q.starts_with(p) && q.chars().count() <= p.chars().count() + 4))
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

/// 将检索结果格式化为注入文本（含 token 上限截断，整行为单位丢弃）。
///
/// 返回 `None` 表示无可用记忆。
fn format_memories(results: &[knowledge_base::SearchResult], token_cap: usize) -> Option<String> {
    if results.is_empty() {
        return None;
    }
    let mut lines: Vec<String> = Vec::with_capacity(results.len());
    let mut total = estimate_str_tokens("关于用户的相关记忆:");
    for r in results {
        let snippet = r.snippet.trim();
        if snippet.is_empty() {
            continue;
        }
        let line = format!("- [{}] {}", category_label(&r.source_path), snippet);
        let cost = estimate_str_tokens(&line) + 1; // +1 换行
        if total + cost > token_cap {
            continue; // 整行丢弃，后续更短的行仍可入选
        }
        total += cost;
        lines.push(line);
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!("关于用户的相关记忆:\n{}", lines.join("\n")))
}

#[async_trait]
impl DynamicContext for MemoryRecallContext {
    async fn query(&self, query: &str) -> Option<String> {
        if is_casual(query) {
            return None;
        }

        let results = match self
            .km
            .search_kb(&self.config.kb_name, query, self.config.recall_top_k)
            .await
        {
            Ok(r) => r,
            // KB 缺失（模板未装/被删）按无记忆处理，不影响对话
            Err(e) => {
                warn!(error = %e, kb = %self.config.kb_name, "Memory recall failed (proceeding without memory)");
                return None;
            }
        };

        // 命中记账（闲聊门控命中在上方提前返回，不写统计）
        if !results.is_empty() {
            let doc_ids: Vec<String> = results
                .iter()
                .map(|r| r.document_id.clone())
                .collect();
            self.record_hits(doc_ids);
        }

        format_memories(&results, self.config.injection_token_cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knowledge_base::{BackendType, ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig};

    fn make_test_kb_config(name: &str) -> KbConfig {
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

    async fn make_km_with_memories() -> Arc<KnowledgeManager> {
        let tmp = tempfile::tempdir().unwrap();
        let km = Arc::new(KnowledgeManager::new(tmp.path().to_path_buf()));
        km.ensure_loaded().await.unwrap();
        km.create_kb(make_test_kb_config("@private_memory"))
            .await
            .unwrap();
        km.add_text_to_kb(
            "@private_memory",
            "m1",
            "用户偏好简洁的回答风格",
            "ppa_profile",
        )
        .await
        .unwrap();
        km.add_text_to_kb(
            "@private_memory",
            "m2",
            "用户的主开发语言是 Rust",
            "ppa_semantic",
        )
        .await
        .unwrap();
        // tempdir 泄漏给进程（测试退出回收）
        std::mem::forget(tmp);
        km
    }

    #[test]
    fn test_casual_gate() {
        assert!(is_casual("你好"));
        assert!(is_casual("谢谢！"));
        assert!(is_casual("ok"));
        assert!(is_casual("hello!"));
        // 短 query 不按长度门控 — 记忆召回类问题必须放行
        assert!(!is_casual("我之前说过什么"));
        assert!(!is_casual("还记得吗"));
        assert!(!is_casual("帮我看看这个 Rust 编译错误是什么原因导致的"));
        assert!(!is_casual("我昨天说过的那个项目偏好还记得吗"));
    }

    #[test]
    fn test_category_label() {
        assert_eq!(category_label("ppa_profile"), "偏好");
        assert_eq!(category_label("ppa_semantic"), "事实");
        assert_eq!(category_label("ppa_episodic"), "事项");
        assert_eq!(category_label("uploaded/doc.md"), "记忆");
    }

    #[test]
    fn test_format_memories_truncates_by_token_cap() {
        let mk = |snippet: &str, source: &str| knowledge_base::SearchResult {
            document_id: "d".into(),
            title: "t".into(),
            snippet: snippet.to_string(),
            score: 1.0,
            source_path: source.to_string(),
            match_sources: vec![],
            confidence: knowledge_base::ConfidenceLevel::High,
            diagnostic: None,
        };
        let results = vec![
            mk("第一条记忆内容", "ppa_profile"),
            mk("第二条记忆内容", "ppa_semantic"),
            mk("第三条记忆内容", "ppa_episodic"),
        ];

        // 足够大的上限 — 全部保留
        let all = format_memories(&results, 10_000).unwrap();
        assert_eq!(all.lines().count(), 4); // 标题 + 3 行
        assert!(all.contains("- [偏好] 第一条记忆内容"));
        assert!(all.contains("- [事实] 第二条记忆内容"));

        // 极小上限 — 一行也放不下 → None
        assert!(format_memories(&results, 1).is_none());

        // 中等上限 — 整行丢弃，只留得下的行（标题约 6 token，单行约 8）
        let partial = format_memories(&results, 20).unwrap();
        let line_count = partial.lines().count();
        assert!(
            (2..=3).contains(&line_count),
            "cap=20 应留 1~2 行记忆，实际 {line_count}"
        );
        assert!(partial.starts_with("关于用户的相关记忆:"));
    }

    #[tokio::test]
    async fn test_casual_query_skips_search() {
        let km = make_km_with_memories().await;
        let ctx = MemoryRecallContext::new(km, MemoryConfig::default(), "test-user", None);
        assert!(ctx.query("你好").await.is_none(), "闲聊不得触发检索");
    }

    #[tokio::test]
    async fn test_recall_formats_memories() {
        let km = make_km_with_memories().await;
        let ctx = MemoryRecallContext::new(km, MemoryConfig::default(), "test-user", None);
        let out = ctx
            .query("用户希望以后回答用什么风格，还记得吗？")
            .await
            .expect("有记忆时应返回注入文本");
        assert!(out.starts_with("关于用户的相关记忆:"));
        assert!(out.contains("- ["));
    }

    #[tokio::test]
    async fn test_short_query_still_searches() {
        let km = make_km_with_memories().await;
        let ctx = MemoryRecallContext::new(km, MemoryConfig::default(), "test-user", None);
        let out = ctx.query("我的偏好是什么").await;
        assert!(out.is_some(), "短的记忆召回 query 不得被门控跳过");
    }

    #[tokio::test]
    async fn test_missing_kb_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let km = Arc::new(KnowledgeManager::new(tmp.path().to_path_buf()));
        km.ensure_loaded().await.unwrap();
        std::mem::forget(tmp);

        let ctx = MemoryRecallContext::new(km, MemoryConfig::default(), "test-user", None);
        assert!(
            ctx.query("这是一个需要检索记忆的正常提问").await.is_none(),
            "KB 缺失应按无记忆处理而非报错"
        );
    }

    // ── 召回记账（P4-T6 扩展测试，独立模块避免与既有 helper 纠缠）────
    #[tokio::test]
    async fn test_recall_records_stats_for_hits() {
        let km = make_km_with_memories().await;
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        std::mem::forget(dir);

        let ctx = MemoryRecallContext::new(
            km.clone(),
            MemoryConfig::default(),
            "stats-user",
            Some(pool.clone()),
        );
        ctx.query("用户的主开发语言是什么，还记得吗").await;

        // 记账经 tokio::spawn 后台写入 — 轮询等待落库
        let mut rows = Vec::new();
        for _ in 0..50 {
            rows = sqlx::query_as::<_, (String, i64)>(
                "SELECT doc_id, recall_count FROM memory_recall_stats WHERE user_id = 'stats-user'",
            )
            .fetch_all(&pool)
            .await
            .unwrap();
            if !rows.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!rows.is_empty(), "命中应记账");
        assert!(rows.len() <= 5, "记账条数不超过 recall_top_k");

        // 重复召回 → recall_count 累加
        ctx.query("用户的主开发语言是什么，还记得吗").await;
        for _ in 0..50 {
            let after = sqlx::query_as::<_, (String, i64)>(
                "SELECT doc_id, recall_count FROM memory_recall_stats WHERE user_id = 'stats-user'",
            )
            .fetch_all(&pool)
            .await
            .unwrap();
            if after.iter().any(|(_, c)| *c >= 2) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let counts: Vec<i64> = sqlx::query_as::<_, (String, i64)>(
            "SELECT doc_id, recall_count FROM memory_recall_stats WHERE user_id = 'stats-user'",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .map(|(_, c)| c)
        .collect();
        assert!(counts.iter().any(|c| *c >= 2), "重复召回应累加: {counts:?}");
    }

    #[tokio::test]
    async fn test_casual_query_records_nothing() {
        let km = make_km_with_memories().await;
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}/test.db?mode=rwc", dir.path().display());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        std::mem::forget(dir);

        let ctx = MemoryRecallContext::new(
            km,
            MemoryConfig::default(),
            "casual-user",
            Some(pool.clone()),
        );
        ctx.query("你好").await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM memory_recall_stats WHERE user_id = 'casual-user'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 0, "闲聊门控命中不得写统计");
    }
}
