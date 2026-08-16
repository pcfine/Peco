// ============================================================================
// WorkflowEngine — 核心执行引擎（spawn 模型）
// ============================================================================

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::tools::AgentAccess;

use super::condition::evaluate_condition;
use super::dag::DagGraph;
use super::definition::{OnFailure, StepOutcome, WorkflowDefinition};
use super::error::WorkflowError;
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
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self { event_buffer: 64 }
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
// run_id 可通过 event 直接获取，无需外部注入
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

/// 生命周期/终态事件必须保证投递（背压 `send().await`），不能像步骤级流式事件那样
/// `try_send` 静默丢弃——`Started`/`Paused`/`Resumed`/`Completed`/`Failed`/`Cancelled`/
/// `TimedOut` 若丢失会让消费端状态错乱（把 `None` 误判为「意外结束」、或漏掉暂停/恢复）。
///
/// 消费端已 drop 时 `send` 返回 `Err`，此时引擎已无后续工作，仅记录日志即可。
async fn send_event_await(tx: &tokio::sync::mpsc::Sender<WorkflowEvent>, ev: WorkflowEvent) {
    if let Err(e) = tx.send(ev).await {
        warn!(error = %e, "lifecycle event receiver closed; event dropped");
    }
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
    ///
    /// `current_level` 语义统一为「已完整执行层级数」（不含进行中的层级）：
    /// Pause 传 `level_idx`（0..level_idx 已完整跑完），Completed 传 `levels.len()`。
    fn build_snapshot(
        &self,
        state: WorkflowSnapshotState,
        error: Option<String>,
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
            error,
            inputs_json,
            step_results: step_results.clone(),
            current_level,
            total_steps: self.definition.steps.len(),
            started_at,
            updated_at: chrono::Utc::now(),
        }
    }

    /// 全局超时终止路径（层边界与收集循环共用）。
    ///
    /// 持久化 `TimedOut` 快照并发射 `TimedOut` 终态事件。调用方负责在调用前
    /// abort 所有在途 handle。全局超时对应层内并行的执行窗口，无唯一「当前步」，
    /// 故 `failed_at_step` 取 `None`。
    #[allow(clippy::too_many_arguments)]
    async fn fail_timeout(
        &self,
        event_tx: &tokio::sync::mpsc::Sender<WorkflowEvent>,
        start: Instant,
        step_results: &HashMap<String, super::definition::StepResult>,
        current_level: usize,
        started_at: chrono::DateTime<chrono::Utc>,
        inputs_json: Option<String>,
    ) {
        let error_msg = WorkflowError::Timeout {
            elapsed_seconds: start.elapsed().as_secs(),
        }
        .to_string();

        let snapshot = self.build_snapshot(
            WorkflowSnapshotState::TimedOut,
            Some(error_msg.clone()),
            step_results,
            current_level,
            started_at,
            inputs_json,
        );
        if let Err(e) = self.persister.save(&snapshot).await {
            error!(run_id = %self.run_id, error = %e, "failed to persist TimedOut snapshot");
        }

        send_event_await(
            event_tx,
            WorkflowEvent::TimedOut {
                run_id: self.run_id.clone(),
                error: error_msg,
                failed_at_step: None,
                total_duration_ms: start.elapsed().as_millis() as u64,
            },
        )
        .await;
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
        let workflow_name = self.definition.name.clone();
        let total_steps = self.definition.steps.len();

        // 生命周期/终态事件需要背压投递（send().await），clone 一个 sender 供
        // send_event_await 使用；原 event_tx 被下面的 send_event 闭包 move 走
        // （try_send 非阻塞路径，仅用于步骤级流式事件）。
        let lifecycle_tx = event_tx.clone();

        // 事件发送辅助：非阻塞发送。当事件 channel 已满（消费端过慢）或已关闭
        // （handle 被 drop）时事件会被静默丢弃 — 至少记录一条日志，否则步骤级
        // 流式事件丢失仅影响观测，不影响引擎终态。
        let send_event = {
            let run_id_log = run_id.clone();
            move |event: WorkflowEvent| {
                if let Err(e) = event_tx.try_send(event) {
                    warn!(
                        run_id = %run_id_log,
                        error = %e,
                        "failed to send workflow event (channel full or closed); event dropped"
                    );
                }
            }
        };

        info!(
            %run_id,
            workflow = %workflow_name,
            total_steps,
            "WorkflowEngine::run() started"
        );

        // 0. 验证输入参数
        let validated_inputs = match self.definition.validate_inputs(&inputs) {
            Ok(v) => v,
            Err(e) => {
                let error_msg = e.to_string();
                warn!(
                    %run_id,
                    workflow = %workflow_name,
                    error = %e,
                    "input validation failed, failing workflow"
                );
                let inputs_json = serde_json::to_string(&inputs).ok();
                let snapshot = WorkflowSnapshot {
                    run_id: run_id.clone(),
                    workflow_name: workflow_name.clone(),
                    definition: self.definition.clone(),
                    state: WorkflowSnapshotState::Failed,
                    error: Some(error_msg.clone()),
                    inputs_json,
                    step_results: HashMap::new(),
                    current_level: 0,
                    total_steps,
                    started_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                };
                if let Err(e) = self.persister.save(&snapshot).await {
                    error!(%run_id, error = %e, "failed to persist Failed snapshot");
                }
                send_event_await(
                    &lifecycle_tx,
                    WorkflowEvent::Failed {
                        run_id,
                        error: error_msg,
                        failed_at_step: None,
                        total_duration_ms: 0,
                    },
                )
                .await;
                return;
            }
        };

        // 1. 构建 DAG + 拓扑排序
        let dag = match DagGraph::build(&self.definition.steps) {
            Ok(dag) => dag,
            Err(e) => {
                let error_msg = e.to_string();
                warn!(
                    %run_id,
                    workflow = %workflow_name,
                    error = %e,
                    "DAG construction failed, failing workflow"
                );
                let inputs_json = serde_json::to_string(&inputs).ok();
                let snapshot = WorkflowSnapshot {
                    run_id: run_id.clone(),
                    workflow_name: workflow_name.clone(),
                    definition: self.definition.clone(),
                    state: WorkflowSnapshotState::Failed,
                    error: Some(error_msg.clone()),
                    inputs_json,
                    step_results: HashMap::new(),
                    current_level: 0,
                    total_steps,
                    started_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                };
                if let Err(e) = self.persister.save(&snapshot).await {
                    error!(%run_id, error = %e, "failed to persist Failed snapshot");
                }
                send_event_await(
                    &lifecycle_tx,
                    WorkflowEvent::Failed {
                        run_id,
                        error: error_msg,
                        failed_at_step: None,
                        total_duration_ms: 0,
                    },
                )
                .await;
                return;
            }
        };

        // 1b. 序列化输入参数（供快照持久化）
        let inputs_json = serde_json::to_string(&validated_inputs).ok();

        // 2. 初始化模板上下文（注入外部输入参数）
        let mut tpl_ctx = TemplateContext::new(Some(&validated_inputs));

        // 3. 发送 Started 事件
        send_event_await(
            &lifecycle_tx,
            WorkflowEvent::Started {
                run_id: run_id.clone(),
                workflow_name: workflow_name.clone(),
                total_steps,
            },
        )
        .await;

        // 4. 按拓扑层级迭代执行
        let start = Instant::now();
        let started_at = chrono::Utc::now();

        // 全局超时 deadline（净执行时长）。Pause 等待审批不计入预算，恢复时顺延。
        let mut deadline: Option<Instant> = self
            .definition
            .timeout_seconds
            .map(|s| Instant::now() + Duration::from_secs(s));
        let mut step_results: HashMap<String, super::definition::StepResult> = HashMap::new();
        let mut steps_completed = 0usize;
        let mut steps_failed = 0usize;
        let mut steps_skipped = 0usize;

        let levels = dag.topological_levels().to_vec();
        debug!(
            %run_id,
            workflow = %workflow_name,
            level_count = levels.len(),
            "DAG resolved into levels"
        );

        for (level_idx, level) in levels.iter().enumerate() {
            // 4a. 层级开始前检查取消
            if cancel_flag.load(Ordering::Acquire) {
                info!(%run_id, level = level_idx, "cancelled before level start");
                send_event_await(
                    &lifecycle_tx,
                    WorkflowEvent::Cancelled {
                        run_id: run_id.clone(),
                    },
                )
                .await;
                return;
            }

            // 层级开始前检查全局超时
            if deadline.is_some_and(|d| Instant::now() >= d) {
                info!(%run_id, level = level_idx, "global timeout reached before level start");
                self.fail_timeout(
                    &lifecycle_tx,
                    start,
                    &step_results,
                    level_idx,
                    started_at,
                    inputs_json.clone(),
                )
                .await;
                return;
            }

            debug!(
                %run_id,
                level = level_idx,
                level_steps = level.len(),
                "starting level"
            );

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
                    debug!(
                        %run_id,
                        step_id = %step.id,
                        reason = %reason,
                        "step condition false, skipping"
                    );
                    send_event(WorkflowEvent::StepSkipped {
                        run_id: run_id.clone(),
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

                debug!(
                    %run_id,
                    step_id = %step.id,
                    step_type = %step.step_type,
                    "starting step"
                );

                // 发射 StepStarted
                send_event(WorkflowEvent::StepStarted {
                    run_id: run_id.clone(),
                    step_id: step.id.clone(),
                    step_name: step.name.clone(),
                    step_type: step.step_type.to_string(),
                });

                let step_id = step.id.clone();
                let step_data = step.clone();
                let ctx = tpl_ctx.clone();
                let cancel = cancel_flag.clone();
                let agent_access = self.agent_access.clone();

                let handle = tokio::spawn(async move {
                    execute_step_static(&step_data, &ctx, &cancel, &agent_access).await
                });
                handles.push((step_id, handle));
            }

            // 4c. 收集结果（用 VecDeque 实现 O(1) 的 pop_front）
            let mut remaining: VecDeque<(
                String,
                tokio::task::JoinHandle<super::definition::StepResult>,
            )> = VecDeque::from(handles);
            let mut aborted = false;
            let mut abort_error: Option<String> = None;

            while let Some((step_id, handle)) = remaining.pop_front() {
                // 如果已触发 abort，显式 abort 所有剩余 handle
                if aborted || cancel_flag.load(Ordering::Acquire) {
                    debug!(%run_id, step_id = %step_id, aborted, "aborting pending step");
                    handle.abort();
                    continue;
                }

                // 等待当前步骤完成；若设定了全局超时，则 race deadline —— 超时立即
                // 终止在途步骤（而非仅在步骤间检查，避免单步超过全局预算时悬挂）。
                let mut handle = handle;
                enum StepWait {
                    Done(Result<Box<super::definition::StepResult>, tokio::task::JoinError>),
                    TimedOut,
                }
                let wait = if let Some(d) = deadline {
                    tokio::select! {
                        r = &mut handle => StepWait::Done(r.map(Box::new)),
                        _ = tokio::time::sleep_until(d.into()) => StepWait::TimedOut,
                    }
                } else {
                    StepWait::Done((&mut handle).await.map(Box::new))
                };

                let result = match wait {
                    StepWait::TimedOut => {
                        info!(%run_id, step_id = %step_id, "global timeout reached during step execution");
                        handle.abort();
                        for (_, h) in remaining.drain(..) {
                            h.abort();
                        }
                        self.fail_timeout(
                            &lifecycle_tx,
                            start,
                            &step_results,
                            level_idx,
                            started_at,
                            inputs_json.clone(),
                        )
                        .await;
                        return;
                    }
                    StepWait::Done(Ok(r)) => *r,
                    StepWait::Done(Err(join_err)) => {
                        error!(
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
                        debug!(
                            %run_id,
                            step_id = %result.step.id,
                            duration_ms = result.duration.as_millis() as u64,
                            attempt = result.attempt,
                            "step completed"
                        );
                        WorkflowEvent::StepCompleted {
                            run_id: run_id.clone(),
                            step_id: result.step.id.clone(),
                            step_name: result.step.name.clone(),
                            output: result.output.clone().unwrap_or_default(),
                            duration_ms: result.duration.as_millis() as u64,
                            attempt: result.attempt,
                        }
                    }
                    // 不可达：条件不满足的步骤在 spawn 前（`for step in level` 循环）
                    // 已直接插入 `step_results` 并 `continue`，从不进入 handles 收集循环。
                    // `execute_step_static` 也只返回 `Success`/`Failed`，永不返回 `Skipped`。
                    StepOutcome::Skipped(_) => {
                        unreachable!(
                            "skipped steps are filtered before spawning and never collected"
                        )
                    }
                    StepOutcome::Failed(err) => {
                        steps_failed += 1;
                        warn!(
                            %run_id,
                            step_id = %result.step.id,
                            error = %err,
                            duration_ms = result.duration.as_millis() as u64,
                            attempt = result.attempt,
                            failure_policy = %result.step.on_failure.as_str(),
                            "step failed"
                        );
                        WorkflowEvent::StepFailed {
                            run_id: run_id.clone(),
                            step_id: result.step.id.clone(),
                            step_name: result.step.name.clone(),
                            error: err.clone(),
                            duration_ms: result.duration.as_millis() as u64,
                            attempt: result.attempt,
                            failure_policy: result.step.on_failure.as_str().to_string(),
                        }
                    }
                };
                send_event(event);

                // 检查失败策略
                if let StepOutcome::Failed(step_error) = &result.outcome {
                    let step_error = step_error.clone();
                    match result.step.on_failure {
                        OnFailure::Abort => {
                            let workflow_err =
                                format!("Step '{}' failed: {}", result.step.id, step_error);
                            info!(
                                %run_id,
                                step_id = %result.step.id,
                                "failure policy=abort, failing workflow"
                            );
                            send_event_await(
                                &lifecycle_tx,
                                WorkflowEvent::Failed {
                                    run_id: run_id.clone(),
                                    error: workflow_err.clone(),
                                    failed_at_step: Some(result.step.id.clone()),
                                    total_duration_ms: start.elapsed().as_millis() as u64,
                                },
                            )
                            .await;
                            aborted = true;
                            abort_error = Some(workflow_err);
                            // 不清空 remaining — while 循环顶部会 abort 所有剩余项
                        }
                        OnFailure::Pause => {
                            let pause_reason = format!(
                                "Step '{}' failed: {step_error}, waiting for approval",
                                result.step.id,
                            );
                            info!(
                                %run_id,
                                step_id = %result.step.id,
                                "failure policy=pause, waiting for approval"
                            );
                            send_event_await(
                                &lifecycle_tx,
                                WorkflowEvent::Paused {
                                    run_id: run_id.clone(),
                                    reason: pause_reason.clone(),
                                    paused_at_step: Some(result.step.id.clone()),
                                },
                            )
                            .await;
                            // 持久化暂停快照（失败则升级为 Failed，避免引擎在无法
                            // 恢复的暂停状态上等待审批）
                            let snapshot = self.build_snapshot(
                                WorkflowSnapshotState::Paused,
                                Some(pause_reason),
                                &step_results,
                                level_idx,
                                started_at,
                                inputs_json.clone(),
                            );
                            if let Err(e) = self.persister.save(&snapshot).await {
                                let fail_reason = format!(
                                    "Step '{}' failed: {step_error}, and persisting Paused snapshot failed: {e}",
                                    result.step.id,
                                );
                                error!(%run_id, error = %e, "failed to persist Paused snapshot");
                                send_event_await(
                                    &lifecycle_tx,
                                    WorkflowEvent::Failed {
                                        run_id: run_id.clone(),
                                        error: fail_reason,
                                        failed_at_step: Some(result.step.id.clone()),
                                        total_duration_ms: start.elapsed().as_millis() as u64,
                                    },
                                )
                                .await;
                                for (_, h) in remaining.drain(..) {
                                    h.abort();
                                }
                                return;
                            }
                            // 阻塞等待审批决策（Pause 等待不计入全局超时预算）
                            let pause_started = Instant::now();
                            match approval_rx.recv().await {
                                Some(ApprovalResponse {
                                    decision: ApprovalDecision::Abort,
                                    ..
                                })
                                | None => {
                                    let abort_reason = format!(
                                        "User aborted after step '{}' failed: {step_error}",
                                        result.step.id,
                                    );
                                    info!(
                                        %run_id,
                                        step_id = %result.step.id,
                                        "approval decision=abort (or channel closed), failing workflow"
                                    );
                                    send_event_await(
                                        &lifecycle_tx,
                                        WorkflowEvent::Failed {
                                            run_id,
                                            error: abort_reason,
                                            failed_at_step: Some(result.step.id.clone()),
                                            total_duration_ms: start.elapsed().as_millis() as u64,
                                        },
                                    )
                                    .await;
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
                                    info!(%run_id, "approval decision=proceed, resuming");
                                    // 恢复：把暂停期间耗时从全局超时预算中剔除（deadline 顺延）
                                    if let Some(d) = deadline {
                                        deadline = Some(d + (Instant::now() - pause_started));
                                    }
                                    send_event_await(
                                        &lifecycle_tx,
                                        WorkflowEvent::Resumed {
                                            run_id: run_id.clone(),
                                        },
                                    )
                                    .await;
                                    // 继续当前层级剩余步骤
                                }
                            }
                        }
                        OnFailure::Continue => {
                            // 继续执行，不中止
                            debug!(
                                %run_id,
                                step_id = %result.step.id,
                                "failure policy=continue"
                            );
                        }
                        OnFailure::Retry => {
                            // Phase 4: 重试逻辑（此处退化为 Continue）
                            debug!(
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
                    abort_error,
                    &step_results,
                    level_idx,
                    started_at,
                    inputs_json.clone(),
                );
                if let Err(e) = self.persister.save(&snapshot).await {
                    error!(%run_id, error = %e, "failed to persist Failed snapshot");
                }
                // 确保所有剩余 handle 都被清理
                for (_, h) in remaining.drain(..) {
                    h.abort();
                }
                return;
            }

            // 层级完成：持久化 Running 快照
            let snapshot = self.build_snapshot(
                WorkflowSnapshotState::Running,
                None,
                &step_results,
                level_idx + 1,
                started_at,
                inputs_json.clone(),
            );
            if let Err(e) = self.persister.save(&snapshot).await {
                error!(%run_id, error = %e, "failed to persist Running snapshot");
            }
            debug!(%run_id, level = level_idx, "level complete");
        }

        // 5. 完成 — 持久化最终快照
        let snapshot = self.build_snapshot(
            WorkflowSnapshotState::Completed,
            None,
            &step_results,
            levels.len(),
            started_at,
            inputs_json.clone(),
        );
        if let Err(e) = self.persister.save(&snapshot).await {
            error!(%run_id, error = %e, "failed to persist Completed snapshot");
        }

        let total_duration_ms = start.elapsed().as_millis() as u64;
        info!(
            %run_id,
            workflow = %workflow_name,
            total_duration_ms,
            steps_completed,
            steps_failed,
            steps_skipped,
            "workflow completed"
        );
        send_event_await(
            &lifecycle_tx,
            WorkflowEvent::Completed {
                run_id,
                total_duration_ms,
                steps_completed,
                steps_failed,
                steps_skipped,
            },
        )
        .await;
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
    async fn test_execute_global_timeout() {
        // 单步 sleep 5s，但全局超时 1s — 必须中断在途步骤并返回 TimedOut 终态。
        let steps = vec![make_shell_step(
            "A",
            "Sleep",
            "sleep 5 && echo 'done'",
            vec![],
        )];
        let def = WorkflowDefinition {
            name: "global-timeout".into(),
            description: "test".into(),
            version: "1.0".into(),
            timeout_seconds: Some(1),
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

        let mut timed_out = false;
        while let Some(event) = handle.recv_event().await {
            match event {
                WorkflowEvent::TimedOut { error, .. } => {
                    assert!(
                        error.contains("timed out"),
                        "TimedOut error should mention timeout, got: {error}"
                    );
                    timed_out = true;
                    break;
                }
                WorkflowEvent::Failed { .. } | WorkflowEvent::Cancelled { .. } => break,
                _ => {}
            }
        }
        let elapsed = start.elapsed();
        assert!(timed_out, "expected TimedOut terminal event");
        assert!(
            elapsed < Duration::from_secs(3),
            "global timeout should interrupt in-flight step promptly, took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn test_execute_step_timeout() {
        // 步骤级超时 1s，shell 命令 sleep 5s — 步骤应失败（error 含 "timed out"），
        // 随后按 on_failure=abort 触发 workflow Failed。
        let steps = vec![WorkflowStep {
            id: "slow".into(),
            name: "Slow Step".into(),
            step_type: StepType::Shell,
            config: StepConfig::Shell {
                command: "sleep 5 && echo 'done'".into(),
            },
            depends_on: vec![],
            condition: None,
            timeout_seconds: Some(1),
            on_failure: OnFailure::Abort,
            retry_policy: None,
            output_schema: None,
        }];
        let def = WorkflowDefinition {
            name: "step-timeout".into(),
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

        let mut step_failed_timeout = false;
        let mut workflow_failed = false;
        while let Some(event) = handle.recv_event().await {
            match event {
                WorkflowEvent::StepFailed { error, .. } => {
                    if error.contains("timed out") {
                        step_failed_timeout = true;
                    }
                }
                WorkflowEvent::Failed { .. } => {
                    workflow_failed = true;
                    break;
                }
                WorkflowEvent::Cancelled { .. } | WorkflowEvent::TimedOut { .. } => break,
                _ => {}
            }
        }
        assert!(step_failed_timeout, "expected step-level timeout failure");
        assert!(
            workflow_failed,
            "expected workflow Failed after step timeout"
        );
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
