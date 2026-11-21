// ============================================================================
// SaveWorkflow — 创建或更新 workflow.md
// ============================================================================

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use crate::tools::{Content, StringError, ToolDyn, ToolError};
use crate::workflow::WorkflowAccess;

pub struct SaveWorkflow {
    workflow_access: Arc<dyn WorkflowAccess>,
}

impl SaveWorkflow {
    pub fn new(workflow_access: Arc<dyn WorkflowAccess>) -> Self {
        Self { workflow_access }
    }
}

impl ToolDyn for SaveWorkflow {
    fn name(&self) -> String {
        "save_workflow".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "save_workflow".to_string(),
            description: r#"Create or update a workflow by writing its workflow.md file. If the workflow already exists, it is updated in place. The content is YAML frontmatter between '---' delimiters, followed by an optional Markdown body used for documentation only.

READ THIS FIRST — in YAML, indentation IS the data structure.
Every workflow field lives inside a single top-level 'workflow:' key at column 0. Frontmatter that puts steps at top level, without the workflow: wrapper, is rejected with "Missing top-level 'workflow:' key" and nothing loads.

Required frontmatter (each key sits 2 spaces under `workflow:`):
- workflow.name — unique workflow identifier
- workflow.description — what the workflow does
- workflow.version — string, e.g. "1.0". Omitting it is a parse error, it has no default.
- workflow.steps — array of step definitions

Optional: workflow.timeout_seconds, workflow.inputs.
Each step requires: id, name, type (shell or agent), config. Optional per step: depends_on, condition, timeout_seconds, on_failure, retry_policy, output_schema.

Step config by type:
- shell: { command: "shell command to run" }
- agent: { agent: "@agent-name", prompt: "what the agent should do" }   (agent steps also accept optional max_iterations)

Correct example — workflow: at column 0, each nested key 2 spaces deeper than its parent:

---
workflow:
  name: "my-workflow"
  description: "What this workflow does"
  version: "1.0"
  steps:
    - id: "step1"
      name: "Run analysis"
      type: agent
      config:
        agent: "@analyst"
        prompt: "Analyze the data"
    - id: "step2"
      name: "Build"
      type: shell
      config:
        command: "cargo build"
---
Optional Markdown documentation goes here.

The workflow becomes immediately available for execute_workflow after creation."#
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Unique workflow name (ASCII alphanumeric, underscores, hyphens). 1-128 chars."
                    },
                    "content": {
                        "type": "string",
                        "description": "Complete workflow.md content: YAML frontmatter + optional Markdown body."
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
            struct SaveWorkflowArgs {
                name: String,
                content: String,
            }

            let parsed: SaveWorkflowArgs =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let name = parsed.name.trim();
            if name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "workflow name is required and cannot be empty".into(),
                ))));
            }
            if parsed.content.trim().is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "workflow content is required and cannot be empty".into(),
                ))));
            }

            self.workflow_access
                .save_workflow(name, &parsed.content)
                .map_err(|e| {
                    ToolError::ToolCallError(Box::new(StringError(format!(
                        "failed to save workflow '{name}': {e}"
                    ))))
                })?;

            Ok(Content::Text(format!(
                "Workflow '{name}' saved successfully."
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::definition::WorkflowDefinition;
    use crate::workflow::{WorkflowError, WorkflowMeta};

    struct StubWorkflowAccess;

    impl WorkflowAccess for StubWorkflowAccess {
        fn load_workflow(&self, _name: &str) -> Result<WorkflowDefinition, WorkflowError> {
            unimplemented!()
        }
        fn list_workflow_names(&self) -> Vec<String> {
            unimplemented!()
        }
        fn list_workflow_meta(&self) -> Vec<WorkflowMeta> {
            unimplemented!()
        }
        fn reload_workflow(&self, _name: &str) -> Result<WorkflowDefinition, WorkflowError> {
            unimplemented!()
        }
        fn save_workflow(&self, _name: &str, _content: &str) -> Result<(), String> {
            unimplemented!()
        }
        fn delete_workflow(&self, _name: &str) -> Result<(), String> {
            unimplemented!()
        }
    }

    /// 描述文本是模型最容易照抄的样例，因此测试必须读到真实对外暴露的那一份。
    fn description() -> String {
        SaveWorkflow::new(Arc::new(StubWorkflowAccess))
            .definition()
            .description
    }

    fn is_fence(line: &str) -> bool {
        line.trim_end() == "---"
    }

    /// 示例块之外的散文若被缩进，模型会照抄出「顶级键被缩进」的 YAML，
    /// 那是必然解析失败的写法，必须在测试期报出来。
    #[test]
    fn description_prose_is_not_indented() {
        let description = description();
        let mut inside_example = false;
        let mut indented_example_lines = 0;

        for (idx, line) in description.lines().enumerate() {
            let line_no = idx + 1;
            if is_fence(line) {
                inside_example = !inside_example;
                continue;
            }
            if inside_example {
                if line.starts_with("  ") {
                    indented_example_lines += 1;
                }
            } else {
                assert!(
                    !line.starts_with(' ') && !line.starts_with('\t'),
                    "description line {line_no} is indented outside the example block, \
                     which teaches YAML that cannot parse: {line:?}\n\n{description}"
                );
            }
        }

        assert!(
            !inside_example,
            "example block is not closed, so the prose after it escapes checking:\n\n{description}"
        );
        assert!(
            indented_example_lines > 0,
            "example block has no 2-space indented line, so it no longer shows nesting \
             (the description must keep a nested example):\n\n{description}"
        );
    }

    /// 把描述里的示例原样喂给真实解析器：示例本身解析不了就说明它在教错写法。
    #[test]
    fn description_example_parses() {
        let description = description();
        let lines: Vec<&str> = description.lines().collect();
        let open = lines
            .iter()
            .position(|l| is_fence(l))
            .unwrap_or_else(|| panic!("description has no example block:\n\n{description}"));
        let close = lines[open + 1..]
            .iter()
            .position(|l| is_fence(l))
            .map(|i| open + 1 + i)
            .unwrap_or_else(|| {
                panic!("example block in description is not closed:\n\n{description}")
            });
        let example = lines[open..=close].join("\n");

        let def = WorkflowDefinition::from_yaml(&example)
            .unwrap_or_else(|e| panic!("example in the description must parse: {e}\n\n{example}"));

        assert_eq!(def.steps.len(), 2, "unexpected steps in:\n{example}");
        assert_eq!(def.version, "1.0", "unexpected version in:\n{example}");
        assert_eq!(def.name, "my-workflow", "unexpected name in:\n{example}");
    }
}
