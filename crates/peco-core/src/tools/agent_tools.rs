// ============================================================================
// Agent tools — Agent CRUD（依赖注入版）
//   ReadAgent   — 读取 agent.md 原始内容
//   SaveAgent   — 创建或更新 Agent
//   DeleteAgent — 删除 Agent（不可逆，需显式确认）
// ============================================================================

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use super::deps::AgentAccess;
use super::{Content, StringError, ToolDyn, ToolError};
use crate::agent::{AgentProfile, parse_agent_md};

// ── ReadAgent ────────────────────────────────────────────────────────────────

pub struct ReadAgent {
    agent_access: Arc<dyn AgentAccess>,
}

impl ReadAgent {
    pub fn new(agent_access: Arc<dyn AgentAccess>) -> Self {
        Self { agent_access }
    }
}

impl ToolDyn for ReadAgent {
    fn name(&self) -> String {
        "read_agent".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_agent".to_string(),
            description: "Read the full agent.md content of a specified agent. \
                Use this before modifying an existing agent — always read first, \
                then modify, then save_agent with the complete updated content."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The agent name to read (e.g., 'coding-assistant')."
                    }
                },
                "required": ["name"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct ReadAgentArgs {
                name: String,
            }

            let parsed: ReadAgentArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "agent name is required and cannot be empty".into(),
                ))));
            }

            self.agent_access
                .read_agent(name)
                .map_err(|e| {
                    ToolError::ToolCallError(Box::new(StringError(format!(
                        "failed to read agent '{name}': {e}"
                    ))))
                })
                .map(Content::Text)
        })
    }
}

// ── SaveAgent ────────────────────────────────────────────────────────────────

pub struct SaveAgent {
    agent_access: Arc<dyn AgentAccess>,
}

impl SaveAgent {
    pub fn new(agent_access: Arc<dyn AgentAccess>) -> Self {
        Self { agent_access }
    }
}

impl ToolDyn for SaveAgent {
    fn name(&self) -> String {
        "save_agent".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "save_agent".to_string(),
            description: r#"Create or update an agent by writing its agent.md configuration file: YAML frontmatter between '---' delimiters, then the system prompt in Markdown.

READ THIS FIRST — in YAML, indentation IS the data structure.
A key written at the wrong depth lands in a different place in the parsed config, and keys that land somewhere unexpected are silently ignored. A mis-indented agent.md still saves "successfully" — the damage only shows up later, as an agent running with default settings.

The frontmatter has exactly these top-level keys, each at column 0: agent, llm, tools, mcp, skills, knowledge_bases, max_iterations. They are listed here as prose, not as YAML — do not copy any indentation from this description into the file. In a correct file, the only indentation is between a key and its own children.

Only two keys nest, one level (2 spaces) deep:
- under agent: -> name, description
- under llm: -> provider, model, temperature, max_tokens, stream, reasoning_effort, web_search

Do not write template_version — that key belongs to built-in template agents and is managed by their migration.

Nothing else is nested. In particular llm / tools / mcp / skills / knowledge_bases / max_iterations are NOT children of agent:. If they end up there they are ignored, and the agent silently runs with the provider's default model, NO built-in tools at all (it cannot call anything), and max_iterations 500.

Required fields:
- agent.name — unique agent identifier (directory name, not the display name)
- agent.description — display name + " — " + what this agent does. The UI shows the text before the dash as the agent's name, so keep that part short (at most 8 characters).

Optional fields: llm, tools, mcp, skills, knowledge_bases, max_iterations (default 500).
- tools / mcp / skills / knowledge_bases are lists of names. Inline form ["shell", "fetch"] and block form with "- shell" two spaces deeper than the key are both valid YAML.
- Before choosing tools, call the list_tools tool (if available in your toolset) to discover the tool names valid in this environment. MCP server tools are selected via the mcp field (see list_mcp_servers).

Correct example — every top-level key starts at column 0:

---
agent:
  name: "code-reviewer"
  description: "代码审查员 — Reviews code for bugs and style issues"
llm:
  provider: "deepseek"
  model: "deepseek-flash"
  temperature: 0.2
tools:
  - shell
  - fetch
  - read_skill
skills: []
knowledge_bases: []
max_iterations: 15
---
You are a code reviewer. When given code, analyze it for...

WRONG — the single most common failure. Here llm / tools / max_iterations are indented under agent:, so they are ignored: this agent would have zero tools, the default model, and max_iterations 500.

---
agent:
  name: "code-reviewer"
  description: "代码审查员 — Reviews code for bugs and style issues"
  llm:
    provider: "deepseek"
  tools: ["shell"]
  max_iterations: 15
---
You are a code reviewer...

Naming convention: names starting with '@' are reserved for built-in system agents (e.g., '@master'). Do NOT prefix user-created agents with '@'.

The agent becomes immediately available for delegate_sub_agent after creation."#
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Unique agent name. Used as the directory name and identifier for delegate_sub_agent."
                    },
                    "content": {
                        "type": "string",
                        "description": "Complete agent.md file content: YAML frontmatter (between '---' delimiters) followed by Markdown system prompt."
                    }
                },
                "required": ["name", "content"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct SaveAgentArgs {
                name: String,
                content: String,
            }

            let parsed: SaveAgentArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "agent name is required and cannot be empty".into(),
                ))));
            }

            if parsed.content.trim().is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "agent content is required and cannot be empty".into(),
                ))));
            }

            // 安全边界：'@' 前缀为系统内置 Agent 保留 — 禁止新建，
            if name.starts_with('@') {
                return Err(ToolError::ToolCallError(Box::new(StringError(format!(
                    "Invalid agent name '{name}' — names starting with '@' are reserved \
                         for built-in system agents. User-created agents must not use the '@' prefix."
                )))));
            }

            // 缩进错位的 frontmatter 仍能解析成功（多出来的层级被 serde 静默忽略），
            // 落盘后就变成一个没有工具、跑默认模型的 agent —— 先解析，让模型立刻看到后果。
            let (profile, _body) = parse_agent_md(&parsed.content).map_err(|err| {
                ToolError::ToolCallError(Box::new(StringError(format!(
                    "agent '{name}' was NOT saved: its YAML frontmatter does not parse: {err}"
                ))))
            })?;

            self.agent_access
                .save_agent(name, &parsed.content)
                .map_err(|e| {
                    ToolError::ToolCallError(Box::new(StringError(format!(
                        "failed to save agent '{name}': {e}"
                    ))))
                })?;

            Ok(Content::Text(format!(
                "Agent '{name}' saved successfully. Parsed: {}.",
                parsed_summary(&profile)
            )))
        })
    }
}

