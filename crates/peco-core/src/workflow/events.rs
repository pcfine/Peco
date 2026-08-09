// ============================================================================
// WorkflowEvent — 执行过程中的事件枚举
// ============================================================================

use serde::{Deserialize, Serialize};

/// Workflow 执行期间产生的事件。
///
/// 通过 Speaker/Listener 通道传输，与 LooperEvent 模式一致。
/// 所有事件变体均自包含 `run_id`，前端/SSE 消费者无需外部上下文注入。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkflowEvent {
    /// Workflow 开始执行
    Started {
        run_id: String,
        workflow_name: String,
        total_steps: usize,
    },

    /// 步骤开始执行
    StepStarted {
        run_id: String,
        step_id: String,
        step_name: String,
        step_type: String, // "shell" | "agent"
    },

    /// 步骤执行中的增量输出（Phase 4 预留）
    StepDelta {
        run_id: String,
        step_id: String,
        text: String,
    },

    /// 步骤执行成功
    StepCompleted {
        run_id: String,
        step_id: String,
        step_name: String,
        output: String,
        duration_ms: u64,
        attempt: usize,
    },

    /// 步骤被跳过（条件不满足）
    StepSkipped {
        run_id: String,
        step_id: String,
        step_name: String,
        reason: String,
    },

    /// 步骤执行失败
    StepFailed {
        run_id: String,
        step_id: String,
        step_name: String,
        error: String,
        duration_ms: u64,
        attempt: usize,
        failure_policy: String, // "continue" | "abort" | "retry" | "pause"
    },

    /// 等待重试（Phase 4 预留）
    StepRetrying {
        run_id: String,
        step_id: String,
        attempt: usize,
        max_attempts: usize,
        backoff_seconds: u64,
    },

    /// Workflow 暂停（等待审批或外部恢复）
    Paused {
        run_id: String,
        reason: String,
        paused_at_step: Option<String>,
    },

    /// Workflow 恢复执行
    Resumed { run_id: String },

    /// Workflow 成功完成
    Completed {
        run_id: String,
        total_duration_ms: u64,
        steps_completed: usize,
        steps_failed: usize,
        steps_skipped: usize,
    },

    /// Workflow 执行失败
    Failed {
        run_id: String,
        error: String,
        failed_at_step: Option<String>,
        total_duration_ms: u64,
    },

    /// Workflow 被取消
    Cancelled { run_id: String },
}

/// 审批决策枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalDecision {
    /// 继续执行（忽略失败）
    Proceed,
    /// 中止整个 workflow
    Abort,
}

/// 审批响应：外部通过 WorkflowHandle::approve() 发送给引擎。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub decision: ApprovalDecision,
    /// 可选备注
    pub note: Option<String>,
}
