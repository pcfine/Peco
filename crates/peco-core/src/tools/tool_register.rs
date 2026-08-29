// ============================================================================
// ToolRegister — 工具组装器，基于依赖注入一次构建到位
// ============================================================================

use std::sync::Arc;

use crate::tools::{
    AddFactsToKnowledgeBase, AddToKnowledgeBase, DefaultToolsExecutor, DelegateSubAgent,
    DeleteAgent, DeleteMcpServer, DeleteSkill, Fetch, GetKnowledgeBaseDocs, ListKnowledgeBases,
    ListMcpServers, ListSkills, QueryEntityFacts, ReadAgent, ReadSkill, RunParallelSubAgents,
    SaveAgent, SaveMcpServer, SaveSkill, SearchKnowledge, ShellTool, ShowWorkspace,
    SyncKnowledgeBase, TestMcpConnection, ToolDyn, ToolExecutor,
};
use crate::workflow::persistence::NullWorkflowPersister;
use crate::workflow::tools::{DeleteWorkflow, ExecuteWorkflow, ListWorkflows, SaveWorkflow};

use super::deps::ToolDependencies;

/// 工具组装器 — 根据 tool_names 和依赖集合构建 ToolExecutor。
pub struct ToolRegister;

impl ToolRegister {
    /// 构建完全组装的 ToolExecutor。
    pub fn build(tool_names: &[String], deps: &ToolDependencies) -> Arc<dyn ToolExecutor> {
        let mut tools: Vec<Box<dyn ToolDyn>> = Vec::with_capacity(tool_names.len());

        for name in tool_names {
            let tool: Option<Box<dyn ToolDyn>> =
                match name.as_str() {
                    // ── 零依赖工具 ──────────────────────────
                    // ShellTool 包装 ShellExec，工作空间根目录作为默认 cwd
                    // （workspace_root 为 None 时行为与旧 ShellExec 逐字节一致）
                    "shell" => Some(Box::new(ShellTool::new(deps.workspace_root.clone()))),
                    "fetch" => Some(Box::new(Fetch)),

                    // ── Workspace 概览（聚合所有 trait）───
                    "show_workspace" => Some(Box::new(ShowWorkspace::new(
                        deps.agent_access.clone(),
                        deps.skill_provider.clone(),
                        deps.knowledge_access.clone(),
                        deps.workflow_access.clone(),
                        deps.mcp_access.clone(),
                        deps.workspace_root.clone(),
                    ))),

                    // ── Skill 依赖 ──────────────────────────
                    "read_skill" => Some(Box::new(ReadSkill::new(
                        deps.skill_provider.skill_registry().clone(),
                    ))),
                    "list_skills" => Some(Box::new(ListSkills::new(
                        deps.skill_provider.skill_registry().clone(),
                    ))),
                    "save_skill" => Some(Box::new(SaveSkill::new(deps.skill_provider.clone()))),
                    "delete_skill" => Some(Box::new(DeleteSkill::new(deps.skill_provider.clone()))),

                    // ── Agent 依赖 ──────────────────────────
                    "delegate_sub_agent" => {
                        Some(Box::new(DelegateSubAgent::new(deps.agent_access.clone())))
                    }
                    "run_parallel_sub_agents" => Some(Box::new(RunParallelSubAgents::new(
                        deps.agent_access.clone(),
                    ))),
                    "save_agent" => Some(Box::new(SaveAgent::new(deps.agent_access.clone()))),
                    "read_agent" => Some(Box::new(ReadAgent::new(deps.agent_access.clone()))),
                    "delete_agent" => Some(Box::new(DeleteAgent::new(deps.agent_access.clone()))),

                    // ── Workflow 依赖 ──────────────────────
                    "execute_workflow" => {
                        let wa = deps.workflow_access.clone().expect(
                            "execute_workflow tool requires workflow_access in ToolDependencies",
                        );
                        let persister = deps
                            .workflow_persister
                            .clone()
                            .unwrap_or_else(|| Arc::new(NullWorkflowPersister));
                        Some(Box::new(ExecuteWorkflow::new(
                            wa,
                            deps.agent_access.clone(),
                            persister,
                        )))
                    }
                    "list_workflows" => {
                        let wa = deps.workflow_access.clone().expect(
                            "list_workflows tool requires workflow_access in ToolDependencies",
                        );
                        Some(Box::new(ListWorkflows::new(wa)))
                    }
                    "save_workflow" => {
                        let wa = deps.workflow_access.clone().expect(
                            "save_workflow tool requires workflow_access in ToolDependencies",
                        );
                        Some(Box::new(SaveWorkflow::new(wa)))
                    }
                    "delete_workflow" => {
                        let wa = deps.workflow_access.clone().expect(
                            "delete_workflow tool requires workflow_access in ToolDependencies",
                        );
                        Some(Box::new(DeleteWorkflow::new(wa)))
                    }

                    // ── MCP 依赖 ────────────────────────────
                    "list_mcp_servers" => {
                        let ma = deps.mcp_access.clone().expect(
                            "list_mcp_servers tool requires mcp_access in ToolDependencies",
                        );
                        Some(Box::new(ListMcpServers::new(ma)))
                    }
                    "save_mcp_server" => {
                        let ma = deps
                            .mcp_access
                            .clone()
                            .expect("save_mcp_server tool requires mcp_access in ToolDependencies");
                        Some(Box::new(SaveMcpServer::new(ma)))
                    }
                    "delete_mcp_server" => {
                        let ma = deps.mcp_access.clone().expect(
                            "delete_mcp_server tool requires mcp_access in ToolDependencies",
                        );
                        Some(Box::new(DeleteMcpServer::new(ma)))
                    }
                    "test_mcp_connection" => {
                        let ma = deps.mcp_access.clone().expect(
                            "test_mcp_connection tool requires mcp_access in ToolDependencies",
                        );
                        Some(Box::new(TestMcpConnection::new(ma)))
                    }

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