/// 保存成功后回显的解析摘要。
///
/// 存在的理由是「保存成功」本身不能证明 frontmatter 写对了：错位的键会被静默忽略，
/// 把解析出来的数量回显给模型，它才有机会自己发现 tools 是空的。
fn parsed_summary(profile: &AgentProfile) -> String {
    let (provider, model) = match profile.llm.as_ref() {
        Some(llm) => (
            llm.provider.as_deref().unwrap_or("default"),
            llm.model.as_deref().unwrap_or("default"),
        ),
        None => ("default", "default"),
    };
    let tools = if profile.tools.is_empty() {
        "0 (NONE — the agent will have no tools; check that 'tools:' is at column 0)".to_string()
    } else {
        format!("{} ({})", profile.tools.len(), profile.tools.join(", "))
    };

    format!(
        "model={provider}/{model}, tools={tools}, skills={}, mcp={}, knowledge_bases={}, max_iterations={}",
        profile.skills.len(),
        profile.mcp.len(),
        profile.knowledge_bases.len(),
        profile.max_iterations
    )
}

// ── DeleteAgent ──────────────────────────────────────────────────────────────

pub struct DeleteAgent {
    agent_access: Arc<dyn AgentAccess>,
}

impl DeleteAgent {
    pub fn new(agent_access: Arc<dyn AgentAccess>) -> Self {
        Self { agent_access }
    }
}

