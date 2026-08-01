//! Peco Workflow demo — 完整展示声明式 DAG 工作流编排。
//!
//! 演示的功能：
//! 1. 从 YAML 定义工作流（Shell 步骤 + DAG 拓扑）
//! 2. 串行/并行步骤执行
//! 3. 模板变量传递（minijinja：{{ steps.X.output }}）
//! 4. 条件分支（condition：根据上一步成败决定是否执行）
//! 5. 失败策略（on_failure: continue 不中止）
//! 6. WorkflowManager 生命周期管理
//! 7. ExecuteWorkflow 工具（Agent 可调用 Workflow）
//!
//! ## Running
//!
//! ```sh
//! cargo run -p peco-core --example workflow_demo
//! ```
//!
//! Or with logging:
//!
//! ```sh
//! RUST_LOG=debug cargo run -p peco-core --example workflow_demo
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use peco_core::agent::{Agent, AgentError};
use peco_core::tools::AgentAccess;
use peco_core::tools::ToolDyn;
use peco_core::workflow::{
    ApprovalDecision, ApprovalResponse, NullWorkflowPersister, StepConfig, StepType,
    WorkflowConfig, WorkflowDefinition, WorkflowEngine, WorkflowEvent, WorkflowManager,
    WorkflowStep,
};
use peco_core::workflow::{ExecuteWorkflow, OnFailure, WorkflowAccess};

// ============================================================================
// Demo 1: 编程方式构建 Workflow 并执行
// ============================================================================

