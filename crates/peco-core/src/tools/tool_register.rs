// ============================================================================
// ToolRegister — 工具组装器，基于依赖注入一次构建到位
// ============================================================================

use std::sync::Arc;

use crate::tools::{
    AddFactsToKnowledgeBase, AddToKnowledgeBase, DefaultToolsExecutor, DelegateSubAgent, Fetch,
    GetKnowledgeBaseDocs, ListKnowledgeBases, QueryEntityFacts, ReadSkill, RunParallelSubAgents,
    SearchKnowledge, ShellExec, SyncKnowledgeBase, ToolDyn, ToolExecutor,
};

use super::deps::ToolDependencies;

/// 工具组装器 — 根据 tool_names 和依赖集合构建 ToolExecutor。
pub struct ToolRegister;

impl ToolRegister {
    /// 构建完全组装的 ToolExecutor。
    pub fn build(tool_names: &[String], deps: &ToolDependencies) -> Arc<dyn ToolExecutor> {
        let mut tools: Vec<Box<dyn ToolDyn>> = Vec::with_capacity(tool_names.len());

        for name in tool_names {
            let tool: Option<Box<dyn ToolDyn>> = match name.as_str() {
                // ── 零依赖工具 ──────────────────────────
                "shell" => Some(Box::new(ShellExec)),
                "fetch" => Some(Box::new(Fetch)),

                // ── Skill 依赖 ──────────────────────────
                "read_skill" => Some(Box::new(ReadSkill::new(
                    deps.skill_provider.skill_registry().clone(),
                ))),

                // ── Agent 加载依赖 ──────────────────────
                "delegate_sub_agent" => {
                    Some(Box::new(DelegateSubAgent::new(deps.agent_loader.clone())))
                }
                "run_parallel_sub_agents" => Some(Box::new(RunParallelSubAgents::new(
                    deps.agent_loader.clone(),
                ))),

                // ── Knowledge 依赖 ──────────────────────
                "search_knowledge" => Some(Box::new(SearchKnowledge::new(
                    deps.knowledge_access.clone(),
                    deps.allowed_kbs.clone(),
                ))),
                "list_knowledge_bases" => Some(Box::new(ListKnowledgeBases::new(
                    deps.knowledge_access.clone(),
                    deps.allowed_kbs.clone(),
                ))),
                "add_to_knowledge_base" => Some(Box::new(AddToKnowledgeBase::new(
                    deps.knowledge_access.clone(),
                    deps.allowed_kbs.clone(),
                ))),
                "sync_knowledge_base" => Some(Box::new(SyncKnowledgeBase::new(
                    deps.knowledge_access.clone(),
                    deps.allowed_kbs.clone(),
                ))),
                "get_knowledge_base_docs" => Some(Box::new(GetKnowledgeBaseDocs::new(
                    deps.knowledge_access.clone(),
                    deps.allowed_kbs.clone(),
                ))),
                "add_facts_to_knowledge_base" => Some(Box::new(AddFactsToKnowledgeBase::new(
                    deps.knowledge_access.clone(),
                    deps.allowed_kbs.clone(),
                ))),
                "query_entity_facts" => Some(Box::new(QueryEntityFacts::new(
                    deps.knowledge_access.clone(),
                    deps.allowed_kbs.clone(),
                ))),

                _ => {
                    tracing::warn!(tool = %name, "Unknown tool, skipping");
                    None
                }
            };

            if let Some(t) = tool {
                tools.push(t);
            }
        }

        Arc::new(DefaultToolsExecutor::new(tools))
    }
}
