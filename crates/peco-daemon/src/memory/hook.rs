//! 记忆写路径：成功轮结束后提取事实并写入团队 `@private_memory`。
//!
//! 执行模型与 `peco-server` 的同名 hook 一致：守卫与文本收集在 looper 上下文
//! 内同步完成（只看最后一轮，O(1)），检索、模型提取与写入全部 `tokio::spawn`
//! 到后台 —— `on_turn_complete` 卡住就等于用户的下一条消息排在它后面。
//!
//! 非致命纪律：所有失败点 `warn!` 后返回，不 panic、不向上抛 —— looper 里
//! panic 会终止整个对话任务。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use knowledge_base::DocumentMetadata;
use model_provider::{InputItem, Role, Usage};
use peco_core::agent::{LooperHook, TurnFailureReason};
use peco_core::knowledge::KnowledgeManager;
use peco_core::session::Session;
use tracing::{info, warn};

use super::WS_ID_TAG;
use super::analyzer::TurnAnalyzer;
use super::config::MemoryConfig;

/// 单次提取调用的上限。
///
/// 这条链路没有调用方可以取消它：provider 挂死会留下一个永久驻留的任务，
/// 把连接与内存一起钉住。到点放弃本轮，下一条消息照常触发新的提取。
const ANALYZE_TIMEOUT: Duration = Duration::from_secs(10);

/// 记忆提取写路径。
pub struct MemoryExtractionHook {
    km: Arc<KnowledgeManager>,
    analyzer: Arc<dyn TurnAnalyzer>,
    config: MemoryConfig,
    /// 本轮写入的记忆归属的空间。
    ws_id: String,
}

impl MemoryExtractionHook {
    pub fn new(
        km: Arc<KnowledgeManager>,
        analyzer: Arc<dyn TurnAnalyzer>,
        config: MemoryConfig,
        ws_id: impl Into<String>,
    ) -> Self {
        Self {
            km,
            analyzer,
            config,
            ws_id: ws_id.into(),
        }
    }

    /// 从最后一轮 committed turn 收集对话转录（User/Assistant 文本，跳过
    /// 工具过程与 reasoning）。返回 `None` 表示本轮没有可提取的内容。
    fn collect_turn_dialogue(session: &Session) -> Option<String> {
        let turn = session.committed_turns().last()?;
        let mut user_parts: Vec<String> = Vec::new();
        let mut assistant_parts: Vec<String> = Vec::new();
        for message in turn {
            let InputItem::Message { role, content } = message.message.as_ref() else {
                continue;
            };
            let text = content.text_view();
            if text.trim().is_empty() {
                continue;
            }
            match role {
                Role::User => user_parts.push(text.into_owned()),
                Role::Assistant => assistant_parts.push(text.into_owned()),
                _ => {}
            }
        }
        if user_parts.is_empty() {
            return None;
        }
        let mut dialogue = String::new();
        for part in user_parts {
            dialogue.push_str("User: ");
            dialogue.push_str(&part);
            dialogue.push('\n');
        }
        for part in assistant_parts {
            dialogue.push_str("Assistant: ");
            dialogue.push_str(&part);
            dialogue.push('\n');
        }
        Some(dialogue)
    }