/// 展示直接通过 Rust API 构建工作流并执行。
/// 适用于需要动态生成步骤的场景。
async fn demo_programmatic_workflow() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Demo 1: 编程方式构建 DAG Workflow（CI/CD 流水线模拟）       ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // Step 1: 定义工作流步骤（模拟 CI/CD 流水线）
    let steps = vec![
        // ── 并行阶段 1：同时进行 Lint 和 Type Check ──────────────
        WorkflowStep {
            id: "lint".into(),
            name: "Lint 检查".into(),
            step_type: StepType::Shell,
            config: StepConfig::Shell {
                command: "echo 'Running linter...' && sleep 0.1 && echo 'No lint errors found'"
                    .into(),
            },
            depends_on: vec![],
            condition: None,
            timeout_seconds: Some(10),
            on_failure: OnFailure::Continue, // Lint 失败不阻塞构建
            retry_policy: None,
            output_schema: None,
        },
        WorkflowStep {
            id: "typecheck".into(),
            name: "类型检查".into(),
            step_type: StepType::Shell,
            config: StepConfig::Shell {
                command: "echo 'Running type checker...' && sleep 0.15 && echo '0 type errors'"
                    .into(),
            },
            depends_on: vec![],
            condition: None,
            timeout_seconds: Some(10),
            on_failure: OnFailure::Abort,
            retry_policy: None,
            output_schema: None,
        },
        // ── 串行阶段 2：测试（依赖 lint + typecheck） ─────────────
        WorkflowStep {
            id: "test".into(),
            name: "单元测试".into(),
            step_type: StepType::Shell,
            config: StepConfig::Shell {
                command: "echo 'Running tests...' && sleep 0.2 && echo '42 tests passed'".into(),
            },
            depends_on: vec!["lint".into(), "typecheck".into()],
            condition: None,
            timeout_seconds: Some(30),
            on_failure: OnFailure::Abort,
            retry_policy: None,
            output_schema: None,
        },
        // ── 串行阶段 3：构建（依赖 test） ─────────────────────────
        WorkflowStep {
            id: "build".into(),
            name: "构建".into(),
            step_type: StepType::Shell,
            config: StepConfig::Shell {
                command: "echo 'Building release...' && sleep 0.1 && echo 'Build succeeded'".into(),
            },
            depends_on: vec!["test".into()],
            condition: Some("{{ steps.test.success }}".into()), // 仅在测试通过时构建
            timeout_seconds: Some(60),
            on_failure: OnFailure::Abort,
            retry_policy: None,
            output_schema: None,
        },
        // ── 串行阶段 4：部署（条件执行，依赖 build） ──────────────
        WorkflowStep {
            id: "deploy".into(),
            name: "部署到 Staging".into(),
            step_type: StepType::Shell,
            config: StepConfig::Shell {
                command: "echo 'Deploying to staging...' && sleep 0.1 && echo 'Deploy OK'".into(),
            },
            depends_on: vec!["build".into()],
            condition: Some("{{ steps.build.success }}".into()),
            timeout_seconds: Some(30),
            on_failure: OnFailure::Pause, // 部署失败 → 暂停等待人工确认
            retry_policy: None,
            output_schema: None,
        },
    ];

    // Step 2: 包装为 WorkflowDefinition
    let definition = WorkflowDefinition {
        name: "ci-cd-pipeline".into(),
        description: "CI/CD 流水线：Lint → Test → Build → Deploy".into(),
        version: "1.0".into(),
        timeout_seconds: Some(120),
        inputs: HashMap::new(),
        steps,
        body: None,
    };

    // Step 3: 启动引擎
    let agent_access: Arc<dyn AgentAccess> = Arc::new(NoopAgentAccess);
    let persister = Arc::new(NullWorkflowPersister);
    let mut handle = WorkflowEngine::spawn(
        definition,
        agent_access,
        persister,
        WorkflowConfig::default(),
        HashMap::new(),
    );

    // Step 4: 实时消费事件流
    println!("🚀 Workflow started: run_id = {}\n", handle.run_id());
    let mut completed = false;
    while let Some(event) = handle.recv_event().await {
        match &event {
            WorkflowEvent::Started {
                workflow_name,
                total_steps,
                ..
            } => {
                println!("  📋 Workflow '{workflow_name}' — {total_steps} steps total");
            }
            WorkflowEvent::StepStarted {
                step_id, step_name, ..
            } => {
                println!("  ⏳ [{step_id}] {step_name} ...");
            }
            WorkflowEvent::StepCompleted {
                step_id,
                output,
                duration_ms,
                ..
            } => {
                let preview: String = output
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(60)
                    .collect();
                println!("  ✅ [{step_id}] completed in {duration_ms}ms → {preview}");
            }
            WorkflowEvent::StepSkipped {
                step_id, reason, ..
            } => {
                println!("  ⏭️  [{step_id}] skipped: {reason}");
            }
            WorkflowEvent::StepFailed {
                step_id,
                error,
                failure_policy,
                ..
            } => {
                println!("  ❌ [{step_id}] FAILED (policy={failure_policy}): {error}");
            }
            WorkflowEvent::Paused {
                reason,
                paused_at_step,
                ..
            } => {
                println!("  ⏸️  PAUSED at {paused_at_step:?}: {reason}");
                // 自动 Proceed（demo 中不等待用户输入）
                handle
                    .approve(ApprovalResponse {
                        decision: ApprovalDecision::Proceed,
                        note: Some("Demo auto-approve".into()),
                    })
                    .await
                    .ok();
            }
            WorkflowEvent::Resumed { .. } => {
                println!("  ▶️  Resumed");
            }
            WorkflowEvent::Completed {
                steps_completed,
                steps_failed,
                steps_skipped,
                total_duration_ms,
                ..
            } => {
                println!("\n  🎉 COMPLETED in {total_duration_ms}ms ");
                println!(
                    "     completed={steps_completed}, failed={steps_failed}, skipped={steps_skipped}"
                );
                completed = true;
            }
            WorkflowEvent::Failed {
                error,
                failed_at_step,
                ..
            } => {
                println!("\n  💥 FAILED at step {failed_at_step:?}: {error}");
                completed = true;
            }
            WorkflowEvent::Cancelled { .. } => {
                println!("\n  🛑 CANCELLED");
                completed = true;
            }
            _ => {}
        }
        if completed {
            break;
        }
    }
    println!();
}

// ============================================================================
// Demo 2: YAML 方式加载 Workflow + WorkflowManager
// ============================================================================

/// 展示从 YAML 文件加载工作流（通过 WorkflowManager）。
/// 这是生产环境的标准用法 — workflow.md 文件存储在 workflows/ 目录中。
async fn demo_yaml_workflow() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Demo 2: YAML 定义 + WorkflowManager（模板变量传递）         ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // Step 1: 在临时目录创建 workflow.md
    let tmp = tempfile::tempdir().unwrap();
    let wf_dir = tmp.path().join("workflows").join("data-pipeline");
    std::fs::create_dir_all(&wf_dir).unwrap();

    let workflow_yaml = r#"---