impl ToolDyn for DeleteAgent {
    fn name(&self) -> String {
        "delete_agent".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delete_agent".to_string(),
            description: "Delete an agent and its agent.md file. This is irreversible. \
                The agent directory and all its contents will be permanently removed. "
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The agent name to delete."
                    },
                    "confirm": {
                        "type": "boolean",
                        "description": "Must be explicitly set to true to confirm deletion. \
                            Set to false or omit to abort."
                    }
                },
                "required": ["name", "confirm"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct DeleteAgentArgs {
                name: String,
                confirm: bool,
            }

            let parsed: DeleteAgentArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            if !parsed.confirm {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "Deletion not confirmed. Set 'confirm' to true to proceed.".into(),
                ))));
            }

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "agent name is required and cannot be empty".into(),
                ))));
            }

            self.agent_access.delete_agent(name).map_err(|e| {
                ToolError::ToolCallError(Box::new(StringError(format!(
                    "failed to delete agent '{name}': {e}"
                ))))
            })?;

            Ok(Content::Text(format!(
                "Agent '{name}' deleted successfully."
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Agent, AgentError};
    use std::path::PathBuf;

    /// 只覆盖 SaveAgent 走到的路径：落盘到临时目录下的 `<name>/agent.md`。
    struct FakeAgentAccess {
        root: PathBuf,
    }

    impl FakeAgentAccess {
        fn md_path(&self, name: &str) -> PathBuf {
            self.root.join(name).join("agent.md")
        }
    }

    impl AgentAccess for FakeAgentAccess {
        fn load_agent(&self, name: &str) -> Result<Arc<Agent>, AgentError> {
            Err(AgentError::MissingField(format!(
                "load_agent('{name}') is not implemented by the test fake"
            )))
        }

        fn list_agent_names(&self) -> Vec<String> {
            Vec::new()
        }

        fn save_agent(&self, name: &str, content: &str) -> Result<(), String> {
            let path = self.md_path(name);
            std::fs::create_dir_all(path.parent().expect("agent.md path has a parent"))
                .map_err(|e| e.to_string())?;
            std::fs::write(&path, content).map_err(|e| e.to_string())
        }

        fn read_agent(&self, name: &str) -> Result<String, String> {
            std::fs::read_to_string(self.md_path(name)).map_err(|e| e.to_string())
        }

        fn delete_agent(&self, name: &str) -> Result<(), String> {
            std::fs::remove_dir_all(self.root.join(name)).map_err(|e| e.to_string())
        }
    }

    fn make_tool(tmp: &tempfile::TempDir) -> SaveAgent {
        SaveAgent::new(Arc::new(FakeAgentAccess {
            root: tmp.path().to_path_buf(),
        }))
    }

    async fn call(tool: &SaveAgent, name: &str, content: &str) -> Result<String, String> {
        let args = json!({ "name": name, "content": content }).to_string();
        match tool.call(args).await {
            Ok(Content::Text(text)) => Ok(text),
            Ok(other) => panic!("expected text content, got {other:?}"),
            Err(err) => Err(err.to_string()),
        }
    }

    /// llm / tools / max_iterations 缩进到 `agent:` 之下 —— serde 静默忽略。
    const MISINDENTED: &str = r#"---
agent:
  name: "misindented"
  description: "错缩进 — misindented test agent"
  llm:
    provider: "deepseek"
  tools: ["shell"]
  max_iterations: 15
---
You are a test agent.
"#;

    /// 缺 `agent:` 段 —— 无法解析成 AgentProfile。
    const UNPARSEABLE: &str = r#"---
llm:
  provider: "deepseek"
---
You are a test agent.
"#;

    const WELL_FORMED: &str = r#"---
agent:
  name: "good"
  description: "规整 — well formed test agent"
llm:
  provider: "deepseek"
  model: "deepseek-flash"
tools:
  - shell
  - fetch
---
You are a test agent.
"#;

    #[test]
    fn save_agent_description_keeps_indentation_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let description = make_tool(&tmp).definition().description;

        for phrase in ["at column 0", "NO built-in tools", "WRONG"] {
            assert!(
                description.contains(phrase),
                "save_agent description lost the guard phrase {phrase:?}"
            );
        }
    }

    #[tokio::test]
    async fn misindented_frontmatter_is_saved_but_flagged_as_toolless() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = make_tool(&tmp);

        let message = call(&tool, "misindented", MISINDENTED)
            .await
            .expect("mis-indented but valid YAML is still saved");

        assert_eq!(
            std::fs::read_to_string(tmp.path().join("misindented").join("agent.md")).unwrap(),
            MISINDENTED
        );
        assert!(
            message.contains("tools=0"),
            "summary must expose the tool count, got: {message}"
        );
        assert!(
            message.contains("NONE"),
            "zero tools must be called out, got: {message}"
        );
        assert!(
            message.contains("model=default/default"),
            "keys nested under agent: are ignored, so llm must fall back to default: {message}"
        );
    }

    #[tokio::test]
    async fn unparseable_frontmatter_is_rejected_before_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = make_tool(&tmp);
        let path = tmp.path().join("broken").join("agent.md");

        let err = call(&tool, "broken", UNPARSEABLE)
            .await
            .expect_err("frontmatter without an agent: section cannot be parsed");
        assert!(
            err.contains("was NOT saved"),
            "error must say nothing was written, got: {err}"
        );
        assert!(!path.exists(), "rejected content must not be written");

        // 已存在的文件也必须保持原样
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "original").unwrap();
        let err = call(&tool, "broken", UNPARSEABLE).await.unwrap_err();
        assert!(err.contains("was NOT saved"), "got: {err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    }

    #[tokio::test]
    async fn well_formed_frontmatter_reports_parsed_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = make_tool(&tmp);

        let message = call(&tool, "good", WELL_FORMED).await.unwrap();

        assert!(
            message.contains("tools=2 (shell, fetch)"),
            "summary must echo the parsed tool count, got: {message}"
        );
        assert!(
            message.contains("model=deepseek/deepseek-flash"),
            "summary must echo provider/model, got: {message}"
        );
        assert!(
            message.contains("skills=0"),
            "summary must echo every count, got: {message}"
        );
        assert!(
            message.contains("max_iterations=500"),
            "summary must echo the effective max_iterations, got: {message}"
        );
    }
}
