// ============================================================================
// ToolRegister — 工具组装器，基于依赖注入一次构建到位
//   ListTools  — 列出当前环境可注册的全部内置工具（name + description）
// ============================================================================

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use model_provider::ToolDefinition;
use serde_json::json;

use crate::tools::{
    AddFactsToKnowledgeBase, AddToKnowledgeBase, DefaultToolsExecutor, DelegateSubAgent,
    DeleteAgent, DeleteMcpServer, DeleteSkill, Fetch, GetKnowledgeBaseDocs, ListKnowledgeBases,
    ListMcpServers, ListSkills, QueryEntityFacts, ReadAgent, ReadSkill, RunParallelSubAgents,
    SaveAgent, SaveMcpServer, SaveSkill, SearchKnowledge, ShellTool, ShowWorkspace,
    SyncKnowledgeBase, TestMcpConnection, ToolDyn, ToolError, ToolExecutor, WebSearchTool,
};
use crate::workflow::persistence::NullWorkflowPersister;
use crate::workflow::tools::{DeleteWorkflow, ExecuteWorkflow, ListWorkflows, SaveWorkflow};

use super::deps::ToolDependencies;

/// 所有内置工具名（agent.md `tools:` 字段的合法取值）。
/// 与下方 `ToolRegister::build` 的 match arms 一一对应，由防漂移测试保障。
pub const BUILTIN_TOOL_NAMES: &[&str] = &[
    "shell",
    "fetch",
    "show_workspace",
    "list_tools",
    "read_skill",
    "list_skills",
    "save_skill",
    "delete_skill",
    "delegate_sub_agent",
    "run_parallel_sub_agents",
    "save_agent",
    "read_agent",
    "delete_agent",
    "execute_workflow",
    "list_workflows",
    "save_workflow",
    "delete_workflow",
    "list_mcp_servers",
    "save_mcp_server",
    "delete_mcp_server",
    "test_mcp_connection",
    "search_knowledge",
    "list_knowledge_bases",
    "add_to_knowledge_base",
    "sync_knowledge_base",
    "get_knowledge_base_docs",
    "add_facts_to_knowledge_base",
    "query_entity_facts",
    "web_search",
];

/// 可选依赖（workflow_access / mcp_access）缺失时跳过该工具 —
/// 与未知工具名一致采用 warn + skip，而非 panic。
fn skip_missing_dep(tool: &str, dep: &str) -> Option<Box<dyn ToolDyn>> {
    tracing::warn!(
        tool,
        dep,
        "Dependency not available in ToolDependencies, skipping tool"
    );
    None
}

/// 工具组装器 — 根据 tool_names 和依赖集合构建 ToolExecutor。
pub struct ToolRegister;

