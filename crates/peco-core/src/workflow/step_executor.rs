// ============================================================================
// step_executor — 步骤执行器（Shell + Agent 类型）
// ============================================================================

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crate::agent::SimpleAgentLooper;
use crate::tools::AgentAccess;

use super::definition::{StepConfig, StepOutcome, StepResult, WorkflowStep};
use super::template::TemplateContext;

/// 步骤级默认超时（秒），仅作为 **shell** 步骤的兜底。
///
/// agent 步骤默认不设超时——这与 ReAct 循环的对话式语义一致，长任务（代码审查、
/// 大重构）不应被硬性截断；只有显式设置 `timeout_seconds` 才会为 agent 步骤限时。
/// 此常量集中管理，便于全局调整默认值。
const DEFAULT_STEP_TIMEOUT_SECS: u64 = 300;

/// 执行单个步骤（静态方法，供 tokio::spawn 使用），返回 StepResult。
///
/// Phase 1 支持 Shell 和 Agent 两种步骤类型。Llm/Tool 在 validate() 阶段已被拒绝。
///
/// 步骤级超时在此处统一收敛：显式设置 `timeout_seconds` 时用 `tokio::time::timeout`
/// 包裹整个步骤分派 future；未设置时仅 shell 步骤获得 `DEFAULT_STEP_TIMEOUT_SECS`
/// 兜底，agent 步骤无超时（保持与 ReAct 循环一致的「可长时间运行」语义）。超时返回
/// `StepOutcome::Failed`（走现有 `StepFailed` 事件 + `on_failure` 策略），不引入新事件。
pub(crate) async fn execute_step_static(
    step: &WorkflowStep,
    tpl_ctx: &TemplateContext,
    cancel_flag: &Arc<AtomicBool>,
    agent_access: &Arc<dyn AgentAccess>,
) -> StepResult {
    let start = Instant::now();
    // 超时策略：显式 `timeout_seconds` 对所有步骤生效；未设置时仅 shell 步骤用
    // `DEFAULT_STEP_TIMEOUT_SECS` 兜底，agent 步骤保持无超时（长任务不截断）。
    let timeout: Option<Duration> = match step.timeout_seconds {
        Some(secs) => Some(Duration::from_secs(secs)),
        None => match step.config {
            StepConfig::Shell { .. } => Some(Duration::from_secs(DEFAULT_STEP_TIMEOUT_SECS)),
            _ => None,
        },
    };

    let step_fut = async {
        match &step.config {
            StepConfig::Shell { command } => match tpl_ctx.render(command) {
                Ok(rendered) => execute_shell_step(&rendered).await,
                Err(e) => StepOutcome::Failed(format!("template error: {e}")),
            },
            StepConfig::Agent {
                agent,
                prompt,
                max_turns,
            } => match tpl_ctx.render(prompt) {
                Ok(rendered_prompt) => {
                    execute_agent_step(
                        agent_access,
                        agent,
                        &rendered_prompt,
                        *max_turns,
                        step.output_schema.clone(),
                        cancel_flag,
                    )
                    .await
                }
                Err(e) => StepOutcome::Failed(format!("template error: {e}")),
            },
            StepConfig::Llm { .. } | StepConfig::Tool { .. } => {
                unreachable!("Phase 4 step types are rejected during validation")
            }
        }
    };

    let outcome = match timeout {
        Some(timeout) => match tokio::time::timeout(timeout, step_fut).await {
            Ok(outcome) => outcome,
            Err(_) => StepOutcome::Failed(format!("step timed out after {timeout:?}")),
        },
        None => step_fut.await,
    };

    StepResult {
        step: step.clone(),
        output: match &outcome {
            StepOutcome::Success(s) => Some(s.clone()),
            _ => None,
        },
        outcome,
        duration: start.elapsed(),
        attempt: 1,
        structured_output: None,
    }
}

/// 执行 Shell 类型步骤（已渲染 command，直接执行，无内层超时）。
///
/// 超时由 `execute_step_static` 统一负责，此处仅调用 `Command::output()`。
async fn execute_shell_step(command: &str) -> StepOutcome {
    match tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        // 超时/取消/全局超时中止步骤任务时，`output()` future 被 drop；若未设置
        // `kill_on_drop`，`sh -c` 子进程会继续在后台运行（孤儿进程，持续消耗 CPU
        // 并产生副作用）。`kill_on_drop(true)` 在 handle drop 时 SIGKILL 直接子进程。
        // 注意：这仅杀死 `sh` 本身；复合命令（如 `sleep 300 && deploy.sh`）fork 出的
        // 孙进程仍可能存活，彻底清理需按进程组 kill（后续可加固）。
        .kill_on_drop(true)
        .output()
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let combined = format!("{stdout}\n{stderr}").trim().to_string();
            if output.status.success() {
                StepOutcome::Success(combined)
            } else {
                StepOutcome::Failed(format!(
                    "exit code: {}\n{combined}",
                    output.status.code().unwrap_or(-1)
                ))
            }
        }
        Err(e) => StepOutcome::Failed(format!("command execution error: {e}")),
    }
}

/// 执行 Agent 类型步骤（复用 SimpleAgentLooper）。
///
/// Phase 1: output_schema 通过在 prompt 末尾追加 JSON schema 指令来处理。
/// Phase 4: 使用 StructuredOutputExecutor 做真正的结构化输出。
///
/// TODO(Phase 4): cancel_flag 当前未在 Agent 步骤执行期间检查。
/// SimpleAgentLooper 内部等待 LLM 响应时对 tokio 的 abort 不敏感
///（tokio JoinHandle::abort 对 pending I/O future 无效），取消信号仅
/// 在层级边界生效。Phase 4 可考虑为 SimpleAgentLooper 增加取消通道或
/// timeout 机制来缩短取消响应延迟。
async fn execute_agent_step(
    agent_access: &Arc<dyn AgentAccess>,
    agent_name: &str,
    prompt: &str,
    max_turns: Option<usize>,
    output_schema: Option<serde_json::Value>,
    _cancel_flag: &Arc<AtomicBool>,
) -> StepOutcome {
    // 1. 通过 DI 加载 Agent（返回 Arc<Agent>，已包含 agent.md 中定义的工具）
    let agent = match agent_access.load_agent(agent_name) {
        Ok(a) => a,
        Err(e) => return StepOutcome::Failed(e.to_string()),
    };

    // 2. 构造最终 prompt（含可选的 schema 指令）
    let final_prompt = if let Some(schema) = &output_schema {
        let schema_str = serde_json::to_string_pretty(schema).unwrap_or_default();
        format!("{prompt}\n\n请以 JSON 格式输出，必须符合以下 schema:\n```json\n{schema_str}\n```")
    } else {
        prompt.to_string()
    };

    // 3. 启动 SimpleAgentLooper（batch 模式）
    let handle = SimpleAgentLooper::spawn(agent, final_prompt, max_turns);

    // 4. 等待完成
    match handle.wait().await {
        Ok(output) => StepOutcome::Success(output),
        Err(e) => StepOutcome::Failed(e.to_string()),
    }
}
