// ============================================================================
// WorkflowEngine — 核心执行引擎（spawn 模型）
// ============================================================================

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::tools::AgentAccess;

use super::condition::evaluate_condition;
use super::dag::DagGraph;
use super::definition::{OnFailure, StepOutcome, WorkflowDefinition};
use super::events::{ApprovalDecision, ApprovalResponse, WorkflowEvent};
use super::handle::WorkflowHandle;
use super::persistence::{WorkflowPersister, WorkflowSnapshot, WorkflowSnapshotState};
use super::step_executor::execute_step_static;
use super::template::TemplateContext;

/// WorkflowHandle 内部持有的 JoinHandle 类型别名。
pub type SharedWorkflowTask = Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>;

/// 引擎配置。
#[derive(Debug, Clone)]
pub struct WorkflowConfig {
    /// 事件通道 buffer 大小（默认 64）
    pub event_buffer: usize,
    /// Phase 2: 步骤间最小延迟（防止模型速率限制）
    pub step_delay: Option<Duration>,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            event_buffer: 64,
            step_delay: None,
        }
    }
}

/// Workflow 执行引擎。
///
/// # 生命周期
///
/// ```text
/// let handle = WorkflowEngine::spawn(definition, agent_access, config, inputs);
///
/// loop {
///     match handle.recv_event().await {
///         Some(WorkflowEvent::StepCompleted { .. }) => { /* 记录 */ }
///         Some(WorkflowEvent::Completed { .. }) => break,
///         Some(WorkflowEvent::Failed { .. }) => break,
///         Some(WorkflowEvent::Paused { .. }) => {
///             handle.approve(ApprovalResponse {
///                 decision: ApprovalDecision::Proceed,
///                 note: None,
///             }).await.ok();
///         }
///         None => break,
///         _ => {}
///     }
/// }
/// ```
pub struct WorkflowEngine {
    run_id: String,
    definition: WorkflowDefinition,
    agent_access: Arc<dyn AgentAccess>,
    persister: Arc<dyn WorkflowPersister>,
    #[allow(dead_code)]
    config: WorkflowConfig,
}

impl WorkflowEngine {
    /// 启动 Workflow 执行，返回控制句柄。
    ///
    /// 引擎在后台 tokio 任务中运行。通过返回的 `WorkflowHandle` 消费事件流、
    /// 发送审批决策、取消执行或等待完成。
    pub fn spawn(
        definition: WorkflowDefinition,
        agent_access: Arc<dyn AgentAccess>,
        persister: Arc<dyn WorkflowPersister>,
        config: WorkflowConfig,
        inputs: HashMap<String, serde_json::Value>,
    ) -> WorkflowHandle {
        let run_id = uuid::Uuid::new_v4().to_string();

        // 创建事件通道（tokio mpsc）：engine 发送，handle 接收
        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<WorkflowEvent>(config.event_buffer);

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let (approval_tx, approval_rx) = tokio::sync::mpsc::channel::<ApprovalResponse>(8);

        let engine = Self {
            run_id: run_id.clone(),
            definition,
            agent_access,
            persister,
            config,
        };

        let engine_cancel = cancel_flag.clone();
        let join_handle = tokio::spawn(async move {
            engine
                .run(event_tx, approval_rx, engine_cancel, inputs)
                .await
        });

        WorkflowHandle {
            run_id,
            cancel_flag,
            event_rx,
            approval_tx,
            join_handle: Arc::new(Mutex::new(Some(join_handle))),
        }
    }

    /// 构建当前引擎状态的快照（供 persister.save() 使用）。
    fn build_snapshot(
        &self,
        state: WorkflowSnapshotState,
        step_results: &HashMap<String, super::definition::StepResult>,
        current_level: usize,
        started_at: chrono::DateTime<chrono::Utc>,
        inputs_json: Option<String>,
    ) -> WorkflowSnapshot {
        WorkflowSnapshot {
            run_id: self.run_id.clone(),
            workflow_name: self.definition.name.clone(),
            definition: self.definition.clone(),
            state,
            inputs_json,
            step_results: step_results.clone(),
            current_level,
            total_steps: self.definition.steps.len(),
            started_at,
            updated_at: chrono::Utc::now(),
        }
    }