workflow:
  name: "data-pipeline"
  description: "数据处理流水线：采集 → 清洗 → 汇总报告"
  version: "1.0"
  timeout_seconds: 60
  steps:
    - id: "collect"
      name: "数据采集"
      type: shell
      config:
        command: "echo '{\"records\": 1500, \"errors\": 3, \"source\": \"api\"}'"
      timeout_seconds: 10
      on_failure: "abort"

    - id: "clean"
      name: "数据清洗"
      type: shell
      config:
        command: "echo 'Cleaned data from {{ steps.collect.output }}'"
      depends_on: ["collect"]
      condition: "{{ steps.collect.success }}"
      timeout_seconds: 10
      on_failure: "continue"

    - id: "report"
      name: "生成报告"
      type: shell
      config:
        command: |
          echo "=== Data Pipeline Report ==="
          echo ""
          echo "Collection output: {{ steps.collect.output }}"
          echo "Cleaning output:   {{ steps.clean.output }}"
          echo ""
          echo "Pipeline completed successfully."
      depends_on: ["clean"]
      timeout_seconds: 10
      on_failure: "abort"
---
# Data Pipeline Workflow
此 Workflow 演示模板变量 `{{ steps.X.output }}` 在步骤间传递数据。
"#;
    std::fs::write(wf_dir.join("workflow.md"), workflow_yaml).unwrap();

    // Step 2: 通过 WorkflowManager 加载
    let manager = WorkflowManager::new(
        tmp.path().join("workflows"),
        Arc::new(NullWorkflowPersister),
    );
    manager.init().unwrap();

    let names = manager.list_names();
    println!("  📁 Available workflows: {names:?}");

    let definition = manager.load("data-pipeline").unwrap();
    println!(
        "  📄 Loaded: '{}' — {} steps, v{}",
        definition.name,
        definition.steps.len(),
        definition.version
    );

    // Step 3: 执行
    let agent_access: Arc<dyn AgentAccess> = Arc::new(NoopAgentAccess);
    let mut handle = manager
        .execute(
            "data-pipeline",
            agent_access,
            WorkflowConfig::default(),
            HashMap::new(),
        )
        .unwrap();

    println!("\n  🚀 Executing data-pipeline...\n");

    while let Some(event) = handle.recv_event().await {
        match &event {
            WorkflowEvent::StepStarted { step_name, .. } => {
                println!("  ⏳ {step_name}");
            }
            WorkflowEvent::StepCompleted {
                step_name, output, ..
            } => {
                let preview = output
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect::<String>();
                println!("  ✅ {step_name} → {preview}");
            }
            WorkflowEvent::Completed {
                total_duration_ms, ..
            } => {
                println!("\n  🎉 Pipeline completed in {total_duration_ms}ms\n");
                break;
            }
            WorkflowEvent::Failed { error, .. } => {
                println!("\n  💥 Pipeline failed: {error}\n");
                break;
            }
            _ => {}
        }
    }
}

// ============================================================================
// Demo 3: ExecuteWorkflow 工具（模拟 Agent 调用 Workflow）
// ============================================================================

