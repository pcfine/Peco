// ============================================================================
// MemoryExtractionHook — 记忆写路径（LooperHook）
// ============================================================================
//
// 每轮成功完成后，将本轮对话交给 Flash 模型提取长期记忆，写入
// @private_memory 知识库。与 compaction 的分工：compaction 解决
// "会话内上下文放不下"；本模块解决"跨会话/超长期的知识"。
//
// 非致命性：所有失败点 warn 后 return — `on_turn_complete` 无返回值，
// hook 永不影响对话主流程。
//
// 执行模型：守卫与收集在 looper 上下文内同步完成（O(1)，只看最后一轮），
// 检索、LLM 提取与 KB 写入全部 `tokio::spawn` 到后台 — turn 边界零阻塞。
// spawn 前数据全部转 owned，无借用问题；单用户场景写入乱序风险可接受。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use model_provider::{InputItem, Role, Usage};
use peco_core::agent::{LooperHook, TurnFailureReason};
use peco_core::knowledge::KnowledgeManager;
use peco_core::session::Session;
use tracing::{info, warn};

use super::analyzer::{MemoryFact, TurnAnalyzer};
use super::config::MemoryConfig;
use super::dedup::max_cosine;

/// 记忆提取写路径。
pub struct MemoryExtractionHook {
    km: Arc<KnowledgeManager>,
    analyzer: Arc<dyn TurnAnalyzer>,
    config: MemoryConfig,
}

impl MemoryExtractionHook {
    pub fn new(
        km: Arc<KnowledgeManager>,
        analyzer: Arc<dyn TurnAnalyzer>,
        config: MemoryConfig,
    ) -> Self {
        Self {
            km,
            analyzer,
            config,
        }
    }

    /// 从最后一轮 committed turn 收集对话转录（User/Assistant 文本，
    /// 跳过 tool 过程与 reasoning）。返回 `None` 表示无可提取内容。
    fn collect_turn_dialogue(session: &Session) -> Option<String> {
        let turn = session.committed_turns().last()?;
        let mut user_parts: Vec<Cow<'_, str>> = Vec::new();
        let mut assistant_parts: Vec<Cow<'_, str>> = Vec::new();
        for am in turn {
            if let InputItem::Message { role, content } = am.message.as_ref() {
                if content.text_view().trim().is_empty() {
                    continue;
                }
                match role {
                    Role::User => user_parts.push(content.text_view()),
                    Role::Assistant => assistant_parts.push(content.text_view()),
                    _ => {}
                }
            }
        }
        if user_parts.is_empty() {
            return None;
        }
        let mut dialogue = String::new();
        for p in user_parts {
            dialogue.push_str("User: ");
            dialogue.push_str(&p);
            dialogue.push('\n');
        }
        for p in assistant_parts {
            dialogue.push_str("Assistant: ");
            dialogue.push_str(&p);
            dialogue.push('\n');
        }
        Some(dialogue)
    }

    /// 写路径近重复判定：一次批量嵌入后，逐条求「同类目既有记忆」的
    /// 最大余弦，返回与 `facts` 等长的标记（`true` = 近重复）。
    ///
    /// 同类目 = fact 的 `ppa_{category}` 与既有 snippet 的 `source` 一致。
    /// 返回 `None` 表示嵌入基建不可用 —— 调用方降级放行全部，嵌入故障
    /// 不得阻塞写路径。
    async fn near_duplicate_flags(
        km: &KnowledgeManager,
        kb_name: &str,
        facts: &[MemoryFact],
        existing: &[(String, String)],
        dedup_cos: f32,
    ) -> Option<Vec<bool>> {
        let categories: HashSet<String> = facts.iter().map(source_of).collect();

        // 同类目既有片段：跨 fact 去重（同一片段可能被多条同类别 fact 引用），
        // 空白片段不参与嵌入
        let mut seen: HashSet<&str> = HashSet::new();
        let same_category: Vec<&(String, String)> = existing
            .iter()
            .filter(|(src, snippet)| {
                categories.contains(src)
                    && !snippet.trim().is_empty()
                    && seen.insert(snippet.as_str())
            })
            .collect();

        let mut texts: Vec<String> = facts.iter().map(|f| f.content.clone()).collect();
        texts.extend(same_category.iter().map(|(_, snippet)| snippet.clone()));

        let vectors = match km.embed_texts(kb_name, &texts).await {
            Ok(v) if v.len() == texts.len() => v,
            Ok(v) => {
                warn!(
                    expected = texts.len(),
                    got = v.len(),
                    "Embedding count mismatch, dedup check skipped"
                );
                return None;
            }
            Err(e) => {
                warn!(
                    error = %e,
                    kb = %kb_name,
                    "Embedding unavailable, dedup check skipped (writing as-is)"
                );
                return None;
            }
        };

        let (fact_vectors, existing_vectors) = vectors.split_at(facts.len());
        let mut by_category: HashMap<String, Vec<Vec<f32>>> = HashMap::new();
        for ((src, _), vector) in same_category.iter().zip(existing_vectors) {
            by_category
                .entry(src.clone())
                .or_default()
                .push(vector.clone());
        }

        // 无同类目既有记忆 → 空候选（max_cosine 恒 0.0，不判重）
        let no_candidates: Vec<Vec<f32>> = Vec::new();
        Some(
            facts
                .iter()
                .enumerate()
                .map(|(i, fact)| {
                    let Some(vector) = fact_vectors.get(i) else {
                        return false;
                    };
                    let others = by_category.get(&source_of(fact)).unwrap_or(&no_candidates);
                    max_cosine(vector, others) >= dedup_cos
                })
                .collect(),
        )
    }
}

