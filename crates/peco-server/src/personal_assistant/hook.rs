// ============================================================================
// PpaMemoryHook — 写路径：对话分析 + 记忆保存
// ============================================================================
//
// 实现 peco_core::agent::LooperHook trait。
// 在 on_turn_complete 中自动分析本轮对话，提取并保存用户记忆。
//
// 获取对话内容的方式：
//   on_turn_complete 在 commit_turn() 之后调用，本轮消息已在最后一个
//   committed turn 中。通过 committed_turns().last() 直接取本轮数据，
//   无需扫描全部历史消息，也无需额外的共享缓冲区。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use model_provider::{InputItem, Role, Usage};
use peco_core::agent::TurnFailureReason;
use peco_core::agent::hooks::{HookAction, LooperHook, ToolHookAction};
use peco_core::session::Session;
use tracing::warn;

use super::analyzer::MemoryAnalyzer;
use super::config::PpaConfig;
use super::store::PersonalMemoryStore;
use super::types::{Importance, MemoryCategory, MemoryOperation, TurnContext};

/// PPA 的 LooperHook 实现。
///
/// 负责：从 Session 获取对话 → 阈值过滤 → LLM 分析 → 冲突检测 → 写入 KB。
pub struct PpaMemoryHook {
    store: Arc<PersonalMemoryStore>,
    analyzer: MemoryAnalyzer,
    config: PpaConfig,
}

impl PpaMemoryHook {
    /// 创建新的 PpaMemoryHook。
    pub fn new(
        store: Arc<PersonalMemoryStore>,
        analyzer: MemoryAnalyzer,
        config: PpaConfig,
    ) -> Self {
        Self {
            store,
            analyzer,
            config,
        }
    }

    /// 从 Session 中提取本轮对话的 User + Assistant 消息。
    ///
    /// `on_turn_complete` 在 `commit_turn()` 之后调用，本轮消息已在最后一个
    /// committed turn 中。直接通过 `committed_turns().last()` 取本轮数据，
    /// 避免 O(n) 扫描全部历史消息。
    fn collect_turn_messages(&self, session: &Session) -> Option<TurnContext> {
        let last_turn = session.committed_turns().last()?;

        let user_text = match last_turn.first()?.message.as_ref() {
            InputItem::Message {
                role: Role::User,
                content,
            } => content.clone(),
            _ => return None,
        };

        // 取 User 消息之后的所有 Assistant 消息（跳过 tool 消息）
        let assistant_responses: Vec<String> = last_turn
            .iter()
            .skip(1) // 跳过 User 自身
            .filter_map(|am| match am.message.as_ref() {
                InputItem::Message {
                    role: Role::Assistant,
                    content,
                } => Some(content.clone()),
                _ => None,
            })
            .collect();

        Some(TurnContext {
            user_query: user_text,
            assistant_responses,
        })
    }
}

#[async_trait]
impl LooperHook for PpaMemoryHook {
    async fn on_turn_complete(
        &self,
        turn_index: usize,
        failure: Option<&TurnFailureReason>,
        _usage: &Usage,
        session: &Session,
    ) {
        // 未启用 → 跳过
        if !self.config.enabled {
            return;
        }

        // 失败轮次不分析
        if failure.is_some() {
            return;
        }

        // 分析间隔控制
        let interval = self.config.analyzer.analyze_interval.max(1);
        if !turn_index.is_multiple_of(interval) {
            return;
        }

        // 从 Session 收集本轮对话
        let conversation = match self.collect_turn_messages(session) {
            Some(c) => c,
            None => return,
        };

        // 最小字符数过滤
        if conversation.total_chars() < self.config.analyzer.min_turn_chars {
            return;
        }

        // LLM 分析（异步，独立 Flash 模型，超时保护）
        let facts = match tokio::time::timeout(
            Duration::from_secs(self.config.analyzer.timeout_secs),
            self.analyzer.analyze(&conversation),
        )
        .await
        {
            Ok(Ok(facts)) => facts,
            Ok(Err(e)) => {
                warn!(error = %e, "Memory analysis failed");
                return;
            }
            Err(_) => {
                warn!("Memory analysis timed out");
                return;
            }
        };

        // 对每条 fact 执行冲突检测 + 写入
        for fact in &facts {
            let operation = self.store.detect_operation(fact).await;
            match operation {
                MemoryOperation::Add | MemoryOperation::Update => {
                    if let Err(e) = self.store.save_or_update_fact(fact).await {
                        warn!(error = %e, fact_id = %fact.id, "Failed to save memory fact");
                    }
                }
                MemoryOperation::Delete => {
                    if let Err(e) = self.store.invalidate_fact(fact).await {
                        warn!(error = %e, fact_id = %fact.id, "Failed to invalidate memory fact");
                    }
                }
                MemoryOperation::Noop => { /* skip */ }
            }
        }

        // 检查是否需要更新 Profile
        if facts.iter().any(|f| f.category == MemoryCategory::Profile) {
            let _ = self.store.sync_profile().await;
        }

        // 同时以图三元组形式保存语义/情景记忆（双写路径）
        let graph_facts: Vec<knowledge_base::Fact> = facts
            .iter()
            .filter(|f| {
                matches!(
                    f.category,
                    MemoryCategory::Semantic | MemoryCategory::Episodic
                )
            })
            .map(|mf| {
                let confidence = match mf.importance {
                    Importance::High => 0.95,
                    Importance::Medium => 0.8,
                    Importance::Low => 0.5,
                };
                knowledge_base::Fact::new("用户", mf.category_name(), &mf.content, confidence)
            })
            .collect();

        if !graph_facts.is_empty()
            && let Err(e) = self.store.save_facts_as_graph(&graph_facts).await
        {
            warn!(error = %e, "Failed to save facts to graph");
        }
    }

    // 其他 hook 方法使用默认空实现
    async fn on_before_request(
        &self,
        _turn_index: usize,
        _messages: &mut Vec<Arc<InputItem>>,
    ) -> HookAction {
        HookAction::Continue
    }

    async fn on_before_tool(
        &self,
        _turn_index: usize,
        _tool_call: &model_provider::ToolCall,
    ) -> ToolHookAction {
        ToolHookAction::Continue
    }
}
