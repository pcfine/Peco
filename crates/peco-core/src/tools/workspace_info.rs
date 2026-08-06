// ============================================================================
// ShowWorkspace — 统一工作空间概览
// ============================================================================

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde_json::json;

use super::deps::{AgentAccess, KnowledgeAccess, McpAccess, SkillProvider};
use super::{StringError, ToolDyn, ToolError};
use crate::workflow::WorkflowAccess;

pub struct ShowWorkspace {
    agent_access: Arc<dyn AgentAccess>,
    skill_provider: Arc<dyn SkillProvider>,
    knowledge_access: Arc<dyn KnowledgeAccess>,
    workflow_access: Option<Arc<dyn WorkflowAccess>>,
    mcp_access: Option<Arc<dyn McpAccess>>,
}

impl ShowWorkspace {
    pub fn new(
        agent_access: Arc<dyn AgentAccess>,
        skill_provider: Arc<dyn SkillProvider>,
        knowledge_access: Arc<dyn KnowledgeAccess>,
        workflow_access: Option<Arc<dyn WorkflowAccess>>,
        mcp_access: Option<Arc<dyn McpAccess>>,
    ) -> Self {
        Self {
            agent_access,
            skill_provider,
            knowledge_access,
            workflow_access,
            mcp_access,
        }
    }
}

impl ToolDyn for ShowWorkspace {
    fn name(&self) -> String {
        "show_workspace".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "show_workspace".to_string(),
            description: "Show a unified overview of the entire workspace — all agents, skills, \
                workflows, MCP servers, and knowledge bases in one call. \
                Use this as the first step when exploring or assessing the workspace state. \
                Returns counts and summaries, not full bodies."
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
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let _ = args;

            let mut output = json!({
                "workspace": {}
            });

            // ── Agents ──────────────────────────────────────────────
            let agent_names = self.agent_access.list_agent_names();
            let mut agents = Vec::new();
            for name in &agent_names {
                // Try to read the agent's description from its frontmatter
                match self.agent_access.read_agent(name) {
                    Ok(content) => {
                        let desc = extract_description(&content);
                        agents.push(json!({
                            "name": name,
                            "description": desc,
                        }));
                    }
                    Err(_) => {
                        agents.push(json!({
                            "name": name,
                            "description": "(unable to read)",
                        }));
                    }
                }
            }
            output["workspace"]["agents"] = json!(agents);
            output["workspace"]["agent_count"] = json!(agent_names.len());

            // ── Skills ──────────────────────────────────────────────
            let skill_metas = self.skill_provider.skill_registry().all_meta();
            let skills: Vec<_> = skill_metas
                .iter()
                .map(|m| {
                    json!({
                        "name": m.name,
                        "description": m.description,
                    })
                })
                .collect();
            output["workspace"]["skills"] = json!(skills);
            output["workspace"]["skill_count"] = json!(skills.len());

            // ── Workflows ───────────────────────────────────────────
            if let Some(ref wa) = self.workflow_access {
                let metas = wa.list_workflow_meta();
                let workflows: Vec<_> = metas
                    .iter()
                    .map(|m| {
                        json!({
                            "name": m.name,
                            "description": m.description,
                            "version": m.version,
                            "step_count": m.step_count,
                        })
                    })
                    .collect();
                output["workspace"]["workflows"] = json!(workflows);
                output["workspace"]["workflow_count"] = json!(workflows.len());
            } else {
                output["workspace"]["workflows"] = json!([]);
                output["workspace"]["workflow_count"] = json!(0);
            }

            // ── MCP Servers ─────────────────────────────────────────
            if let Some(ref ma) = self.mcp_access {
                let servers = ma.list_mcp_servers();
                let mcp_servers: Vec<_> = servers
                    .iter()
                    .map(|s| {
                        let transport = match s.transport {
                            crate::config::TransportType::Stdio => "stdio",
                            crate::config::TransportType::Sse => "sse",
                            crate::config::TransportType::StreamableHttp => "streamable_http",
                        };
                        json!({
                            "name": s.name,
                            "transport": transport,
                            "enabled": s.enabled,
                            "url": s.url,
                            "command": s.command,
                        })
                    })
                    .collect();
                output["workspace"]["mcp_servers"] = json!(mcp_servers);
                output["workspace"]["mcp_server_count"] = json!(mcp_servers.len());
            } else {
                output["workspace"]["mcp_servers"] = json!([]);
                output["workspace"]["mcp_server_count"] = json!(0);
            }

            // ── Knowledge Bases ─────────────────────────────────────
            match self.knowledge_access.knowledge_manager().list_kbs().await {
                Ok(kbs) => {
                    let kb_infos: Vec<_> = kbs
                        .iter()
                        .map(|kb| {
                            json!({
                                "name": kb.name,
                                "doc_count": kb.document_count,
                            })
                        })
                        .collect();
                    output["workspace"]["knowledge_bases"] = json!(kb_infos);
                    output["workspace"]["kb_count"] = json!(kb_infos.len());
                }
                Err(e) => {
                    output["workspace"]["knowledge_bases"] = json!([]);
                    output["workspace"]["kb_count"] = json!(0);
                    output["workspace"]["kb_error"] = json!(format!("Failed to list KBs: {e}"));
                }
            }

            serde_json::to_string_pretty(&output)
                .map_err(|e| ToolError::ToolCallError(Box::new(StringError(e.to_string()))))
        })
    }
}

/// 从 agent.md 内容中提取 description 字段。
fn extract_description(content: &str) -> String {
    // 简单解析 YAML frontmatter 中的 description 行
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("description:") {
            let desc = value.trim().trim_matches('"').trim_matches('\'');
            if !desc.is_empty() {
                return desc.to_string();
            }
        }
    }
    "(no description)".to_string()
}