    /// 提取 + 写入（`on_turn_complete` 的 spawn 体）。
    async fn write_facts(
        km: &KnowledgeManager,
        analyzer: &dyn TurnAnalyzer,
        config: &MemoryConfig,
        ws_id: &str,
        dialogue: String,
    ) {
        let facts = match tokio::time::timeout(ANALYZE_TIMEOUT, analyzer.analyze(&dialogue)).await {
            Ok(Ok(facts)) => facts,
            Ok(Err(e)) => {
                warn!(ws_id = %ws_id, error = %e, "memory extraction failed (non-fatal)");
                return;
            }
            Err(_) => {
                warn!(
                    ws_id = %ws_id,
                    timeout_secs = ANALYZE_TIMEOUT.as_secs(),
                    "memory extraction timed out (non-fatal)"
                );
                return;
            }
        };
        if facts.is_empty() {
            return;
        }

        // 标题只作人读定位：文档 id 由正文哈希决定，同一轮的多条 fact 靠序号
        // 区分，跨轮靠时间戳区分。
        let base_ts = chrono::Utc::now().timestamp_millis();
        for (index, fact) in facts.iter().enumerate() {
            let source = format!("ppa_{}", fact.category.as_str());
            let mut metadata = DocumentMetadata::default();
            metadata
                .extra
                .insert(WS_ID_TAG.to_string(), ws_id.to_string());
            let title = format!("memory_{base_ts}_{index}");
            match km
                .add_text_to_kb_with_metadata(
                    &config.kb_name,
                    &title,
                    &fact.text,
                    &source,
                    metadata,
                )
                .await
            {
                Ok(_) => info!(
                    ws_id = %ws_id,
                    kb = %config.kb_name,
                    category = fact.category.as_str(),
                    "memory written"
                ),
                Err(e) => warn!(
                    ws_id = %ws_id,
                    kb = %config.kb_name,
                    error = %e,
                    "memory write failed (non-fatal)"
                ),
            }
        }
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
        // 失败轮不提取：回滚/中断的对话不构成可靠记忆来源
        if failure.is_some() {
            return;
        }
        if !self.config.enabled {
            return;
        }
        let Some(dialogue) = Self::collect_turn_dialogue(session) else {
            return;
        };

        let km = Arc::clone(&self.km);
        let analyzer = Arc::clone(&self.analyzer);
        let config = self.config.clone();
        let ws_id = self.ws_id.clone();
        tokio::spawn(async move {
            Self::write_facts(&km, analyzer.as_ref(), &config, &ws_id, dialogue).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::memory::analyzer::{MemoryCategory, MemoryFact};
    use crate::memory::test_support::memory_kb;
    use peco_core::session::MessageSource;

    /// 假提取器：记录被调用的转录，返回预置事实。
    struct FakeAnalyzer {
        facts: Vec<MemoryFact>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeAnalyzer {
        fn returning(facts: Vec<MemoryFact>) -> Arc<Self> {
            Arc::new(Self {
                facts,
                calls: Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl TurnAnalyzer for FakeAnalyzer {
        async fn analyze(&self, turn_dialogue: &str) -> Result<Vec<MemoryFact>, String> {
            self.calls.lock().unwrap().push(turn_dialogue.to_string());
            Ok(self.facts.clone())
        }
    }

    fn session_with_turn(user: &str, assistant: &str) -> Session {
        let mut session = Session::new("test".into(), "test".into());
        session.start_turn(user.into()).unwrap();
        session
            .stage_item(
                MessageSource::ModelGeneration,
                InputItem::Message {
                    role: Role::Assistant,
                    content: assistant.into(),
                },
            )
            .unwrap();
        let _ = session.commit_turn().unwrap();
        session
    }

    /// 写入发生在后台任务里，轮询到 KB 中至少有 `expected` 条文档为止。
    ///
    /// 逐条写入，只看「非空」会在只写下第一条时提前返回。
    async fn wait_for_docs(
        km: &KnowledgeManager,
        expected: usize,
    ) -> Vec<knowledge_base::Document> {
        for _ in 0..100 {
            let docs = km
                .list_memory_documents_with_content("@private_memory", 100)
                .await
                .unwrap();
            if docs.len() >= expected {
                return docs;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("后台写入未在超时前写满 {expected} 条");
    }

    fn fact(text: &str, category: MemoryCategory) -> MemoryFact {
        MemoryFact {
            text: text.into(),
            category,
        }
    }

    async fn docs(km: &KnowledgeManager) -> Vec<knowledge_base::Document> {
        km.list_memory_documents_with_content("@private_memory", 100)
            .await
            .unwrap()
    }

    /// 失败轮绝不提取：回滚/中断的对话不是可靠的记忆来源。
    #[tokio::test]
    async fn failed_turn_writes_nothing() {
        let (km, _dir) = memory_kb().await;
        let analyzer =
            FakeAnalyzer::returning(vec![fact("不该被写下的记忆", MemoryCategory::Profile)]);
        let hook = MemoryExtractionHook::new(Arc::clone(&km), analyzer.clone(), config(), "ws_a");
        let session = session_with_turn("请记住我偏好简洁的回答", "好的，记住了。");

        hook.on_turn_complete(
            0,
            Some(&TurnFailureReason::Cancelled),
            &Usage::default(),
            &session,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(analyzer.calls(), 0, "失败轮不得调用提取器");
        assert!(docs(&km).await.is_empty(), "失败轮不得写入任何记忆");
    }

    /// 成功轮写入必须带 `ws_id`：读路径只认这个标签，漏了等于没写。
    #[tokio::test]
    async fn successful_turn_writes_tagged_memories() {
        let (km, _dir) = memory_kb().await;
        let analyzer = FakeAnalyzer::returning(vec![
            fact("用户偏好简洁的回答", MemoryCategory::Profile),
            fact("用户在用 Rust 写 daemon", MemoryCategory::Semantic),
        ]);
        let hook = MemoryExtractionHook::new(Arc::clone(&km), analyzer.clone(), config(), "ws_a");
        let session = session_with_turn("请记住我偏好简洁的回答，我在用 Rust", "好的。");

        hook.on_turn_complete(0, None, &Usage::default(), &session)
            .await;

        let written = wait_for_docs(&km, 2).await;
        assert_eq!(written.len(), 2, "两条 fact 各成一条文档");
        for doc in &written {
            assert_eq!(
                doc.metadata.extra.get(WS_ID_TAG).map(String::as_str),
                Some("ws_a"),
                "每条记忆都必须带自己的空间标签"
            );
            assert!(
                doc.source_path.starts_with("ppa_"),
                "source 走 ppa_ 约定，与既有列表过滤兼容"
            );
        }
        let sources: Vec<&str> = written.iter().map(|d| d.source_path.as_str()).collect();
        assert!(sources.contains(&"ppa_profile"));
        assert!(sources.contains(&"ppa_semantic"));
        assert_eq!(analyzer.calls(), 1, "一轮只提取一次");
    }

    /// 总开关关闭时不提取：这是「卸载记忆」的唯一开关。
    #[tokio::test]
    async fn disabled_config_writes_nothing() {
        let (km, _dir) = memory_kb().await;
        let analyzer =
            FakeAnalyzer::returning(vec![fact("不该被写下的记忆", MemoryCategory::Profile)]);
        let hook = MemoryExtractionHook::new(
            Arc::clone(&km),
            analyzer.clone(),
            MemoryConfig {
                enabled: false,
                ..config()
            },
            "ws_a",
        );
        let session = session_with_turn("请记住我偏好简洁的回答", "好的。");

        hook.on_turn_complete(0, None, &Usage::default(), &session)
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(analyzer.calls(), 0);
        assert!(docs(&km).await.is_empty());
    }

    fn config() -> MemoryConfig {
        MemoryConfig::default()
    }
}
