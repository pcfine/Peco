// ============================================================================
// execute_task — 定时任务执行逻辑
// ============================================================================
//
// 由 CronScheduler 中注册的 Job closure 调用。
// 每次触发时：构建 Agent → 创建 Session → AgentLooper 执行 → 收集结果 → 写入日志。

use std::sync::Arc;

use chrono::Utc;
use peco_core::agent::{AgentLooper, LooperConfig, LooperEvent, TurnOutcome};
use peco_core::persistence::{NullSessionPersister, SessionPersister};
use peco_core::session::Session;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::db::{task_logs, tasks};
use crate::state::AppState;

/// 执行一次定时任务。
///
/// # 流程
///
/// 1. 写入 task_log (status = "running")
/// 2. 从 AgentRegistry 构建 Agent（带 LRU 缓存）
/// 3. 创建临时 Session + AgentLooper
/// 4. 发送 prompt，通过 LooperEvent 收集结果文本
/// 5. 更新 task_log (status = "success" / "error")
/// 6. 更新 tasks.last_run_at
///
/// # Panic safety
///
/// 此函数不 panic，所有错误通过 task_log 记录。
pub async fn execute_task(
    task_id: String,
    agent_id: String,
    user_id: String,
    prompt: String,
    pool: SqlitePool,
    state: Arc<AppState>,
) {
    let started_at = Utc::now();
    let log_id = Uuid::new_v4().to_string();

    tracing::info!(
        task_id = %task_id,
        agent_id = %agent_id,
        "Scheduled task triggered"
    );

    // ── 1. 写入 running 日志 ──────────────────────────────────────────────
    if let Err(e) = task_logs::insert(
        &pool,
        &task_logs::CreateLogParams {
            id: &log_id,
            task_id: &task_id,
            status: "running",
            output: "",
            error: "",
            started_at: &started_at.to_rfc3339(),
            finished_at: None,
        },
    )
    .await
    {
        tracing::error!(task_id = %task_id, error = %e, "Failed to insert running log");
        return;
    }

    // ── 2. 构建 Agent ─────────────────────────────────────────────────────
    // ★ 注意：get_or_build 需要 self_arc 参数
    let agent = match state
        .agent_registry
        .get_or_build(
            Arc::clone(&state.agent_registry),
            &pool,
            &user_id,
            &agent_id,
            &state.data_dir,
        )
        .await
    {
        Ok(a) => a,
        Err(e) => {
            let finished_at = Utc::now().to_rfc3339();
            let error_msg = format!("Failed to build agent: {e}");
            tracing::error!(task_id = %task_id, error = %error_msg);
            let _ = task_logs::update_status(
                &pool,
                &log_id,
                "error",
                "",
                &error_msg,
                &finished_at,
            )
            .await;
            return;
        }
    };

    // ── 3. 创建 Session + AgentLooper ─────────────────────────────────────
    // ★ LooperConfig 没有 max_turns 字段；Agent 自身携带 max_turns
    let session = Box::new(Session::new(
        Uuid::new_v4().to_string(),
        format!("Scheduled Task: {}", &task_id),
    ));
    let persister: Arc<dyn SessionPersister> = Arc::new(NullSessionPersister);
    let config = LooperConfig::default();
    let handle = AgentLooper::spawn(agent, session, config, persister);

    // ── 4. 发送用户消息 ──────────────────────────────────────────────────
    if let Err(e) = handle.send_query(prompt.clone()).await {
        let finished_at = Utc::now().to_rfc3339();
        let error_msg = format!("Failed to send query: {e}");
        tracing::error!(task_id = %task_id, error = %error_msg);
        let _ = task_logs::update_status(&pool, &log_id, "error", "", &error_msg, &finished_at)
            .await;
        return;
    }

    // ── 5. 事件循环：收集文本结果 ────────────────────────────────────────
    // ★ wait() 只返回 ModelResponse { usage, turns }，不含文本
    // ★ 文本从 TurnComplete 事件中提取：Success { text } / Failed { partial_text }
    let mut output = String::new();
    let mut had_error = false;
    let mut error_text = String::new();

    loop {
        match handle.recv_event().await {
            Some(LooperEvent::TurnComplete { outcome, .. }) => {
                match outcome {
                    TurnOutcome::Success { text } => {
                        output = text;
                    }
                    TurnOutcome::Failed {
                        reason,
                        partial_text,
                    } => {
                        had_error = true;
                        error_text = format!("Turn failed: {reason:?}");
                        if !partial_text.is_empty() {
                            output = partial_text;
                        }
                        tracing::warn!(
                            task_id = %task_id,
                            reason = ?reason,
                            "Task turn failed"
                        );
                    }
                }
            }
            Some(LooperEvent::Shutdown {
                total_usage,
                total_turns,
                ..
            }) => {
                tracing::info!(
                    task_id = %task_id,
                    turns = total_turns,
                    input_tokens = total_usage.input_tokens,
                    output_tokens = total_usage.output_tokens,
                    "Task looper shutdown"
                );
                break;
            }
            None => {
                // 事件通道关闭
                break;
            }
            _ => {
                // 忽略其他事件（TextDelta、ToolCall 等）
            }
        }
    }

    // ── 6. 写入 task_log 结果 ─────────────────────────────────────────
    let finished_at = Utc::now().to_rfc3339();
    let status = if had_error { "error" } else { "success" };
    let recorded_error = if had_error { error_text.as_str() } else { "" };

    if let Err(e) = task_logs::update_status(
        &pool,
        &log_id,
        status,
        &output,
        recorded_error,
        &finished_at,
    )
    .await
    {
        tracing::error!(task_id = %task_id, log_id = %log_id, error = %e, "Failed to update task log");
    }

    // ── 7. 更新 tasks.last_run_at ─────────────────────────────────────────
    if let Err(e) =
        tasks::update_run_timestamps(&pool, &task_id, &started_at.to_rfc3339()).await
    {
        tracing::error!(task_id = %task_id, error = %e, "Failed to update task timestamps");
    }

    tracing::info!(
        task_id = %task_id,
        status = status,
        duration_ms = (Utc::now() - started_at).num_milliseconds(),
        "Task execution completed"
    );
}
