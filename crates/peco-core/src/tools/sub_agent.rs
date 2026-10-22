// ============================================================================
// sub_agent — Sub-Agent delegation tools (dependency-injected)
// ============================================================================
//
// Both CLI and Web use the same tool implementations.
// Difference is only in how the AgentAccess is constructed:
// - CLI: WorkSpace reads agents/ from local filesystem
// - Web: WorkSpace reads agents/ from user workspace directory

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use model_provider::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use super::deps::AgentAccess;
use crate::agent::simple_looper::{SimpleAgentLooper, SimpleLooperOutput};
use crate::artifacts::{ArtifactDecl, with_artifact_lines};

use super::{Content, StringError, ToolDyn, ToolError};

// ============================================================================
// DelegateSubAgent
// ============================================================================

pub struct DelegateSubAgent {
    agent_access: Arc<dyn AgentAccess>,
}

impl DelegateSubAgent {
    pub fn new(agent_access: Arc<dyn AgentAccess>) -> Self {
        Self { agent_access }
    }
}

impl ToolDyn for DelegateSubAgent {
    fn name(&self) -> String {
        "delegate_sub_agent".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delegate_sub_agent".to_string(),
            description: "Delegate a task to a sub-agent by name and wait for the result. \
                The sub-agent runs a full ReAct loop (model → tools → ... → final answer) \
                and returns its output. Use this for single subtasks."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "agent_name": {
                        "type": "string",
                        "description": "Name of the sub-agent to delegate to. Must match an existing agent name exactly."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The task description to send to the sub-agent."
                    }
                },
                "required": ["agent_name", "prompt"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct SubAgentArgs {
                agent_name: String,
                prompt: String,
            }

            let parsed: SubAgentArgs = serde_json::from_str(&args).map_err(ToolError::JsonError)?;

            let agent_name = parsed.agent_name.trim();
            if agent_name.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "agent_name is required".into(),
                ))));
            }

            let agent = self.agent_access.load_agent(agent_name).map_err(|e| {
                ToolError::ToolCallError(Box::new(StringError(format!(
                    "failed to load agent '{agent_name}': {e}"
                ))))
            })?;

            let handle = SimpleAgentLooper::spawn(agent, parsed.prompt, None);
            let out = handle.wait().await.map_err(|e| {
                ToolError::ToolCallError(Box::new(StringError(format!(
                    "sub-agent '{agent_name}' execution failed: {e}"
                ))))
            })?;

            // 声明必须随正文一起上浮：宿主层拿到的这个 `Content` 是它唯一的
            // 落库输入，留在这里等于丢。失败消息不拼声明 —— 那时没有可信候选。
            Ok(Content::Text(with_artifact_lines(
                &out.text,
                &out.artifacts,
            )))
        })
    }
}

// ============================================================================
// RunParallelSubAgents
// ============================================================================

/// 单个并行任务的采集结果。
///
/// 失败保留为 `Err(String)` 而不是 `AgentError`：宿主要的是展示文本，
/// 而这里的错误已经过 `wait()` 归一。
pub(crate) struct SubAgentTaskOutcome {
    pub agent_name: String,
    pub prompt: String,
    pub outcome: Result<SimpleLooperOutput, String>,
}

/// 把并行结果渲染成给模型看的 JSON，并把所有声明**上浮到 JSON 之外**。
///
/// 声明绝不能写进 `output` / `error` 字段：JSON 会把 marker 及其后的换行一起
/// 转义，父层剥离按行首匹配就再也剥不掉，marker 会漏进模型上下文并诱导它
/// 照着编造产物。因此声明行前置在整个 JSON 之前。
///
/// 顺序按任务入参顺序（失败任务不贡献声明），与 JSON 中的数组顺序一致，
/// 便于宿主把候选与 `AgentCallEnd` 事件对齐。
pub(crate) fn render_parallel_results(tasks: &[SubAgentTaskOutcome]) -> String {
    let values: Vec<serde_json::Value> = tasks
        .iter()
        .map(|t| match &t.outcome {
            Ok(out) => json!({
                "agent_name": t.agent_name,
                "prompt": t.prompt,
                "status": "completed",
                "output": out.text,
            }),
            Err(err) => json!({
                "agent_name": t.agent_name,
                "prompt": t.prompt,
                "status": "failed",
                "error": err,
            }),
        })
        .collect();

    let json =
        serde_json::to_string_pretty(&values).expect("JSON value array is always serializable");
    let decls: Vec<ArtifactDecl> = tasks
        .iter()
        .filter_map(|t| t.outcome.as_ref().ok())
        .flat_map(|o| o.artifacts.iter().cloned())
        .collect();
    with_artifact_lines(&json, &decls)
}

pub struct RunParallelSubAgents {
    agent_access: Arc<dyn AgentAccess>,
}