/// 展示 execute_workflow 工具 — 从 Agent 视角调用 Workflow。
/// Agent 在 ReAct 循环中调用此工具，同步等待 Workflow 结果。
async fn demo_execute_workflow_tool() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Demo 3: ExecuteWorkflow 工具（Agent → Workflow）            ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // Step 1: 准备 WorkflowAccess（预加载 Definition）
    let definition = WorkflowDefinition {
        name: "health-check".into(),
        description: "系统健康检查".into(),
        version: "1.0".into(),
        timeout_seconds: Some(30),
        inputs: HashMap::new(),
        steps: vec![
            WorkflowStep {
                id: "disk".into(),
                name: "磁盘检查".into(),
                step_type: StepType::Shell,
                config: StepConfig::Shell {
                    command: "echo 'disk: OK (75% free)'".into(),
                },
                depends_on: vec![],
                condition: None,
                timeout_seconds: Some(5),
                on_failure: OnFailure::Continue,
                retry_policy: None,
                output_schema: None,
            },
            WorkflowStep {
                id: "memory".into(),
                name: "内存检查".into(),
                step_type: StepType::Shell,
                config: StepConfig::Shell {
                    command: "echo 'memory: OK (2.1GB free)'".into(),
                },
                depends_on: vec![],
                condition: None,
                timeout_seconds: Some(5),
                on_failure: OnFailure::Continue,
                retry_policy: None,
                output_schema: None,
            },
            WorkflowStep {
                id: "summary".into(),
                name: "生成摘要".into(),
                step_type: StepType::Shell,
                config: StepConfig::Shell {
                    command: "echo 'Health Check Summary: All systems operational'".into(),
                },
                depends_on: vec!["disk".into(), "memory".into()],
                condition: None,
                timeout_seconds: Some(5),
                on_failure: OnFailure::Abort,
                retry_policy: None,
                output_schema: None,
            },
        ],
        body: None,
    };

    // Step 2: 创建 Stub WorkflowAccess
    struct StubWfAccess {
        defs: HashMap<String, WorkflowDefinition>,
    }
    impl WorkflowAccess for StubWfAccess {
        fn load_workflow(
            &self,
            name: &str,
        ) -> Result<WorkflowDefinition, peco_core::workflow::WorkflowError> {
            self.defs.get(name).cloned().ok_or_else(|| {
                peco_core::workflow::WorkflowError::Parse(format!("not found: {name}"))
            })
        }
        fn list_workflow_names(&self) -> Vec<String> {
            self.defs.keys().cloned().collect()
        }
        fn reload_workflow(
            &self,
            name: &str,
        ) -> Result<WorkflowDefinition, peco_core::workflow::WorkflowError> {
            self.load_workflow(name)
        }
    }

    let mut defs: HashMap<String, WorkflowDefinition> = HashMap::new();
    defs.insert("health-check".to_string(), definition);
    let wf_access: Arc<dyn WorkflowAccess> = Arc::new(StubWfAccess { defs });

    // Step 3: 创建 ExecuteWorkflow 工具并调用
    let tool = ExecuteWorkflow::new(
        wf_access,
        Arc::new(NoopAgentAccess),
        Arc::new(NullWorkflowPersister),
    );

    println!("  🔧 Tool: execute_workflow");
    println!("  📋 Description: {}", tool.definition().description);
    println!();

    // 模拟 Agent 的 tool_call
    let result = tool
        .call(r#"{"workflow_name": "health-check"}"#.to_string())
        .await
        .unwrap();

    println!("  📤 Agent receives:\n{result}");
    println!();

    // 测试 not-found 错误
    let err = tool
        .call(r#"{"workflow_name": "nonexistent"}"#.to_string())
        .await
        .unwrap_err();
    println!("  ❌ Not-found error: {err}");
    println!();
}

// ============================================================================
// Demo 4: 失败策略演示（Pause + Approve / Continue / Abort）
// ============================================================================

/// 展示三种失败策略的行为差异。
async fn demo_failure_policies() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Demo 4: 失败策略对比（Continue vs Abort vs Pause）           ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // ── 4a: on_failure: continue（失败后继续） ─────────────────
    println!("  ── 4a: on_failure = continue ──");
    {
        let steps = vec![
            WorkflowStep {
                id: "fail_ok".into(),
                name: "这个步骤会失败".into(),
                step_type: StepType::Shell,
                config: StepConfig::Shell {
                    command: "echo 'I will fail' && exit 1".into(),
                },
                depends_on: vec![],
                condition: None,
                timeout_seconds: Some(5),
                on_failure: OnFailure::Continue,
                retry_policy: None,
                output_schema: None,
            },
            WorkflowStep {
                id: "still_runs".into(),
                name: "失败后依然执行".into(),
                step_type: StepType::Shell,
                config: StepConfig::Shell {
                    command: "echo 'I still run despite previous failure'".into(),
                },
                depends_on: vec!["fail_ok".into()],
                condition: None,
                timeout_seconds: Some(5),
                on_failure: OnFailure::Abort,
                retry_policy: None,
                output_schema: None,
            },
        ];
        run_and_print(&steps, "continue").await;
    }

    // ── 4b: on_failure: abort（失败即中止） ─────────────────────
    println!("  ── 4b: on_failure = abort ──");
    {
        let steps = vec![
            WorkflowStep {
                id: "fail_abort".into(),
                name: "这个步骤会失败".into(),
                step_type: StepType::Shell,
                config: StepConfig::Shell {
                    command: "echo 'ABORTING!' && exit 1".into(),
                },
                depends_on: vec![],
                condition: None,
                timeout_seconds: Some(5),
                on_failure: OnFailure::Abort,
                retry_policy: None,
                output_schema: None,
            },
            WorkflowStep {
                id: "never_runs".into(),
                name: "永远不会执行".into(),
                step_type: StepType::Shell,
                config: StepConfig::Shell {
                    command: "echo 'This should NOT print'".into(),
                },
                depends_on: vec!["fail_abort".into()],
                condition: None,
                timeout_seconds: Some(5),
                on_failure: OnFailure::Abort,
                retry_policy: None,
                output_schema: None,
            },
        ];
        run_and_print(&steps, "abort").await;
    }

    // ── 4c: on_failure: pause（失败暂停等待审批） ───────────────
    println!("  ── 4c: on_failure = pause ──");
    {
        let steps = vec![WorkflowStep {
            id: "fail_pause".into(),
            name: "失败后暂停等待审批".into(),
            step_type: StepType::Shell,
            config: StepConfig::Shell {
                command: "echo 'Pausing for human approval...' && exit 1".into(),
            },
            depends_on: vec![],
            condition: None,
            timeout_seconds: Some(5),
            on_failure: OnFailure::Pause,
            retry_policy: None,
            output_schema: None,
        }];
        run_and_print(&steps, "pause").await;
    }
}

