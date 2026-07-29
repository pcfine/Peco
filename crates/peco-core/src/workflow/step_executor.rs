// ============================================================================
// step_executor — 步骤执行器（Shell + Agent 类型）
// ============================================================================

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crate::agent::SimpleAgentLooper;
use crate::tools::AgentAccess;

use super::definition::{StepConfig, StepOutcome, StepResult, WorkflowStep};
use super::template::TemplateContext;

/// 执行单个步骤（静态方法，供 tokio::spawn 使用），返回 StepResult。
///
/// Phase 1 支持 Shell 和 Agent 两种步骤类型。Llm/Tool 在 validate() 阶段已被拒绝。
pub(crate) async fn execute_step_static(
    step: &WorkflowStep,
    _prev_results: &HashMap<String, StepResult>,
    tpl_ctx: &TemplateContext,
    cancel_flag: &Arc<AtomicBool>,
    agent_access: &Arc<dyn AgentAccess>,
) -> StepResult {
    let start = Instant::now();

    let outcome = match &step.config {
        StepConfig::Shell { command } => match tpl_ctx.render(command) {
            Ok(rendered) => execute_shell_step(&rendered, step.timeout_seconds).await,
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

/// 执行 Shell 类型步骤。
async fn execute_shell_step(command: &str, timeout_seconds: Option<u64>) -> StepOutcome {
    let timeout = Duration::from_secs(timeout_seconds.unwrap_or(300));

    match tokio::time::timeout(
        timeout,
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .output(),
    )
    .await
    {
        Ok(Ok(output)) => {
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
        Ok(Err(e)) => StepOutcome::Failed(format!("command execution error: {e}")),
        Err(_) => StepOutcome::Failed("command timed out".to_string()),
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
