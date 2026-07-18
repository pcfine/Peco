// ============================================================================
// PpaDynamicContext — 读路径：记忆检索 + 上下文注入
// ============================================================================
//
// 实现 peco_core::agent::DynamicContext trait。
// 在每次用户 query 时被 AgentLooper 调用，检索相关记忆并格式化为字符串，
// 注入到 system prompt 中。

use std::sync::Arc;

use async_trait::async_trait;
use peco_core::agent::DynamicContext;
use tracing::warn;

use super::classifier::QueryClassifier;
use super::config::PpaConfig;
use super::store::PersonalMemoryStore;
use super::types::QueryType;

/// PPA 的 DynamicContext 实现。
///
/// 负责：查询分类 → Profile 加载 → Semantic/Episodic 检索 → 格式化输出。
pub struct PpaDynamicContext {
    store: Arc<PersonalMemoryStore>,
    classifier: QueryClassifier,
    config: PpaConfig,
}

impl PpaDynamicContext {
    /// 创建新的 PpaDynamicContext。
    pub fn new(
        store: Arc<PersonalMemoryStore>,
        classifier: QueryClassifier,
        config: PpaConfig,
    ) -> Self {
        Self {
            store,
            classifier,
            config,
        }
    }
}

#[async_trait]
impl DynamicContext for PpaDynamicContext {
    async fn query(&self, query: &str) -> Option<String> {
        // 仅在启用时执行
        if !self.config.enabled || !self.config.retrieval.auto_retrieve {
            return None;
        }

        // 1. 查询分类（规则引擎，零 LLM 成本）
        let query_type = self.classifier.classify(query);

        // 2. Profile 始终加载
        let profile = match self.store.get_profile().await {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to load profile, continuing without");
                Default::default()
            }
        };
        let mut parts: Vec<String> = Vec::new();

        let profile_text = profile.format_for_prompt();
        if !profile_text.is_empty() {
            parts.push(profile_text);
        }

        // 3. Semantic + Episodic 检索（非闲聊时）
        if query_type != QueryType::CasualChat {
            let cfg = &self.config.retrieval;

            let semantic = self
                .store
                .search_semantic(query, cfg.semantic_top_k, cfg.min_relevance_score)
                .await
                .unwrap_or_default();
            if !semantic.is_empty() {
                let lines: Vec<String> = semantic
                    .iter()
                    .map(|f| format!("- {} (相关度: {:.2})", f.content, 0.85))
                    .collect();
                parts.push(format!("[相关记忆]\n{}", lines.join("\n")));
            }

            // PersonalQuery 额外检索 Episodic
            if query_type == QueryType::PersonalQuery {
                let episodic = self
                    .store
                    .search_episodic(query, cfg.episodic_top_k, cfg.min_relevance_score)
                    .await
                    .unwrap_or_default();
                if !episodic.is_empty() {
                    let lines: Vec<String> = episodic
                        .iter()
                        .map(|f| format!("- {}", f.content))
                        .collect();
                    parts.push(format!("[历史上下文]\n{}", lines.join("\n")));
                }
            }
        }

        let context = parts.join("\n\n");
        if context.is_empty() {
            None
        } else {
            Some(format!("关于用户的相关记忆:\n\n{context}"))
        }
    }
}