impl RunParallelSubAgents {
    pub fn new(agent_access: Arc<dyn AgentAccess>) -> Self {
        Self { agent_access }
    }
}

#[derive(Debug, Deserialize)]
struct ParallelTaskDef {
    agent_name: String,
    prompt: String,
}

impl ToolDyn for RunParallelSubAgents {
    fn name(&self) -> String {
        "run_parallel_sub_agents".to_string()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_parallel_sub_agents".to_string(),
            description: "Run multiple sub-agent tasks in parallel and return all results. \
                Each task is defined by an agent_name and a prompt. All tasks run concurrently."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "description": "List of sub-agent tasks to run in parallel.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "agent_name": {
                                    "type": "string",
                                    "description": "Name of the sub-agent to delegate to. Must match an existing agent name exactly."
                                },
                                "prompt": {
                                    "type": "string",
                                    "description": "The task description to send to the sub-agent."
                                }
                            },
                            "required": ["agent_name", "prompt"]
                        }
                    }
                },
                "required": ["tasks"]
            }),
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<Content, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct TasksWrapper {
                tasks: Vec<ParallelTaskDef>,
            }

            let wrapper: TasksWrapper =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let task_defs = wrapper.tasks;

            if task_defs.is_empty() {
                return Err(ToolError::ToolCallError(Box::new(StringError(
                    "tasks array is empty".into(),
                ))));
            }

            // Load all agents
            let mut agent_pairs = Vec::with_capacity(task_defs.len());
            for td in &task_defs {
                let agent = self.agent_access.load_agent(&td.agent_name).map_err(|e| {
                    ToolError::ToolCallError(Box::new(StringError(format!(
                        "failed to load agent '{}': {e}",
                        td.agent_name
                    ))))
                })?;
                agent_pairs.push((td.agent_name.clone(), td.prompt.clone(), agent));
            }

            // Spawn all concurrently
            struct IndexedHandle {
                agent_name: String,
                prompt: String,
                handle: crate::agent::SimpleLooperHandle,
            }

            let mut handles = Vec::with_capacity(agent_pairs.len());
            for (name, prompt, agent) in agent_pairs {
                let handle = SimpleAgentLooper::spawn(agent, prompt.clone(), None);
                handles.push(IndexedHandle {
                    agent_name: name,
                    prompt,
                    handle,
                });
            }

            // Collect results
            let mut outcomes = Vec::with_capacity(handles.len());
            for h in handles {
                let outcome = h.handle.wait().await.map_err(|e| e.to_string());
                outcomes.push(SubAgentTaskOutcome {
                    agent_name: h.agent_name,
                    prompt: h.prompt,
                    outcome,
                });
            }

            Ok(Content::Text(render_parallel_results(&outcomes)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::test_support::*;
    use crate::artifacts::{ARTIFACT_MARKER, strip_artifact_lines};
    use model_provider::{FinishReason, ResponseError, ResponseStatus};
    use std::collections::HashMap;

    fn decl(path: &str) -> ArtifactDecl {
        ArtifactDecl {
            path: path.to_string(),
            title: None,
            mime: None,
        }
    }

    fn ok_outcome(
        agent_name: &str,
        prompt: &str,
        text: &str,
        artifacts: Vec<ArtifactDecl>,
    ) -> SubAgentTaskOutcome {
        SubAgentTaskOutcome {
            agent_name: agent_name.to_string(),
            prompt: prompt.to_string(),
            outcome: Ok(SimpleLooperOutput {
                text: text.to_string(),
                artifacts,
            }),
        }
    }

    fn failed_outcome(agent_name: &str, prompt: &str, err: &str) -> SubAgentTaskOutcome {
        SubAgentTaskOutcome {
            agent_name: agent_name.to_string(),
            prompt: prompt.to_string(),
            outcome: Err(err.to_string()),
        }
    }

    /// 声明在 JSON 之外、JSON 本身与 HEAD 同形。
    ///
    /// 变异点：把声明塞进 `output` 字段 ⇒ 剥离后 JSON 里仍能找到 marker，断言红。
    #[test]
    fn test_render_parallel_results_hoists_artifacts_above_json() {
        let tasks = vec![ok_outcome("a1", "p1", "正文一", vec![decl("/tmp/1.png")])];

        let text = render_parallel_results(&tasks);
        let (json, decls) = strip_artifact_lines(&text);

        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&json).expect("stripped body must stay valid JSON");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["agent_name"], "a1");
        assert_eq!(parsed[0]["prompt"], "p1");
        assert_eq!(parsed[0]["status"], "completed");
        assert_eq!(parsed[0]["output"], "正文一");
        assert_eq!(
            json,
            serde_json::to_string_pretty(&vec![json!({
                "agent_name": "a1",
                "prompt": "p1",
                "status": "completed",
                "output": "正文一",
            })])
            .unwrap(),
            "字段集与顺序必须与 HEAD 一致"
        );
        assert!(
            !json.contains(ARTIFACT_MARKER),
            "marker must not appear inside the JSON body: {json}"
        );
        assert_eq!(decls, vec![decl("/tmp/1.png")]);
    }

    /// 多任务多声明：声明顺序 == 任务顺序（每任务 2 条）。
    ///
    /// 变异点：用 `HashMap`/排序收集 ⇒ 声明顺序与任务顺序脱钩，断言红。
    #[test]
    fn test_render_parallel_results_keeps_task_order() {
        let tasks = vec![
            ok_outcome(
                "a1",
                "p1",
                "正文一",
                vec![decl("/tmp/a1-1.png"), decl("/tmp/a1-2.png")],
            ),
            ok_outcome(
                "a2",
                "p2",
                "正文二",
                vec![decl("/tmp/a2-1.png"), decl("/tmp/a2-2.png")],
            ),
        ];

        let (_, decls) = strip_artifact_lines(&render_parallel_results(&tasks));
        assert_eq!(
            decls,
            vec![
                decl("/tmp/a1-1.png"),
                decl("/tmp/a1-2.png"),
                decl("/tmp/a2-1.png"),
                decl("/tmp/a2-2.png"),
            ]
        );
    }

    /// 一失败一成功：失败项只带 error，成功项声明照样上浮。
    ///
    /// 变异点：把失败项的声明也算进去（失败项没有声明可算）或把成功项跳过
    /// ⇒ 条数断言红；把声明拼进 error ⇒ error 里出现 marker。
    #[test]
    fn test_render_parallel_results_failed_task_contributes_no_artifacts() {
        let tasks = vec![
            failed_outcome("a1", "p1", "boom"),
            ok_outcome("a2", "p2", "正文二", vec![decl("/tmp/2.png")]),
        ];

        let text = render_parallel_results(&tasks);
        let (json, decls) = strip_artifact_lines(&text);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed[0]["status"], "failed");
        assert_eq!(parsed[0]["error"], "boom");
        assert!(
            !parsed[0]["error"]
                .as_str()
                .unwrap()
                .contains(ARTIFACT_MARKER)
        );
        assert_eq!(parsed[1]["status"], "completed");
        assert_eq!(decls, vec![decl("/tmp/2.png")]);
    }

    /// 无声明：delegate 与 parallel 的输出都不得多出任何字节。
    ///
    /// 变异点：`with_artifact_lines` 在空声明时仍追加换行/前缀 ⇒ 逐字节比较红。
    #[tokio::test]
    async fn test_no_artifacts_means_no_injection() {
        let (agent, _provider) = scripted_agent(vec![completed("子 agent 正文")]);
        let access = MapAgentAccess(HashMap::from([("worker".to_string(), agent)]));
        let tool = DelegateSubAgent::new(Arc::new(access));

        let content = tool
            .call(r#"{"agent_name":"worker","prompt":"跑一下"}"#.to_string())
            .await
            .expect("delegate should succeed");
        assert_eq!(content, Content::Text("子 agent 正文".to_string()));

        let tasks = vec![ok_outcome("a1", "p1", "正文一", vec![])];
        let expected = serde_json::to_string_pretty(&vec![json!({
            "agent_name": "a1",
            "prompt": "p1",
            "status": "completed",
            "output": "正文一",
        })])
        .unwrap();
        assert_eq!(render_parallel_results(&tasks), expected);
    }

    /// 端到端：子 agent 工具输出里的声明必须出现在 `delegate_sub_agent`
    /// 返回的 `Content` 顶部，正文逐字节干净。
    ///
    /// 变异点：`DelegateSubAgent::call` 丢掉 `out.artifacts`（这是本次改动前
    /// 的行为）⇒ 首行不是 marker、声明数为 0，断言红。
    #[tokio::test]
    async fn test_delegate_returns_artifacts_with_clean_body() {
        let (agent, _provider) = scripted_agent_with_tools(
            vec![
                tool_call_response("call-1", "gen_artifact", json!({})),
                completed("子 agent 最终正文"),
            ],
            vec![Box::new(StaticTool::new(
                "gen_artifact",
                Content::Text("@@PECO_ARTIFACT {\"path\":\"/tmp/a.jpg\"}\n生成完成".to_string()),
            ))],
        );
        let access = MapAgentAccess(HashMap::from([("worker".to_string(), agent)]));
        let tool = DelegateSubAgent::new(Arc::new(access));

        let content = tool
            .call(r#"{"agent_name":"worker","prompt":"跑一下"}"#.to_string())
            .await
            .expect("delegate should succeed");
        let Content::Text(text) = content else {
            panic!("delegate must return plain text, got {content:?}");
        };

        assert!(
            text.lines()
                .next()
                .unwrap_or_default()
                .starts_with(ARTIFACT_MARKER),
            "declaration must be the first line: {text:?}"
        );
        let (clean, decls) = strip_artifact_lines(&text);
        assert_eq!(clean, "子 agent 最终正文");
        assert_eq!(decls, vec![decl("/tmp/a.jpg")]);
    }

    /// 端到端：并行任务里失败项不影响成功项的声明上浮，且顺序按任务序。
    ///
    /// 每个子 agent 必须持有独立的 `ScriptedBatchProvider`：共用一个
    /// `VecDeque` 脚本队列时并发执行会互相抢下一个响应，断言抖动。
    ///
    /// 变异点：把声明写进 `output` 字段 ⇒ JSON 里漏 marker；收集时按失败项
    /// 短路 ⇒ 声明只剩 1 条。
    #[tokio::test]
    async fn test_parallel_end_to_end_hoists_artifacts_in_task_order() {
        let (agent_a, _pa) = scripted_agent_with_tools(
            vec![
                tool_call_response("call-a", "gen_artifact", json!({})),
                completed("任务一正文"),
            ],
            vec![Box::new(StaticTool::new(
                "gen_artifact",
                Content::Text(format!(
                    "{}任务一产物",
                    crate::artifacts::render_artifact_lines(&[decl("/tmp/a1.png")])
                )),
            ))],
        );

        let mut failed = completed("半截");
        failed.status = ResponseStatus::Failed;
        failed.finish_reason = Some(FinishReason::Error);
        failed.error = Some(ResponseError {
            code: None,
            message: "upstream boom".into(),
        });
        let (agent_b, _pb) = scripted_agent(vec![failed]);

        let (agent_c, _pc) = scripted_agent_with_tools(
            vec![
                tool_call_response("call-c", "gen_artifact", json!({})),
                completed("任务三正文"),
            ],
            vec![Box::new(StaticTool::new(
                "gen_artifact",
                Content::Text(format!(
                    "{}任务三产物",
                    crate::artifacts::render_artifact_lines(&[decl("/tmp/c1.png")])
                )),
            ))],
        );

        let access = MapAgentAccess(HashMap::from([
            ("a".to_string(), agent_a),
            ("b".to_string(), agent_b),
            ("c".to_string(), agent_c),
        ]));
        let tool = RunParallelSubAgents::new(Arc::new(access));

        let content = tool
            .call(
                json!({
                    "tasks": [
                        {"agent_name": "a", "prompt": "p1"},
                        {"agent_name": "b", "prompt": "p2"},
                        {"agent_name": "c", "prompt": "p3"},
                    ]
                })
                .to_string(),
            )
            .await
            .expect("parallel should succeed");
        let Content::Text(text) = content else {
            panic!("parallel must return plain text, got {content:?}");
        };

        let (json, decls) = strip_artifact_lines(&text);
        assert!(
            !json.contains(ARTIFACT_MARKER),
            "marker leaked into JSON body: {json}"
        );
        let items: Vec<serde_json::Value> =
            serde_json::from_str(&json).expect("stripped body must stay valid JSON");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["output"], "任务一正文");
        assert_eq!(items[1]["status"], "failed");
        assert_eq!(items[2]["output"], "任务三正文");
        assert_eq!(decls, vec![decl("/tmp/a1.png"), decl("/tmp/c1.png")]);
    }

    /// 组合闭合：父层 looper 剥离子 agent 注入的声明行，正文与声明都不失真。
    ///
    /// 用 2 条带 `title`/`mime` 的声明压住字段保真度（序列化丢字段会在断言红）。
    ///
    /// 变异点：父层剥离被绕过（注入的不是行首声明）⇒ `out.text` 带 marker、
    /// `out.artifacts` 为空，断言红。
    #[tokio::test]
    async fn test_parent_looper_strips_hoisted_declarations() {
        let d1 = ArtifactDecl {
            path: "/tmp/p1.png".into(),
            title: Some("图一".into()),
            mime: Some("image/png".into()),
        };
        let d2 = ArtifactDecl {
            path: "/tmp/p2.mp4".into(),
            title: Some("视频".into()),
            mime: Some("video/mp4".into()),
        };
        let t = "收尾";
        let tool_output = with_artifact_lines(t, &[d1.clone(), d2.clone()]);

        let (agent, _provider) = scripted_agent_with_tools(
            vec![
                tool_call_response("call-1", "gen_artifact", json!({})),
                completed(t),
            ],
            vec![Box::new(StaticTool::new(
                "gen_artifact",
                Content::Text(tool_output),
            ))],
        );

        let out = SimpleAgentLooper::spawn(agent, "hi".into(), None)
            .wait()
            .await
            .expect("parent run should succeed");

        assert_eq!(out.text, t);
        assert_eq!(out.artifacts, vec![d1, d2]);
    }
}
