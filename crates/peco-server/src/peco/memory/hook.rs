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

use std::sync::Arc;

use async_trait::async_trait;
use model_provider::{InputItem, Role, Usage};
use peco_core::agent::{LooperHook, TurnFailureReason};
use peco_core::knowledge::KnowledgeManager;
use peco_core::session::Session;
use tracing::{info, warn};

use super::analyzer::TurnAnalyzer;
use super::config::MemoryConfig;

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
        let mut user_parts: Vec<&str> = Vec::new();
        let mut assistant_parts: Vec<&str> = Vec::new();
        for am in turn {
            if let InputItem::Message { role, content } = am.message.as_ref() {
                if content.trim().is_empty() {
                    continue;
                }
                match role {
                    Role::User => user_parts.push(content),
                    Role::Assistant => assistant_parts.push(content),
                    _ => {}
                }
            }
        }
        if user_parts.is_empty() {
            return None;
        }
        let mut dialogue = String::new();
        for p in user_parts {
            dialogue.push_str("用户: ");
            dialogue.push_str(p);
            dialogue.push('\n');
        }
        for p in assistant_parts {
            dialogue.push_str("助手: ");
            dialogue.push_str(p);
            dialogue.push('\n');
        }
        Some(dialogue)
    }
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
            let existing: Vec<String> = match km
                .search_kb(&config.kb_name, &query, config.extraction_top_k)
                .await
            {
                Ok(results) => results.into_iter().map(|r| r.snippet).collect(),
                Err(e) => {
                    warn!(error = %e, kb = %config.kb_name, "记忆提取前检索失败（继续无既有记忆的提取）");
                    Vec::new()
                }
            };

            let analyzed = tokio::time::timeout(
                std::time::Duration::from_secs(config.analyzer_timeout_secs),
                analyzer.analyze(&dialogue, &existing),
            )
            .await;

            let facts = match analyzed {
                Ok(Ok(facts)) => facts,
                Ok(Err(e)) => {
                    warn!(error = %e, "记忆提取失败（非致命）");
                    return;
                }
                Err(_) => {
                    warn!(
                        timeout_secs = config.analyzer_timeout_secs,
                        "记忆提取超时（非致命）"
                    );
                    return;
                }
            };
            if facts.is_empty() {
                return;
            }

            // KB 由 personal 模板幂等安装保证存在；缺失（NotFound）按非致命处理
            let base_ts = chrono::Utc::now().timestamp_millis();
            for (i, fact) in facts.iter().enumerate() {
                let title = format!("memory_{base_ts}_{i}");
                let source = format!("ppa_{}", fact.category.as_str());
                match km
                    .add_text_to_kb(&config.kb_name, &title, &fact.content, &source)
                    .await
                {
                    Ok(_) => {
                        info!(
                            kb = %config.kb_name,
                            category = fact.category.as_str(),
                            "已写入记忆"
                        );
                    }
                    Err(e) => {
                        warn!(error = %e, kb = %config.kb_name, "记忆写入失败（非致命）");
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
        }
    }

    fn make_config() -> MemoryConfig {
        MemoryConfig {
            analyze_min_chars: 10,
            extraction_top_k: 3,
            ..MemoryConfig::default()
        }
    }

    /// 构造一个已提交一轮对话的 session（User + Assistant 文本）。
    fn make_session_with_turn(user: &str, assistant: &str) -> Session {
        let mut s = Session::new("test".to_string(), "test".to_string());
        s.start_turn(user.to_string()).unwrap();
        s.stage_item(
            MessageSource::ModelGeneration,
            InputItem::Message {
                role: Role::Assistant,
                content: assistant.to_string(),
            },
        )
        .unwrap();
        let _ = s.commit_turn().unwrap();
        s
    }

    fn user_text(session: &Session) -> String {
        session
            .committed_turns()
            .last()
            .and_then(|t| t.first())
            .map(|am| match am.message.as_ref() {
                InputItem::Message { content, .. } => content.clone(),
                _ => String::new(),
            })
            .unwrap()
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
}