/// 执行一个 Workflow 并打印所有事件。
async fn run_and_print(steps: &[WorkflowStep], label: &str) {
    let definition = WorkflowDefinition {
        name: format!("failure-demo-{label}"),
        description: format!("Demonstrate {label} failure policy"),
        version: "1.0".into(),
        timeout_seconds: Some(30),
        inputs: HashMap::new(),
        steps: steps.to_vec(),
        body: None,
    };

    let mut handle = WorkflowEngine::spawn(
        definition,
        Arc::new(NoopAgentAccess),
        Arc::new(NullWorkflowPersister),
        WorkflowConfig::default(),
        HashMap::new(),
    );

    while let Some(event) = handle.recv_event().await {
        match event {
            WorkflowEvent::StepStarted { step_name, .. } => {
                println!("     ⏳ {step_name}");
            }
            WorkflowEvent::StepCompleted { step_name, .. } => {
                println!("     ✅ {step_name}");
            }
            WorkflowEvent::StepFailed {
                step_name,
                error,
                failure_policy,
                ..
            } => {
                println!("     ❌ {step_name}: {error}  (policy={failure_policy})");
            }
            WorkflowEvent::StepSkipped {
                step_name, reason, ..
            } => {
                println!("     ⏭️  {step_name}: {reason}");
            }
            WorkflowEvent::Paused { reason, .. } => {
                println!("     ⏸️  PAUSED: {reason}");
                // Demo 自动 Proceed
                handle
                    .approve(ApprovalResponse {
                        decision: ApprovalDecision::Proceed,
                        note: Some("Demo auto-approve".into()),
                    })
                    .await
                    .ok();
            }
            WorkflowEvent::Resumed { .. } => {
                println!("     ▶️  Resumed (auto-approved)");
            }
            WorkflowEvent::Completed {
                steps_completed,
                steps_failed,
                steps_skipped,
                ..
            } => {
                println!(
                    "     🎉 Done: {steps_completed} ok, {steps_failed} failed, {steps_skipped} skipped\n"
                );
                break;
            }
            WorkflowEvent::Failed { error, .. } => {
                println!("     💥 Workflow FAILED: {error}\n");
                break;
            }
            _ => {}
        }
    }
}

// ============================================================================
// Noop 实现（本 Demo 只用 Shell 步骤，无需真实 Agent）
// ============================================================================

struct NoopAgentAccess;
impl AgentAccess for NoopAgentAccess {
    fn load_agent(&self, _name: &str) -> Result<Arc<Agent>, AgentError> {
        Err(AgentError::Config("noop agent (shell-only demo)".into()))
    }
    fn list_agent_names(&self) -> Vec<String> {
        vec![]
    }
    fn save_agent(&self, _name: &str, _content: &str) -> Result<(), String> {
        Ok(())
    }
}

// ============================================================================
// main
// ============================================================================

#[tokio::main]
async fn main() {
    println!();
    demo_programmatic_workflow().await;
    demo_yaml_workflow().await;
    demo_execute_workflow_tool().await;
    demo_failure_policies().await;
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  ✅ All demos completed successfully!                       ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");
}
