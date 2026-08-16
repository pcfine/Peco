// ============================================================================
// ExecuteWorkflow — 让 Agent 可以调用 Workflow 的工具
// ============================================================================
//
// 遵循 DelegateSubAgent / SaveAgent 的 ToolDyn 实现模式。
//
// 阻塞语义：Agent 的 ReAct 迭代会等待 Workflow 执行完毕。
// Pause 场景下自动 cancel 并返回错误（Agent 无法在工具调用中途处理审批）。

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use crate::tools::AgentAccess;
use crate::tools::{StringError, ToolDyn, ToolError};

use super::super::access::WorkflowAccess;
use super::super::engine::{WorkflowConfig, WorkflowEngine};
use super::super::events::WorkflowEvent;
use super::super::persistence::WorkflowPersister;

pub struct ExecuteWorkflow {
    workflow_access: Arc<dyn WorkflowAccess>,
    agent_access: Arc<dyn AgentAccess>,
    persister: Arc<dyn WorkflowPersister>,
}

impl ExecuteWorkflow {
    pub fn new(
        workflow_access: Arc<dyn WorkflowAccess>,
        agent_access: Arc<dyn AgentAccess>,
        persister: Arc<dyn WorkflowPersister>,
    ) -> Self {
        Self {
            workflow_access,
            agent_access,
            persister,
        }
    }
}