/// fact 对应的 KB source 标签（与写入时一致）。
fn source_of(fact: &MemoryFact) -> String {
    format!("ppa_{}", fact.category.as_str())
}

#[async_trait]
impl LooperHook for MemoryExtractionHook {
    async fn on_turn_complete(
        &self,
        _turn_index: usize,
        failure: Option<&TurnFailureReason>,
        _usage: &Usage,
        session: &Session,
    ) {
        // 失败轮不提取 — 回滚/中断的对话不构成可靠记忆来源
        if failure.is_some() {
            return;
        }

        let Some(dialogue) = Self::collect_turn_dialogue(session) else {
            return;
        };
        if dialogue.chars().count() < self.config.analyze_min_chars {
            return;
        }

        let km = Arc::clone(&self.km);
        let analyzer = Arc::clone(&self.analyzer);
        let config = self.config.clone();

        tokio::spawn(async move {
            // 提取前检索既有相关记忆（进入 prompt 抑制重复提取）。
            // 检索失败不阻断 — 只是失去去重提示。
            let query = dialogue.chars().take(200).collect::<String>();
            // 保留 (source_path, snippet) 两份视图：snippet 序列进 analyzer
            // 抑制重复提取，source_path 供写前近重复判定按类目比对
            let existing: Vec<(String, String)> = match km
                .search_kb(&config.kb_name, &query, config.extraction_top_k)
                .await
            {
                Ok(results) => results
                    .into_iter()
                    .map(|r| (r.source_path, r.snippet))
                    .collect(),
                Err(e) => {
                    warn!(error = %e, kb = %config.kb_name, "Pre-extraction recall failed (proceeding without existing memories)");
                    Vec::new()
                }
            };
            let existing_snippets: Vec<String> = existing.iter().map(|(_, s)| s.clone()).collect();

            let analyzed = tokio::time::timeout(
                std::time::Duration::from_secs(config.analyzer_timeout_secs),
                analyzer.analyze(&dialogue, &existing_snippets),
            )
            .await;

            let facts = match analyzed {
                Ok(Ok(facts)) => facts,
                Ok(Err(e)) => {
                    warn!(error = %e, "Memory extraction failed (non-fatal)");
                    return;
                }
                Err(_) => {
                    warn!(
                        timeout_secs = config.analyzer_timeout_secs,
                        "Memory extraction timed out (non-fatal)"
                    );
                    return;
                }
            };
            if facts.is_empty() {
                return;
            }

            // 写前近重复判定（shadow 下只记日志）。嵌入不可用 → None → 全部放行
            let duplicates = Self::near_duplicate_flags(
                &km,
                &config.kb_name,
                &facts,
                &existing,
                config.consolidation.dedup_cos,
            )
            .await
            .unwrap_or_else(|| vec![false; facts.len()]);
            let enforce = config.consolidation.dedup_enforce;

            // KB 由 personal 模板幂等安装保证存在；缺失（NotFound）按非致命处理
            let base_ts = chrono::Utc::now().timestamp_millis();
            for (i, fact) in facts.iter().enumerate() {
                let source = source_of(fact);
                if duplicates.get(i).copied().unwrap_or(false) {
                    if enforce {
                        info!(
                            kb = %config.kb_name,
                            category = fact.category.as_str(),
                            content = %fact.content,
                            "Near-duplicate memory skipped (dedup enforce)"
                        );
                        continue;
                    }
                    warn!(
                        kb = %config.kb_name,
                        category = fact.category.as_str(),
                        content = %fact.content,
                        "dedup shadow: near-duplicate memory would be suppressed (written anyway)"
                    );
                }
                let title = format!("memory_{base_ts}_{i}");
                match km
                    .add_text_to_kb(&config.kb_name, &title, &fact.content, &source)
                    .await
                {
                    Ok(_) => {
                        info!(
                            kb = %config.kb_name,
                            category = fact.category.as_str(),
                            "Memory written"
                        );
                    }
                    Err(e) => {
                        warn!(error = %e, kb = %config.kb_name, "Memory write failed (non-fatal)");
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::analyzer::{MemoryCategory, MemoryFact};
    use super::*;
    use knowledge_base::{BackendType, ChunkingStrategySerde, FastembedModelTypeSerde, KbConfig};
    use peco_core::session::{MessageSource, Session};

    /// 可编程 mock 提取器 — 记录调用入参，返回预设结果。
    struct MockAnalyzer {
        result: std::sync::Mutex<Result<Vec<MemoryFact>, String>>,
        calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    }

    impl MockAnalyzer {
        fn ok(facts: Vec<MemoryFact>) -> Self {
            Self {
                result: std::sync::Mutex::new(Ok(facts)),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn err(msg: &str) -> Self {
            Self {
                result: std::sync::Mutex::new(Err(msg.to_string())),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl TurnAnalyzer for MockAnalyzer {
        async fn analyze(
            &self,
            turn_dialogue: &str,
            existing_memories: &[String],
        ) -> Result<Vec<MemoryFact>, String> {
            self.calls
                .lock()
                .unwrap()
                .push((turn_dialogue.to_string(), existing_memories.to_vec()));
            self.result.lock().unwrap().clone()
        }
    }

    fn make_test_kb_config(name: &str) -> KbConfig {
        KbConfig {
            name: name.to_string(),
            description: "测试记忆库".into(),
            embedding_model: FastembedModelTypeSerde::AllMiniLML6V2Q,
            chunking: ChunkingStrategySerde::FixedSize { size: 100 },
            backend: BackendType::InMemory,
            storage_path: None,
            default_storage_mode: Default::default(),
            helix_url: None,
        }
    }

    fn make_config() -> MemoryConfig {
        MemoryConfig {
            analyze_min_chars: 10,
            extraction_top_k: 3,
            ..MemoryConfig::default()
        }
    }

    /// 去重门控配置：`dedup_cos` 取 0.9（与既有 consolidation 测试同档，
    /// 标点变体近重复对实测 ≈0.999）。
    fn dedup_config(enforce: bool) -> MemoryConfig {
        MemoryConfig {
            analyze_min_chars: 10,
            extraction_top_k: 3,
            consolidation: super::super::config::ConsolidationConfig {
                dedup_enforce: enforce,
                dedup_cos: 0.9,
                ..Default::default()
            },
            ..MemoryConfig::default()
        }
    }

    /// 已有一条既有记忆的 KB（写路径去重的比对对象）。
    async fn make_km_with_existing(
        title: &str,
        content: &str,
        source: &str,
    ) -> Arc<KnowledgeManager> {
        let km = make_km().await;
        km.add_text_to_kb("@private_memory", title, content, source)
            .await
            .unwrap();
        km
    }

    /// 轮询等待后台任务被分析器调用（`on_turn_complete` 的 spawn 是异步的）。
    async fn wait_for_analyzer(analyzer: &MockAnalyzer) {
        for _ in 0..100 {
            if !analyzer.calls.lock().unwrap().is_empty() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("分析器应在超时前被调用");
    }

    async fn doc_count(km: &KnowledgeManager) -> usize {
        km.list_documents("@private_memory", 0, 100)
            .await
            .unwrap()
            .len()
    }

    /// 构造一个已提交一轮对话的 session（User + Assistant 文本）。
    fn make_session_with_turn(user: &str, assistant: &str) -> Session {
        let mut s = Session::new("test".to_string(), "test".to_string());
        s.start_turn(user.into()).unwrap();
        s.stage_item(
            MessageSource::ModelGeneration,
            InputItem::Message {
                role: Role::Assistant,
                content: assistant.into(),
            },
        )
        .unwrap();
        let _ = s.commit_turn().unwrap();
        s
    }

    async fn make_km() -> Arc<KnowledgeManager> {
        let tmp = tempfile::tempdir().unwrap();
        let km = Arc::new(KnowledgeManager::new(tmp.path().to_path_buf()));
        km.ensure_loaded().await.unwrap();
        km.create_kb(make_test_kb_config("@private_memory"))
            .await
            .unwrap();
        // tempdir 由调用方持有不能释放 — leak 掉测试目录（进程退出回收）
        std::mem::forget(tmp);
        km
    }

    #[tokio::test]
    async fn test_skips_failed_turn() {
        let analyzer = Arc::new(MockAnalyzer::ok(vec![]));
        let hook = MemoryExtractionHook::new(make_km().await, analyzer.clone(), make_config());
        let session = make_session_with_turn(
            "这是一个足够长的用户提问内容",
            "这是一个足够长的助手回答内容",
        );

        hook.on_turn_complete(
            0,
            Some(&TurnFailureReason::Cancelled),
            &Usage::default(),
            &session,
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            analyzer.calls.lock().unwrap().is_empty(),
            "失败轮不得触发提取"
        );
    }

    #[tokio::test]
    async fn test_skips_short_turns() {
        let analyzer = Arc::new(MockAnalyzer::ok(vec![]));
        let hook = MemoryExtractionHook::new(make_km().await, analyzer.clone(), make_config());
        // "用户: 你好\n" 共 8 字符 < analyze_min_chars(10)
        let session = make_session_with_turn("你好", "");

        hook.on_turn_complete(0, None, &Usage::default(), &session)
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            analyzer.calls.lock().unwrap().is_empty(),
            "短轮不得触发提取"
        );
    }

    #[tokio::test]
    async fn test_extracts_and_writes_to_kb() {
        let km = make_km().await;
        let facts = vec![MemoryFact {
            category: MemoryCategory::Profile,
            content: "用户偏好简洁的回答风格".to_string(),
        }];
        let analyzer = Arc::new(MockAnalyzer::ok(facts));
        let hook = MemoryExtractionHook::new(Arc::clone(&km), analyzer.clone(), make_config());
        let session = make_session_with_turn(
            "请记住：我偏好简洁的回答风格，以后所有回答都尽量精炼",
            "好的，我已记住你的偏好，之后会以简洁风格回答。",
        );

        hook.on_turn_complete(0, None, &Usage::default(), &session)
            .await;

        // spawn 的后台任务需要时间完成（含 embedding 索引），轮询等待
        for _ in 0..100 {
            let docs = km.list_documents("@private_memory", 0, 10).await.unwrap();
            if !docs.is_empty() {
                assert_eq!(docs[0].source_path, "ppa_profile");
                assert!(docs[0].title.starts_with("memory_"), "标题应带时间戳前缀");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("记忆应在超时前写入 KB");
    }

    #[tokio::test]
    async fn test_analyzer_error_is_non_fatal() {
        let km = make_km().await;
        let analyzer = Arc::new(MockAnalyzer::err("model exploded"));
        let hook = MemoryExtractionHook::new(Arc::clone(&km), analyzer.clone(), make_config());
        let session = make_session_with_turn(
            "这是一个足够长的用户提问内容",
            "这是一个足够长的助手回答内容",
        );

        // 不应 panic、不应上抛（on_turn_complete 无返回值即编译期保证）
        hook.on_turn_complete(0, None, &Usage::default(), &session)
            .await;

        // spawn 的后台任务需要时间，轮询等待提取器被调用（固定 sleep 会 flaky）
        for _ in 0..100 {
            if analyzer.calls.lock().unwrap().len() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let docs = km.list_documents("@private_memory", 0, 10).await.unwrap();
        assert!(docs.is_empty(), "提取失败时不得写入");
        assert_eq!(
            analyzer.calls.lock().unwrap().len(),
            1,
            "提取器应被调用一次"
        );
    }

    // ── 写路径去重（Stage 4 / 事项 5）────────────────────────────────────

    /// 既有记忆与本轮提取结果近重复（标点变体，余弦 ≈1.0）。
    const EXISTING: &str = "The user prefers concise answers when discussing Rust.";
    const NEAR_DUP: &str = "The user prefers concise answers when discussing Rust!";

    fn near_dup_facts() -> Vec<MemoryFact> {
        vec![MemoryFact {
            category: MemoryCategory::Profile,
            content: NEAR_DUP.to_string(),
        }]
    }

    fn dedup_session() -> Session {
        make_session_with_turn(
            "Please remember that I prefer concise answers when discussing Rust topics.",
            "Got it — I will keep answers about Rust concise from now on.",
        )
    }

    #[tokio::test]
    async fn dedup_enforce_skips_near_duplicate_write() {
        let km = make_km_with_existing("memory_1000_0", EXISTING, "ppa_profile").await;
        let analyzer = Arc::new(MockAnalyzer::ok(near_dup_facts()));
        let hook = MemoryExtractionHook::new(Arc::clone(&km), analyzer.clone(), dedup_config(true));

        hook.on_turn_complete(0, None, &Usage::default(), &dedup_session())
            .await;
        wait_for_analyzer(&analyzer).await;
        // 判定后无写入发生 —— 留足后台任务收尾时间再断言
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

        assert_eq!(
            doc_count(&km).await,
            1,
            "enforce 下近重复事实不得写入（仅存量那条）"
        );
    }

    #[tokio::test]
    async fn dedup_shadow_writes_near_duplicate() {
        let km = make_km_with_existing("memory_1000_0", EXISTING, "ppa_profile").await;
        let analyzer = Arc::new(MockAnalyzer::ok(near_dup_facts()));
        let hook =
            MemoryExtractionHook::new(Arc::clone(&km), analyzer.clone(), dedup_config(false));

        hook.on_turn_complete(0, None, &Usage::default(), &dedup_session())
            .await;

        // shadow：判定照算、日志照记，但写入照常发生 → 条数增至 2
        for _ in 0..100 {
            if doc_count(&km).await == 2 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("shadow 模式下近重复事实应照常写入");
    }

    #[tokio::test]
    async fn dedup_ignores_other_categories() {
        let km = make_km_with_existing("memory_1000_0", EXISTING, "ppa_profile").await;
        // 同样的文本但归为 episodic → 与既有 ppa_profile 不同类目，不判重
        let analyzer = Arc::new(MockAnalyzer::ok(vec![MemoryFact {
            category: MemoryCategory::Episodic,
            content: NEAR_DUP.to_string(),
        }]));
        let hook = MemoryExtractionHook::new(Arc::clone(&km), analyzer.clone(), dedup_config(true));

        hook.on_turn_complete(0, None, &Usage::default(), &dedup_session())
            .await;

        for _ in 0..100 {
            if doc_count(&km).await == 2 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("跨类目不判重，事实应照常写入");
    }

    #[tokio::test]
    async fn dedup_check_degrades_when_embedding_unavailable() {
        // KB 不存在 → embed_texts 失败 → 返回 None（调用方降级放行全部）
        let tmp = tempfile::tempdir().unwrap();
        let km = Arc::new(KnowledgeManager::new(tmp.path().to_path_buf()));
        km.ensure_loaded().await.unwrap();
        std::mem::forget(tmp);

        let flags = MemoryExtractionHook::near_duplicate_flags(
            &km,
            "@private_memory",
            &near_dup_facts(),
            &[("ppa_profile".to_string(), EXISTING.to_string())],
            0.9,
        )
        .await;
        assert!(flags.is_none(), "嵌入不可用应返回 None 供调用方降级放行");
    }

    #[tokio::test]
    async fn dedup_flags_only_same_category_near_duplicates() {
        let km = make_km().await;
        let facts = vec![
            MemoryFact {
                category: MemoryCategory::Profile,
                content: NEAR_DUP.to_string(),
            },
            MemoryFact {
                category: MemoryCategory::Semantic,
                content: "The user's primary programming language is Rust.".to_string(),
            },
        ];
        let existing = vec![
            ("ppa_profile".to_string(), EXISTING.to_string()),
            (
                "ppa_semantic".to_string(),
                "The user commutes by bicycle every day.".to_string(),
            ),
        ];

        let flags = MemoryExtractionHook::near_duplicate_flags(
            &km,
            "@private_memory",
            &facts,
            &existing,
            0.9,
        )
        .await
        .expect("KB 存在时嵌入可用");

        assert_eq!(flags, vec![true, false], "只对同类目近重复置位");
    }
}
