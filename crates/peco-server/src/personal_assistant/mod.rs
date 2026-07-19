// ============================================================================
// PPA (Peco Personal Assistant) — 私人助理模块
// ============================================================================
//
// 模块结构:
//   config.rs    — PpaConfig 配置结构
//   types.rs     — MemoryFact, UserProfile, QueryType 等数据类型
//   store.rs     — PersonalMemoryStore (知识库 CRUD 封装)
//   classifier.rs    — QueryClassifier (规则引擎查询分类)
//   analyzer.rs      — MemoryAnalyzer (LLM 驱动记忆提取)
//   dynamic_context.rs — PpaDynamicContext (读路径: DynamicContext)
//   hook.rs           — PpaMemoryHook (写路径: LooperHook)
//   tools.rs          — remember / recall / forget 工具

pub mod analyzer;
pub mod classifier;
pub mod config;
pub mod dynamic_context;
pub mod hook;
pub mod store;
pub mod types;

use std::sync::Arc;

use crate::state::AppState;

// Re-exports
pub use config::PpaConfig;
pub use dynamic_context::PpaDynamicContext;
pub use hook::PpaMemoryHook;
pub use store::PersonalMemoryStore;

// ============================================================================
// PpaComponents — 聚合 PPA 的读/写路径组件
// ============================================================================

/// PPA 组件聚合，供外部（chat handler / assistant manager）注入 LooperConfig。
pub struct PpaComponents {
    pub dynamic_context: Option<Arc<dyn peco_core::agent::DynamicContext>>,
    pub hooks: Vec<Arc<dyn peco_core::agent::hooks::LooperHook>>,
}

/// 为指定用户构建 PPA 全量组件（PersonalMemoryStore → PpaDynamicContext + PpaMemoryHook）。
///
/// 仅在 `PpaConfig::enabled` 时生效；模型初始化失败降级为只读模式。
pub async fn build_ppa_components(state: &AppState, user_id: &str) -> PpaComponents {
    let ppa_config = PpaConfig::default();

    if !ppa_config.enabled {
        tracing::info!("PPA disabled");
        return PpaComponents {
            dynamic_context: None,
            hooks: Vec::new(),
        };
    }

    // 获取 per-user KnowledgeManager (via Workspace)
    let ws = match state.workspace_manager.get(user_id) {
        Ok(ws) => ws,
        Err(e) => {
            tracing::warn!(error = %e, user_id = %user_id, "Failed to get workspace, PPA disabled");
            return PpaComponents {
                dynamic_context: None,
                hooks: Vec::new(),
            };
        }
    };

    let user_km = ws.knowledge_manager().clone();
    let kb_name = format!("personal_memory_{}", user_id);
    let store = Arc::new(PersonalMemoryStore::new(
        user_km,
        kb_name,
        ppa_config.storage.clone(),
    ));

    // 创建 DynamicContext（读路径）
    let dynamic_context: Option<Arc<dyn peco_core::agent::DynamicContext>> = Some(Arc::new(
        PpaDynamicContext::new(
            store.clone(),
            classifier::QueryClassifier::new(),
            ppa_config.clone(),
        ),
    ));

    // 创建 MemoryAnalyzer（写路径）
    // 使用独立模型分析对话，失败不影响主流程
    let analyzer_model = match model_provider::DeepSeek::from_env() {
        Ok(provider) => Arc::new(provider),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to create PPA analyzer model provider, PPA write path disabled");
            return PpaComponents {
                dynamic_context,
                hooks: Vec::new(),
            };
        }
    };

    let analyzer = analyzer::MemoryAnalyzer::new(
        analyzer_model as Arc<dyn model_provider::ModelProvider>,
        ppa_config.analyzer.clone(),
    );

    let hook = Arc::new(PpaMemoryHook::new(store, analyzer, ppa_config));

    tracing::info!(user_id = %user_id, "PPA components built");

    PpaComponents {
        dynamic_context,
        hooks: vec![hook],
    }
}