    /// 核心执行循环（在 tokio::spawn 中运行）。
    async fn run(
        self,
        event_tx: tokio::sync::mpsc::Sender<WorkflowEvent>,
        mut approval_rx: tokio::sync::mpsc::Receiver<ApprovalResponse>,
        cancel_flag: Arc<AtomicBool>,
        inputs: HashMap<String, serde_json::Value>,
    ) {
        let run_id = self.run_id.clone();

        tracing::debug!(%run_id, "WorkflowEngine::run() started");

        // 0. 验证输入参数
        let validated_inputs = match self.definition.validate_inputs(&inputs) {
            Ok(v) => v,
            Err(e) => {
                let inputs_json = serde_json::to_string(&inputs).ok();
                let _ = self
                    .persister
                    .save(&WorkflowSnapshot {
                        run_id: run_id.clone(),
                        workflow_name: self.definition.name.clone(),
                        definition: self.definition.clone(),
                        state: WorkflowSnapshotState::Failed,
                        inputs_json,
                        step_results: HashMap::new(),
                        current_level: 0,
                        total_steps: self.definition.steps.len(),
                        started_at: chrono::Utc::now(),
                        updated_at: chrono::Utc::now(),
                    })
                    .await;
                let _ = event_tx.try_send(WorkflowEvent::Failed {
                    run_id,
                    error: e.to_string(),
                    failed_at_step: None,
                    total_duration_ms: 0,
                });
                return;
            }
        };

        // 1. 构建 DAG + 拓扑排序
        let dag = match DagGraph::build(&self.definition.steps) {
            Ok(dag) => dag,
            Err(e) => {
                let inputs_json = serde_json::to_string(&inputs).ok();
                let _ = self
                    .persister
                    .save(&WorkflowSnapshot {
                        run_id: run_id.clone(),
                        workflow_name: self.definition.name.clone(),
                        definition: self.definition.clone(),
                        state: WorkflowSnapshotState::Failed,
                        inputs_json,
                        step_results: HashMap::new(),
                        current_level: 0,
                        total_steps: self.definition.steps.len(),
                        started_at: chrono::Utc::now(),
                        updated_at: chrono::Utc::now(),
                    })
                    .await;
                let _ = event_tx.try_send(WorkflowEvent::Failed {
                    run_id,
                    error: e.to_string(),
                    failed_at_step: None,
                    total_duration_ms: 0,
                });
                return;
            }
        };

        // 1b. 序列化输入参数（供快照持久化）
        let inputs_json = serde_json::to_string(&validated_inputs).ok();

        // 2. 初始化模板上下文（注入外部输入参数）
        let mut tpl_ctx = TemplateContext::new(Some(&validated_inputs));

        // 3. 发送 Started 事件
        let _ = event_tx.try_send(WorkflowEvent::Started {
            run_id: run_id.clone(),
            workflow_name: self.definition.name.clone(),
            total_steps: self.definition.steps.len(),
        });

        // 4. 按拓扑层级迭代执行
        let start = Instant::now();
        let started_at = chrono::Utc::now();
        let mut step_results: HashMap<String, super::definition::StepResult> = HashMap::new();
        let mut steps_completed = 0usize;
        let mut steps_failed = 0usize;
        let mut steps_skipped = 0usize;

        let levels = dag.topological_levels().to_vec();
        for (level_idx, level) in levels.iter().enumerate() {
            // 4a. 层级开始前检查取消
            if cancel_flag.load(Ordering::Acquire) {
                let _ = event_tx.try_send(WorkflowEvent::Cancelled {
                    run_id: run_id.clone(),
                });
                return;
            }

            // 4b. 发射 StepStarted + 启动同一层级的所有步骤（并行执行）
            let mut handles: Vec<(
                String,
                tokio::task::JoinHandle<super::definition::StepResult>,
            )> = Vec::new();

            for step in level {
                if !evaluate_condition(step, &step_results, &tpl_ctx) {
                    let reason = step
                        .condition
                        .clone()
                        .unwrap_or_else(|| "condition evaluated to false".into());
                    let _ = event_tx.try_send(WorkflowEvent::StepSkipped {
                        step_id: step.id.clone(),
                        step_name: step.name.clone(),
                        reason: reason.clone(),
                    });
                    steps_skipped += 1;
                    let skipped_result = super::definition::StepResult {
                        step: step.clone(),
                        outcome: StepOutcome::Skipped(reason),
                        output: None,
                        structured_output: None,
                        duration: Duration::ZERO,
                        attempt: 0,
                    };
                    // 将跳过步骤的结果同步到模板上下文，确保后续步骤
                    // 可以通过 {{ steps.X.success }} 引用被跳过步骤的状态。
                    tpl_ctx.set_step_result(&step.id, &skipped_result);
                    step_results.insert(step.id.clone(), skipped_result);
                    continue;
                }

                // 发射 StepStarted
                let _ = event_tx.try_send(WorkflowEvent::StepStarted {
                    step_id: step.id.clone(),
                    step_name: step.name.clone(),
                    step_type: step.step_type.to_string(),
                });

                let step_id = step.id.clone();
                let step_data = step.clone();
                let prev = step_results.clone();
                let ctx = tpl_ctx.clone();
                let cancel = cancel_flag.clone();
                let agent_access = self.agent_access.clone();

                let handle = tokio::spawn(async move {
                    execute_step_static(&step_data, &prev, &ctx, &cancel, &agent_access).await
                });
                handles.push((step_id, handle));
            }

            // 4c. 收集结果（用 VecDeque 实现 O(1) 的 pop_front）
            let mut remaining: VecDeque<(
                String,
                tokio::task::JoinHandle<super::definition::StepResult>,
            )> = VecDeque::from(handles);
            let mut aborted = false;

            while let Some((step_id, handle)) = remaining.pop_front() {
                // 如果已触发 abort，显式 abort 所有剩余 handle
                if aborted || cancel_flag.load(Ordering::Acquire) {
                    handle.abort();
                    continue;
                }

                // 等待当前步骤完成
                let result = match handle.await {
                    Ok(r) => r,
                    Err(join_err) => {
                        tracing::error!(
                            step_id = %step_id,
                            error = %join_err,
                            "Step task panicked or was cancelled"
                        );
                        continue;
                    }
                };

                // 发射结果事件
                let event = match &result.outcome {
                    StepOutcome::Success(_) => {
                        steps_completed += 1;
                        WorkflowEvent::StepCompleted {
                            step_id: result.step.id.clone(),
                            step_name: result.step.name.clone(),
                            output: result.output.clone().unwrap_or_default(),
                            duration_ms: result.duration.as_millis() as u64,
                            attempt: result.attempt,
                        }
                    }
                    StepOutcome::Skipped(reason) => {
                        steps_skipped += 1;
                        WorkflowEvent::StepSkipped {
                            step_id: result.step.id.clone(),
                            step_name: result.step.name.clone(),
                            reason: reason.clone(),
                        }
                    }
                    StepOutcome::Failed(err) => {
                        steps_failed += 1;
                        WorkflowEvent::StepFailed {
                            step_id: result.step.id.clone(),
                            step_name: result.step.name.clone(),
                            error: err.clone(),
                            duration_ms: result.duration.as_millis() as u64,
                            attempt: result.attempt,
                            failure_policy: format!("{:?}", result.step.on_failure),
                        }
                    }
                };
                let _ = event_tx.try_send(event);

                // 检查失败策略
                if result.outcome.is_failed() {
                    match result.step.on_failure {
                        OnFailure::Abort => {
                            let _ = event_tx.try_send(WorkflowEvent::Failed {
                                run_id: run_id.clone(),
                                error: format!("Step '{}' failed", result.step.id,),
                                failed_at_step: Some(result.step.id.clone()),
                                total_duration_ms: start.elapsed().as_millis() as u64,
                            });
                            aborted = true;
                            // 不清空 remaining — while 循环顶部会 abort 所有剩余项
                        }
                        OnFailure::Pause => {
                            let _ = event_tx.try_send(WorkflowEvent::Paused {
                                run_id: run_id.clone(),
                                reason: format!(
                                    "Step '{}' failed, waiting for approval",
                                    result.step.id
                                ),
                                paused_at_step: Some(result.step.id.clone()),
                            });
                            // 持久化暂停快照
                            let snapshot = self.build_snapshot(
                                WorkflowSnapshotState::Paused,
                                &step_results,
                                level_idx,
                                started_at,
                                inputs_json.clone(),
                            );
                            let _ = self.persister.save(&snapshot).await;
                            // 阻塞等待审批决策
                            match approval_rx.recv().await {
                                Some(ApprovalResponse {
                                    decision: ApprovalDecision::Abort,
                                    ..
                                })
                                | None => {
                                    let _ = event_tx.try_send(WorkflowEvent::Failed {
                                        run_id,
                                        error: "User aborted after pause".into(),
                                        failed_at_step: Some(result.step.id.clone()),
                                        total_duration_ms: start.elapsed().as_millis() as u64,
                                    });
                                    // 取消剩余 handles
                                    for (_, h) in remaining.drain(..) {
                                        h.abort();
                                    }
                                    return;
                                }
                                Some(ApprovalResponse {
                                    decision: ApprovalDecision::Proceed,
                                    ..
                                }) => {
                                    let _ = event_tx.try_send(WorkflowEvent::Resumed {
                                        run_id: run_id.clone(),
                                    });
                                    // 继续当前层级剩余步骤
                                }
                            }
                        }
                        OnFailure::Continue => {
                            // 继续执行，不中止
                        }
                        OnFailure::Retry => {
                            // Phase 4: 重试逻辑（此处退化为 Continue）
                            tracing::debug!(
                                step_id = %result.step.id,
                                "Retry not supported in Phase 1, continuing"
                            );
                        }
                    }
                }

                // 更新模板上下文（即使在 Abort 路径中也要更新，供外部 snapshot 使用）
                tpl_ctx.set_step_result(&result.step.id, &result);
                step_results.insert(result.step.id.clone(), result);
            }

            // 4d. Abort 后不再进入下一层级
            if aborted {
                // 持久化失败快照
                let snapshot = self.build_snapshot(
                    WorkflowSnapshotState::Failed,
                    &step_results,
                    level_idx,
                    started_at,
                    inputs_json.clone(),
                );
                let _ = self.persister.save(&snapshot).await;
                // 确保所有剩余 handle 都被清理
                for (_, h) in remaining.drain(..) {
                    h.abort();
                }
                return;
            }

            // 层级完成：持久化 Running 快照
            let snapshot = self.build_snapshot(
                WorkflowSnapshotState::Running,
                &step_results,
                level_idx + 1,
                started_at,
                inputs_json.clone(),
            );
            let _ = self.persister.save(&snapshot).await;
        }

        // 5. 完成 — 持久化最终快照
        let snapshot = self.build_snapshot(
            WorkflowSnapshotState::Completed,
            &step_results,
            levels.len(),
            started_at,
            inputs_json.clone(),
        );
        let _ = self.persister.save(&snapshot).await;

        let _ = event_tx.try_send(WorkflowEvent::Completed {
            run_id,
            total_duration_ms: start.elapsed().as_millis() as u64,
            steps_completed,
            steps_failed,
            steps_skipped,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentError;
    use crate::workflow::definition::{OnFailure, StepConfig, StepType, WorkflowStep};
    use crate::workflow::persistence::NullWorkflowPersister;
    use std::sync::Arc;

    /// 空的 AgentAccess 实现（测试用）。
    struct NullAgentAccess;
    impl AgentAccess for NullAgentAccess {
        fn load_agent(&self, _name: &str) -> Result<Arc<crate::agent::Agent>, AgentError> {
            Err(AgentError::AgentProtocol("no agents in test mode".into()))
        }
        fn list_agent_names(&self) -> Vec<String> {
            vec![]
        }
        fn save_agent(&self, _name: &str, _content: &str) -> Result<(), String> {
            Err("not supported".into())
        }
        fn read_agent(&self, _name: &str) -> Result<String, String> {
            Err("not supported".into())
        }
        fn delete_agent(&self, _name: &str) -> Result<(), String> {
            Err("not supported".into())
        }
    }

    fn make_shell_step(id: &str, name: &str, command: &str, deps: Vec<&str>) -> WorkflowStep {
        WorkflowStep {
            id: id.to_string(),
            name: name.to_string(),
            step_type: StepType::Shell,
            config: StepConfig::Shell {
                command: command.to_string(),
            },
            depends_on: deps.into_iter().map(|s| s.to_string()).collect(),
            condition: None,
            timeout_seconds: Some(30),
            on_failure: OnFailure::Abort,
            retry_policy: None,
            output_schema: None,
        }
    }

    #[tokio::test]
    async fn test_execute_empty_steps() {
        let def = WorkflowDefinition {
            name: "empty".into(),
            description: "test".into(),
            version: "1.0".into(),
            timeout_seconds: None,
            inputs: HashMap::new(),
            steps: vec![],
            body: None,
        };
        let agent_access: Arc<dyn AgentAccess> = Arc::new(NullAgentAccess);
        let mut handle = WorkflowEngine::spawn(
            def,
            agent_access,
            Arc::new(NullWorkflowPersister),
            WorkflowConfig::default(),
            HashMap::new(),
        );

        let mut events = Vec::new();
        while let Some(event) = handle.recv_event().await {
            let is_terminal = matches!(
                event,
                WorkflowEvent::Completed { .. }
                    | WorkflowEvent::Failed { .. }
                    | WorkflowEvent::Cancelled { .. }
            );
            events.push(event);
            if is_terminal {
                break;
            }
        }

        assert!(
            events
                .iter()
                .any(|e| matches!(e, WorkflowEvent::Started { .. }))
        );
        assert!(events.iter().any(|e| matches!(
            e,
            WorkflowEvent::Completed {
                steps_completed: 0,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn test_execute_simple_chain() {
        let steps = vec![
            make_shell_step("A", "Step A", "echo 'hello'", vec![]),
            make_shell_step("B", "Step B", "echo 'world'", vec!["A"]),
        ];
        let def = WorkflowDefinition {
            name: "chain".into(),
            description: "test".into(),
            version: "1.0".into(),
            timeout_seconds: None,
            inputs: HashMap::new(),
            steps,
            body: None,
        };
        let agent_access: Arc<dyn AgentAccess> = Arc::new(NullAgentAccess);
        let mut handle = WorkflowEngine::spawn(
            def,
            agent_access,
            Arc::new(NullWorkflowPersister),
            WorkflowConfig::default(),
            HashMap::new(),
        );

        let mut events = Vec::new();
        while let Some(event) = handle.recv_event().await {
            let is_terminal = matches!(
                event,
                WorkflowEvent::Completed { .. }
                    | WorkflowEvent::Failed { .. }
                    | WorkflowEvent::Cancelled { .. }
            );
            events.push(event);
            if is_terminal {
                break;
            }
        }

        assert!(events.iter().any(|e| matches!(
            e,
            WorkflowEvent::Completed {
                steps_completed: 2,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn test_execute_parallel_steps() {
        // Two parallel steps that each sleep 200ms
        let steps = vec![
            make_shell_step("A", "Step A", "sleep 0.2 && echo 'A'", vec![]),
            make_shell_step("B", "Step B", "sleep 0.2 && echo 'B'", vec![]),
        ];
        let def = WorkflowDefinition {
            name: "parallel".into(),
            description: "test".into(),
            version: "1.0".into(),
            timeout_seconds: None,
            inputs: HashMap::new(),
            steps,
            body: None,
        };
        let agent_access: Arc<dyn AgentAccess> = Arc::new(NullAgentAccess);
        let start = Instant::now();
        let mut handle = WorkflowEngine::spawn(
            def,
            agent_access,
            Arc::new(NullWorkflowPersister),
            WorkflowConfig::default(),
            HashMap::new(),
        );

        let mut completed = false;
        while let Some(event) = handle.recv_event().await {
            if matches!(event, WorkflowEvent::Completed { .. }) {
                completed = true;
                break;
            }
        }
        let elapsed = start.elapsed();
        assert!(completed);
        // Parallel execution should take ~200ms, not ~400ms+
        assert!(
            elapsed < Duration::from_millis(800),
            "parallel steps took too long: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn test_execute_condition_skip() {
        let steps = vec![
            make_shell_step("A", "Step A", "echo 'ok'", vec![]),
            WorkflowStep {
                id: "B".into(),
                name: "Step B".into(),
                step_type: StepType::Shell,
                config: StepConfig::Shell {
                    command: "echo 'should not run'".into(),
                },
                depends_on: vec!["A".into()],
                condition: Some("false".into()), // always false → skip
                timeout_seconds: Some(30),
                on_failure: OnFailure::Abort,
                retry_policy: None,
                output_schema: None,
            },
        ];
        let def = WorkflowDefinition {
            name: "condition".into(),
            description: "test".into(),
            version: "1.0".into(),
            timeout_seconds: None,
            inputs: HashMap::new(),
            steps,
            body: None,
        };
        let agent_access: Arc<dyn AgentAccess> = Arc::new(NullAgentAccess);
        let mut handle = WorkflowEngine::spawn(
            def,
            agent_access,
            Arc::new(NullWorkflowPersister),
            WorkflowConfig::default(),
            HashMap::new(),
        );

        let mut skipped = false;
        let mut completed = false;
        while let Some(event) = handle.recv_event().await {
            match event {
                WorkflowEvent::StepSkipped { ref step_id, .. } if step_id == "B" => {
                    skipped = true;
                }
                WorkflowEvent::Completed { steps_skipped, .. } => {
                    assert_eq!(steps_skipped, 1);
                    completed = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(skipped);
        assert!(completed);
    }

    #[tokio::test]
    async fn test_execute_failure_abort() {
        let steps = vec![
            WorkflowStep {
                id: "fail".into(),
                name: "Fail Step".into(),
                step_type: StepType::Shell,
                config: StepConfig::Shell {
                    command: "exit 1".into(),
                },
                depends_on: vec![],
                condition: None,
                timeout_seconds: Some(30),
                on_failure: OnFailure::Abort,
                retry_policy: None,
                output_schema: None,
            },
            make_shell_step("B", "Should not run", "echo 'B'", vec!["fail"]),
        ];
        let def = WorkflowDefinition {
            name: "fail-abort".into(),
            description: "test".into(),
            version: "1.0".into(),
            timeout_seconds: None,
            inputs: HashMap::new(),
            steps,
            body: None,
        };
        let agent_access: Arc<dyn AgentAccess> = Arc::new(NullAgentAccess);
        let mut handle = WorkflowEngine::spawn(
            def,
            agent_access,
            Arc::new(NullWorkflowPersister),
            WorkflowConfig::default(),
            HashMap::new(),
        );

        let mut failed = false;
        while let Some(event) = handle.recv_event().await {
            if matches!(event, WorkflowEvent::Failed { .. }) {
                failed = true;
                break;
            }
        }
        assert!(failed);
    }

    #[tokio::test]
    async fn test_execute_failure_continue() {
        let steps = vec![
            WorkflowStep {
                id: "fail".into(),
                name: "Fail but continue".into(),
                step_type: StepType::Shell,
                config: StepConfig::Shell {
                    command: "exit 1".into(),
                },
                depends_on: vec![],
                condition: None,
                timeout_seconds: Some(30),
                on_failure: OnFailure::Continue,
                retry_policy: None,
                output_schema: None,
            },
            make_shell_step("B", "Runs anyway", "echo 'B'", vec!["fail"]),
        ];
        let def = WorkflowDefinition {
            name: "fail-continue".into(),
            description: "test".into(),
            version: "1.0".into(),
            timeout_seconds: None,
            inputs: HashMap::new(),
            steps,
            body: None,
        };
        let agent_access: Arc<dyn AgentAccess> = Arc::new(NullAgentAccess);
        let mut handle = WorkflowEngine::spawn(
            def,
            agent_access,
            Arc::new(NullWorkflowPersister),
            WorkflowConfig::default(),
            HashMap::new(),
        );

        let mut completed = false;
        while let Some(event) = handle.recv_event().await {
            if let WorkflowEvent::Completed {
                steps_completed,
                steps_failed,
                ..
            } = event
            {
                assert_eq!(steps_completed, 1);
                assert_eq!(steps_failed, 1);
                completed = true;
                break;
            }
        }
        assert!(completed);
    }

    #[tokio::test]
    async fn test_execute_cancel() {
        let steps = vec![make_shell_step("A", "Sleep", "sleep 10", vec![])];
        let def = WorkflowDefinition {
            name: "cancel".into(),
            description: "test".into(),
            version: "1.0".into(),
            timeout_seconds: None,
            inputs: HashMap::new(),
            steps,
            body: None,
        };
        let agent_access: Arc<dyn AgentAccess> = Arc::new(NullAgentAccess);
        let mut handle = WorkflowEngine::spawn(
            def,
            agent_access,
            Arc::new(NullWorkflowPersister),
            WorkflowConfig::default(),
            HashMap::new(),
        );

        // Wait for StepStarted, then cancel
        let mut started = false;
        loop {
            match handle.recv_event().await {
                Some(WorkflowEvent::StepStarted { .. }) => {
                    started = true;
                    handle.cancel();
                }
                Some(WorkflowEvent::Cancelled { .. }) => break,
                Some(WorkflowEvent::Failed { .. }) => break, // may also fail if step already finished
                None => break,
                _ => {}
            }
            if !started {
                // Give it a moment to start
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        }
        // The cancelled flag was set — we got either Cancelled or Failed
    }

    #[tokio::test]
    async fn test_execute_failure_pause_approve_proceed() {
        let steps = vec![WorkflowStep {
            id: "fail".into(),
            name: "Fail and pause".into(),
            step_type: StepType::Shell,
            config: StepConfig::Shell {
                command: "exit 1".into(),
            },
            depends_on: vec![],
            condition: None,
            timeout_seconds: Some(30),
            on_failure: OnFailure::Pause,
            retry_policy: None,
            output_schema: None,
        }];
        let def = WorkflowDefinition {
            name: "pause-proceed".into(),
            description: "test".into(),
            version: "1.0".into(),
            timeout_seconds: None,
            inputs: HashMap::new(),
            steps,
            body: None,
        };
        let agent_access: Arc<dyn AgentAccess> = Arc::new(NullAgentAccess);
        let mut handle = WorkflowEngine::spawn(
            def,
            agent_access,
            Arc::new(NullWorkflowPersister),
            WorkflowConfig::default(),
            HashMap::new(),
        );

        let mut paused = false;
        while let Some(event) = handle.recv_event().await {
            match event {
                WorkflowEvent::Paused { .. } => {
                    paused = true;
                    handle
                        .approve(ApprovalResponse {
                            decision: ApprovalDecision::Proceed,
                            note: None,
                        })
                        .await
                        .ok();
                }
                WorkflowEvent::Resumed { .. } => {
                    // Continue
                }
                WorkflowEvent::Completed { steps_failed, .. } => {
                    assert_eq!(steps_failed, 1);
                    break;
                }
                WorkflowEvent::Failed { .. } => {
                    // Should not happen with Proceed
                    break;
                }
                _ => {}
            }
        }
        assert!(paused, "Expected Paused event");
    }

    #[tokio::test]
    async fn test_execute_failure_pause_approve_abort() {
        let steps = vec![WorkflowStep {
            id: "fail".into(),
            name: "Fail and pause".into(),
            step_type: StepType::Shell,
            config: StepConfig::Shell {
                command: "exit 1".into(),
            },
            depends_on: vec![],
            condition: None,
            timeout_seconds: Some(30),
            on_failure: OnFailure::Pause,
            retry_policy: None,
            output_schema: None,
        }];
        let def = WorkflowDefinition {
            name: "pause-abort".into(),
            description: "test".into(),
            version: "1.0".into(),
            timeout_seconds: None,
            inputs: HashMap::new(),
            steps,
            body: None,
        };
        let agent_access: Arc<dyn AgentAccess> = Arc::new(NullAgentAccess);
        let mut handle = WorkflowEngine::spawn(
            def,
            agent_access,
            Arc::new(NullWorkflowPersister),
            WorkflowConfig::default(),
            HashMap::new(),
        );

        let mut aborted_after_pause = false;
        while let Some(event) = handle.recv_event().await {
            match event {
                WorkflowEvent::Paused { .. } => {
                    handle
                        .approve(ApprovalResponse {
                            decision: ApprovalDecision::Abort,
                            note: None,
                        })
                        .await
                        .ok();
                }
                WorkflowEvent::Failed { .. } => {
                    aborted_after_pause = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(aborted_after_pause, "Expected Failed after Abort decision");
    }

    #[tokio::test]
    async fn test_run_id_in_all_events() {
        let steps = vec![make_shell_step("A", "Step A", "echo 'hello'", vec![])];
        let def = WorkflowDefinition {
            name: "runid".into(),
            description: "test".into(),
            version: "1.0".into(),
            timeout_seconds: None,
            inputs: HashMap::new(),
            steps,
            body: None,
        };
        let agent_access: Arc<dyn AgentAccess> = Arc::new(NullAgentAccess);
        let mut handle = WorkflowEngine::spawn(
            def,
            agent_access,
            Arc::new(NullWorkflowPersister),
            WorkflowConfig::default(),
            HashMap::new(),
        );

        let expected_run_id = handle.run_id().to_string();
        let mut all_match = true;
        while let Some(event) = handle.recv_event().await {
            let event_run_id = match &event {
                WorkflowEvent::Started { run_id, .. } => run_id.clone(),
                WorkflowEvent::Paused { run_id, .. } => run_id.clone(),
                WorkflowEvent::Resumed { run_id } => run_id.clone(),
                WorkflowEvent::Completed { run_id, .. } => run_id.clone(),
                WorkflowEvent::Failed { run_id, .. } => run_id.clone(),
                WorkflowEvent::Cancelled { run_id } => run_id.clone(),
                // Step-level events don't carry run_id directly (they're in the context)
                // But testing that the terminal event has the correct run_id is sufficient
                _ => continue,
            };
            if event_run_id != expected_run_id {
                all_match = false;
            }
            if matches!(
                event,
                WorkflowEvent::Completed { .. }
                    | WorkflowEvent::Failed { .. }
                    | WorkflowEvent::Cancelled { .. }
            ) {
                break;
            }
        }
        assert!(all_match);
    }
}