impl ToolRegister {
    /// 构建完全组装的 ToolExecutor。
    pub fn build(tool_names: &[String], deps: &ToolDependencies) -> Arc<dyn ToolExecutor> {
        let mut tools: Vec<Box<dyn ToolDyn>> = Vec::with_capacity(tool_names.len());

        for name in tool_names {
            let tool: Option<Box<dyn ToolDyn>> = match name.as_str() {
                // ── 零依赖工具 ──────────────────────────
                // ShellTool 包装 ShellExec，工作空间根目录作为默认 cwd
                // （workspace_root 为 None 时行为与旧 ShellExec 逐字节一致）
                "shell" => Some(Box::new(ShellTool::new(deps.workspace_root.clone()))),
                "fetch" => Some(Box::new(Fetch)),
                "list_tools" => Some(Box::new(ListTools { deps: deps.clone() })),

                // ── web_search（web_search 后端缺失时 warn + skip）──
                "web_search" => match deps.web_search.as_ref() {
                    Some(backend) => Some(Box::new(WebSearchTool::new(backend.clone()))),
                    None => skip_missing_dep("web_search", "web_search config"),
                },

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

                // ── Workflow 依赖（workflow_access 缺失时 warn + skip）──
                "execute_workflow" => match deps.workflow_access.as_ref() {
                    Some(wa) => {
                        let persister = deps
                            .workflow_persister
                            .clone()
                            .unwrap_or_else(|| Arc::new(NullWorkflowPersister));
                        Some(Box::new(ExecuteWorkflow::new(
                            wa.clone(),
                            deps.agent_access.clone(),
                            persister,
                        )))
                    }
                    None => skip_missing_dep("execute_workflow", "workflow_access"),
                },
                "list_workflows" => match deps.workflow_access.as_ref() {
                    Some(wa) => Some(Box::new(ListWorkflows::new(wa.clone()))),
                    None => skip_missing_dep("list_workflows", "workflow_access"),
                },
                "save_workflow" => match deps.workflow_access.as_ref() {
                    Some(wa) => Some(Box::new(SaveWorkflow::new(wa.clone()))),
                    None => skip_missing_dep("save_workflow", "workflow_access"),
                },
                "delete_workflow" => match deps.workflow_access.as_ref() {
                    Some(wa) => Some(Box::new(DeleteWorkflow::new(wa.clone()))),
                    None => skip_missing_dep("delete_workflow", "workflow_access"),
                },

                // ── MCP 依赖（mcp_access 缺失时 warn + skip）──────────
                "list_mcp_servers" => match deps.mcp_access.as_ref() {
                    Some(ma) => Some(Box::new(ListMcpServers::new(ma.clone()))),
                    None => skip_missing_dep("list_mcp_servers", "mcp_access"),
                },
                "save_mcp_server" => match deps.mcp_access.as_ref() {
                    Some(ma) => Some(Box::new(SaveMcpServer::new(ma.clone()))),
                    None => skip_missing_dep("save_mcp_server", "mcp_access"),
                },
                "delete_mcp_server" => match deps.mcp_access.as_ref() {
                    Some(ma) => Some(Box::new(DeleteMcpServer::new(ma.clone()))),
                    None => skip_missing_dep("delete_mcp_server", "mcp_access"),
                },
                "test_mcp_connection" => match deps.mcp_access.as_ref() {
                    Some(ma) => Some(Box::new(TestMcpConnection::new(ma.clone()))),
                    None => skip_missing_dep("test_mcp_connection", "mcp_access"),
                },

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

// ============================================================================
// ListTools — 列出当前环境可注册的全部内置工具（name + description）
// ============================================================================

pub struct ListTools {
    /// 持有完整依赖副本，call 时重建 executor 以收集各工具自身的 description。
    deps: ToolDependencies,
}

impl ToolDyn for ListTools {
    fn name(&self) -> String {
        "list_tools".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_tools".to_string(),
            description: "List all built-in tools available in this environment, with a short \
                description for each. Use this to discover valid tool names when creating or \
                updating an agent (the agent.md 'tools' field). Only built-in tools are listed; \
                MCP server tools are configured separately via the 'mcp' field \
                (see list_mcp_servers)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn call<'a>(
        &'a self,
        _args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            // 惰性构建：build 会再次构造 ListTools（仅存储 deps，无递归）。
            // 缺失可选依赖的工具被 warn + skip，因此输出即为当前环境
            // 真正可注册的工具集合。
            let names: Vec<String> = BUILTIN_TOOL_NAMES.iter().map(|s| s.to_string()).collect();
            let executor = ToolRegister::build(&names, &self.deps);
            let mut tools: Vec<serde_json::Value> = executor
                .definitions()
                .into_iter()
                .map(|d| json!({ "name": d.name, "description": d.description }))
                .collect();
            tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            serde_json::to_string_pretty(&json!({ "tools": tools })).map_err(ToolError::JsonError)
        })
    }
}

// ============================================================================
// 测试 — BUILTIN_TOOL_NAMES 与 match arms 的防漂移 + 按依赖过滤
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agent::{Agent, AgentError};
    use crate::config::McpServerConfig;
    use crate::knowledge::KnowledgeManager;
    use crate::search::{SearchBackend, searxng::SearxngClient};
    use crate::skills::SkillRegister;
    use crate::workflow::WorkflowAccess;

    // ── 最小 stub 实现（工具构造阶段不会被调用）─────────────────────────

    struct StubAgentAccess;
    impl crate::tools::deps::AgentAccess for StubAgentAccess {
        fn load_agent(&self, _name: &str) -> Result<Arc<Agent>, AgentError> {
            unimplemented!("not called during tool construction")
        }
        fn list_agent_names(&self) -> Vec<String> {
            vec![]
        }
        fn save_agent(&self, _name: &str, _content: &str) -> Result<(), String> {
            unimplemented!()
        }
        fn read_agent(&self, _name: &str) -> Result<String, String> {
            unimplemented!()
        }
        fn delete_agent(&self, _name: &str) -> Result<(), String> {
            unimplemented!()
        }
    }

    struct StubSkillProvider {
        registry: Arc<SkillRegister>,
    }
    impl crate::tools::deps::SkillProvider for StubSkillProvider {
        fn skill_registry(&self) -> &Arc<SkillRegister> {
            &self.registry
        }
        fn save_skill(&self, _name: &str, _content: &str) -> Result<(), String> {
            unimplemented!()
        }
        fn delete_skill(&self, _name: &str) -> Result<(), String> {
            unimplemented!()
        }
    }

    struct StubKnowledgeAccess {
        manager: Arc<KnowledgeManager>,
    }
    impl crate::tools::deps::KnowledgeAccess for StubKnowledgeAccess {
        fn user_id(&self) -> &str {
            "test-user"
        }
        fn knowledge_manager(&self) -> &Arc<KnowledgeManager> {
            &self.manager
        }
    }

    struct StubWorkflowAccess;
    impl WorkflowAccess for StubWorkflowAccess {
        fn load_workflow(
            &self,
            _name: &str,
        ) -> Result<crate::workflow::WorkflowDefinition, crate::workflow::WorkflowError> {
            unimplemented!()
        }
        fn list_workflow_names(&self) -> Vec<String> {
            vec![]
        }
        fn list_workflow_meta(&self) -> Vec<crate::workflow::WorkflowMeta> {
            vec![]
        }
        fn reload_workflow(
            &self,
            _name: &str,
        ) -> Result<crate::workflow::WorkflowDefinition, crate::workflow::WorkflowError> {
            unimplemented!()
        }
        fn save_workflow(&self, _name: &str, _content: &str) -> Result<(), String> {
            unimplemented!()
        }
        fn delete_workflow(&self, _name: &str) -> Result<(), String> {
            unimplemented!()
        }
    }

    struct StubMcpAccess;
    impl crate::tools::deps::McpAccess for StubMcpAccess {
        fn list_mcp_servers(&self) -> Vec<crate::tools::deps::McpServerInfo> {
            vec![]
        }
        fn add_mcp_server(&self, _name: &str, _config: McpServerConfig) -> Result<(), String> {
            unimplemented!()
        }
        fn remove_mcp_server(&self, _name: &str) -> Result<(), String> {
            unimplemented!()
        }
        fn get_mcp_server_config(&self, _name: &str) -> Option<McpServerConfig> {
            None
        }
    }

    // ── 夹具 ─────────────────────────────────────────────────────────────

    fn base_deps() -> ToolDependencies {
        let dir = std::env::temp_dir().join(format!("peco-tool-register-{}", std::process::id()));
        ToolDependencies {
            agent_access: Arc::new(StubAgentAccess),
            skill_provider: Arc::new(StubSkillProvider {
                registry: Arc::new(SkillRegister::new(dir.join("skills")).expect("skill register")),
            }),
            knowledge_access: Arc::new(StubKnowledgeAccess {
                manager: Arc::new(KnowledgeManager::new(dir.join("knowledge"))),
            }),
            allowed_kbs: vec![],
            workflow_access: None,
            mcp_access: None,
            workflow_persister: None,
            workspace_root: None,
            web_search: None,
        }
    }

    fn full_deps() -> ToolDependencies {
        let mut deps = base_deps();
        deps.workflow_access = Some(Arc::new(StubWorkflowAccess));
        deps.mcp_access = Some(Arc::new(StubMcpAccess));
        deps.web_search = Some(Arc::new(SearchBackend::Searxng(
            SearxngClient::new("http://localhost:8888").expect("client"),
        )));
        deps
    }

    fn definition_names(executor: &dyn ToolExecutor) -> Vec<String> {
        let mut names: Vec<String> = executor.definitions().into_iter().map(|d| d.name).collect();
        names.sort();
        names
    }

    // ── 测试 ─────────────────────────────────────────────────────────────

    /// 防漂移：BUILTIN_TOOL_NAMES 中每个名字都必须能在 build 中注册出恰好一个工具。
    #[test]
    fn every_builtin_tool_name_registers_exactly_one_tool() {
        let deps = full_deps();
        for name in BUILTIN_TOOL_NAMES {
            let executor = ToolRegister::build(&[name.to_string()], &deps);
            let names = definition_names(executor.as_ref());
            assert_eq!(
                names,
                vec![name.to_string()],
                "BUILTIN_TOOL_NAMES entry '{name}' does not match a build() match arm"
            );
        }
    }

    /// 按依赖过滤：缺 workflow_access / mcp_access / web_search 后端时，
    /// 对应工具不可注册。
    #[test]
    fn optional_dep_tools_skipped_when_deps_missing() {
        let deps = base_deps();
        let executor = ToolRegister::build(
            &BUILTIN_TOOL_NAMES
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            &deps,
        );
        let names = definition_names(executor.as_ref());
        for name in [
            "execute_workflow",
            "list_workflows",
            "save_workflow",
            "delete_workflow",
            "list_mcp_servers",
            "save_mcp_server",
            "delete_mcp_server",
            "test_mcp_connection",
            "web_search",
        ] {
            assert!(
                !names.contains(&name.to_string()),
                "tool '{name}' should be skipped without its optional dependency"
            );
        }
        assert!(names.contains(&"shell".to_string()));
        assert!(names.contains(&"list_tools".to_string()));
    }

    /// web_search 在后端就绪时正常注册（与 fetch 同级注册验证）。
    #[test]
    fn web_search_registers_with_backend() {
        let deps = full_deps();
        let executor = ToolRegister::build(&["web_search".to_string()], &deps);
        let names = definition_names(executor.as_ref());
        assert_eq!(names, vec!["web_search".to_string()]);
    }

    /// list_tools 输出与 build 全量注册结果一致（过滤后）。
    #[tokio::test]
    async fn list_tools_returns_available_tools() {
        let deps = full_deps();
        let executor = ToolRegister::build(&["list_tools".to_string()], &deps);
        let output = executor.execute("list_tools", "{}").await.expect("call");
        let parsed: serde_json::Value = serde_json::from_str(&output).expect("json");
        let listed: Vec<&str> = parsed["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().expect("name"))
            .collect();
        assert_eq!(listed.len(), BUILTIN_TOOL_NAMES.len());
        assert!(listed.contains(&"shell"));
        // 每一项都带有非空 description
        for tool in parsed["tools"].as_array().unwrap() {
            assert!(
                !tool["description"].as_str().unwrap_or_default().is_empty(),
                "tool '{}' missing description",
                tool["name"]
            );
        }
    }
}