impl ToolDyn for ExecuteWorkflow {
    fn name(&self) -> String {
        "execute_workflow".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "execute_workflow".to_string(),
            description: "Execute a predefined workflow. A workflow is an automated multi-step \
                process where steps can run in serial, parallel, or conditionally. \
                Note: this tool synchronously waits for the workflow to finish, \
                so it is best for short workflows (expected to complete within 30 seconds). \
                For long-running workflows, use the CLI or UI directly."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "workflow_name": {
                        "type": "string",
                        "description": "The name of the workflow to execute"
                    },
                    "params": {
                        "type": "object",
                        "description": "Optional external input parameters to pass to the workflow"
                    }
                },
                "required": ["workflow_name"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct ExecuteArgs {
                workflow_name: String,
                #[serde(default)]
                params: serde_json::Map<String, serde_json::Value>,
            }

            let parsed: ExecuteArgs = serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            // 1. 加载 WorkflowDefinition
            let definition = self
                .workflow_access
                .load_workflow(&parsed.workflow_name)
                .map_err(|e| {
                    ToolError::ToolCallError(Box::new(StringError(format!(
                        "failed to load workflow '{}': {e}",
                        parsed.workflow_name
                    ))))
                })?;

            // 2. 启动引擎
            let config = WorkflowConfig::default();
            let inputs: std::collections::HashMap<String, serde_json::Value> =
                parsed.params.into_iter().collect();
            let mut handle = WorkflowEngine::spawn(
                definition,
                self.agent_access.clone(),
                self.persister.clone(),
                config,
                inputs,
            );

            // 3. 收集事件，等待完成/失败
            let mut outputs: Vec<String> = Vec::new();
            loop {
                match handle.recv_event().await {
                    Some(WorkflowEvent::StepCompleted { output, .. }) => {
                        outputs.push(output);
                    }
                    Some(WorkflowEvent::Paused { reason, .. }) => {
                        handle.cancel();
                        return Err(ToolError::ToolCallError(Box::new(StringError(format!(
                            "Workflow paused and requires human approval: {reason}. \
                             Please run this workflow via the UI or CLI to handle approvals."
                        )))));
                    }
                    Some(WorkflowEvent::Completed {
                        steps_completed,
                        steps_failed,
                        ..
                    }) => {
                        return Ok(format!(
                            "Workflow completed: {steps_completed} steps succeeded, \
                             {steps_failed} steps failed.\n\nOutputs:\n{}",
                            outputs.join("\n---\n")
                        ));
                    }
                    Some(WorkflowEvent::Failed { error, .. }) => {
                        return Err(ToolError::ToolCallError(Box::new(StringError(format!(
                            "Workflow failed: {error}"
                        )))));
                    }
                    Some(WorkflowEvent::Cancelled { .. }) => {
                        return Err(ToolError::ToolCallError(Box::new(StringError(
                            "Workflow cancelled.".to_string(),
                        ))));
                    }
                    Some(WorkflowEvent::TimedOut { error, .. }) => {
                        return Err(ToolError::ToolCallError(Box::new(StringError(format!(
                            "Workflow timed out: {error}"
                        )))));
                    }
                    None => {
                        return Err(ToolError::ToolCallError(Box::new(StringError(
                            "Workflow ended unexpectedly.".to_string(),
                        ))));
                    }
                    _ => {} // StepStarted, StepSkipped, etc.
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::definition::{
        OnFailure, StepConfig, StepType, WorkflowDefinition, WorkflowStep,
    };
    use crate::workflow::persistence::NullWorkflowPersister;
    use std::collections::HashMap;

    /// Stub WorkflowAccess that returns pre-loaded definitions.
    struct StubWorkflowAccess {
        definitions: HashMap<String, WorkflowDefinition>,
    }

    impl WorkflowAccess for StubWorkflowAccess {
        fn load_workflow(
            &self,
            name: &str,
        ) -> Result<WorkflowDefinition, crate::workflow::WorkflowError> {
            self.definitions
                .get(name)
                .cloned()
                .ok_or_else(|| crate::workflow::WorkflowError::Parse(format!("not found: {name}")))
        }

        fn list_workflow_names(&self) -> Vec<String> {
            self.definitions.keys().cloned().collect()
        }

        fn list_workflow_meta(&self) -> Vec<crate::workflow::WorkflowMeta> {
            self.definitions
                .iter()
                .map(|(name, def)| crate::workflow::WorkflowMeta {
                    name: name.clone(),
                    description: def.description.clone(),
                    version: def.version.clone(),
                    step_count: def.steps.len(),
                })
                .collect()
        }

        fn reload_workflow(
            &self,
            name: &str,
        ) -> Result<WorkflowDefinition, crate::workflow::WorkflowError> {
            self.load_workflow(name)
        }

        fn save_workflow(&self, _name: &str, _content: &str) -> Result<(), String> {
            Err("save_workflow not supported in stub".to_string())
        }

        fn delete_workflow(&self, _name: &str) -> Result<(), String> {
            Err("delete_workflow not supported in stub".to_string())
        }
    }

    /// Stub AgentAccess (not used for shell steps, but required).
    struct StubAgentAccess;

    impl AgentAccess for StubAgentAccess {
        fn load_agent(
            &self,
            _name: &str,
        ) -> Result<Arc<crate::agent::Agent>, crate::agent::AgentError> {
            Err(crate::agent::AgentError::Config("stub".into()))
        }
        fn list_agent_names(&self) -> Vec<String> {
            vec![]
        }
        fn save_agent(&self, _name: &str, _content: &str) -> Result<(), String> {
            Ok(())
        }
        fn read_agent(&self, _name: &str) -> Result<String, String> {
            Err("not supported".into())
        }
        fn delete_agent(&self, _name: &str) -> Result<(), String> {
            Err("not supported".into())
        }
    }

    #[tokio::test]
    async fn test_execute_workflow_success() {
        let steps = vec![WorkflowStep {
            id: "A".into(),
            name: "Echo".into(),
            step_type: StepType::Shell,
            config: StepConfig::Shell {
                command: "echo hello".into(),
            },
            depends_on: vec![],
            condition: None,
            timeout_seconds: Some(30),
            on_failure: OnFailure::Abort,
            retry_policy: None,
            output_schema: None,
        }];

        let def = WorkflowDefinition {
            name: "test-wf".into(),
            description: "test".into(),
            version: "1.0".into(),
            timeout_seconds: None,
            inputs: HashMap::new(),
            steps,
            body: None,
        };

        let mut definitions = HashMap::new();
        definitions.insert("test-wf".to_string(), def);

        let tool = ExecuteWorkflow::new(
            Arc::new(StubWorkflowAccess { definitions }),
            Arc::new(StubAgentAccess),
            Arc::new(NullWorkflowPersister),
        );

        let result = tool
            .call(r#"{"workflow_name": "test-wf"}"#.to_string())
            .await
            .unwrap();
        assert!(result.contains("Workflow completed"));
        assert!(result.contains("1 steps succeeded"));
    }

    #[tokio::test]
    async fn test_execute_workflow_not_found() {
        let tool = ExecuteWorkflow::new(
            Arc::new(StubWorkflowAccess {
                definitions: HashMap::new(),
            }),
            Arc::new(StubAgentAccess),
            Arc::new(NullWorkflowPersister),
        );

        let result = tool
            .call(r#"{"workflow_name": "nonexistent"}"#.to_string())
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"), "expected 'not found' in: {err}");
    }
}
