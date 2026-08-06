// Workflow types — aligned with peco-server /api/workflows/* and /api/schedules/*

// ── Workflow 定义 ──────────────────────────────────────────

export interface WorkflowListItem {
  name: string;
  description: string;
  version: string;
  stepCount: number;
  schedule: ScheduleInfo | null;
  lastExecution: ExecutionSummary | null;
  createdAt: string;
  updatedAt: string;
}

export interface ScheduleInfo {
  cron: string;
  enabled: boolean;
  timezone?: string;
}

export interface WorkflowDetailResponse {
  name: string;
  description: string;
  version: string;
  timeoutSeconds?: number;
  inputs: Record<string, WorkflowInputDef>;
  stepCount: number;
  yaml: string;
  schedule: ScheduleInfo | null;
  lastExecution: ExecutionSummary | null;
}

export interface WorkflowInputDef {
  type: string;
  description?: string;
  required: boolean;
  default?: unknown;
}

// ── 请求体 ──────────────────────────────────────────────────

export interface CreateWorkflowRequest {
  yaml: string;
}

export interface UpdateWorkflowRequest {
  yaml: string;
}

export interface ExecuteWorkflowRequest {
  inputs?: Record<string, unknown>;
}

export interface ExecuteResponse {
  runId: string;
  workflowName: string;
  status: string;
  triggerType: string;
  startedAt: string;
}

export interface ApproveRequest {
  decision: "proceed" | "abort";
  note?: string;
}

// ── 执行记录 ────────────────────────────────────────────────

export interface ExecutionSummary {
  runId: string;
  workflowName: string;
  triggerType: "manual" | "scheduled";
  status: "running" | "paused" | "completed" | "failed" | "cancelled";
  totalSteps: number;
  stepsCompleted: number;
  stepsFailed: number;
  stepsSkipped: number;
  totalDurationMs?: number;
  startedAt: string;
  finishedAt?: string;
}

export interface ExecutionDetailResponse {
  summary: ExecutionSummary;
  inputs?: Record<string, unknown>;
  error?: string;
  stepResults: StepResultResponse[];
}

export interface StepResultResponse {
  stepId: string;
  stepName: string;
  stepType: string;
  outcome: string;
  output?: string;
  durationMs: number;
  attempt: number;
}

export interface ExecutionListResponse {
  executions: ExecutionSummary[];
  total: number;
  offset: number;
  limit: number;
}

export interface ExecutionQueryParams {
  workflowName?: string;
  status?: string;
  triggerType?: string;
  offset?: number;
  limit?: number;
}

// ── 统计 ────────────────────────────────────────────────────

export interface StatisticsResponse {
  workflowName: string;
  totalRuns: number;
  successCount: number;
  failureCount: number;
  cancelledCount: number;
  successRate: number;
  avgDurationMs: number;
  minDurationMs: number;
  maxDurationMs: number;
  lastRun: ExecutionSummary | null;
  runHistory30d: DailyRunStat[];
  stepStats: StepStatResponse[];
}

export interface DailyRunStat {
  date: string;
  total: number;
  success: number;
  failure: number;
}

export interface StepStatResponse {
  stepId: string;
  stepName: string;
  avgDurationMs: number;
  failureRate: number;
}

export interface StatisticsQuery {
  days?: number;
}

// ── 调度配置 ────────────────────────────────────────────────

export interface ScheduleResponse {
  workflowName: string;
  cron: string;
  enabled: boolean;
  timezone?: string;
  createdAt: string;
  updatedAt: string;
}

export interface CreateScheduleRequest {
  workflowName: string;
  cron: string;
  enabled?: boolean;
  timezone?: string;
}

export interface ReplaceScheduleRequest {
  cron: string;
  enabled: boolean;
  timezone?: string;
}

export interface UpdateScheduleRequest {
  cron?: string;
  enabled?: boolean;
  timezone?: string;
}

// ── SSE 事件 ────────────────────────────────────────────────

export type WorkflowSSEEvent =
  | {
      type: "workflow_started";
      runId: string;
      workflowName: string;
      totalSteps: number;
    }
  | {
      type: "step_started";
      runId: string;
      stepId: string;
      stepName: string;
      stepType: string;
    }
  | {
      type: "step_completed";
      runId: string;
      stepId: string;
      stepName: string;
      output: string;
      durationMs: number;
      attempt: number;
    }
  | {
      type: "step_skipped";
      runId: string;
      stepId: string;
      stepName: string;
      reason: string;
    }
  | {
      type: "step_failed";
      runId: string;
      stepId: string;
      stepName: string;
      error: string;
      durationMs: number;
      attempt: number;
      failurePolicy: "continue" | "abort" | "retry" | "pause";
    }
  | {
      type: "workflow_paused";
      runId: string;
      reason: string;
      pausedAtStep?: string;
    }
  | {
      type: "workflow_resumed";
      runId: string;
    }
  | {
      type: "workflow_completed";
      runId: string;
      totalDurationMs: number;
      stepsCompleted: number;
      stepsFailed: number;
      stepsSkipped: number;
    }
  | {
      type: "workflow_failed";
      runId: string;
      error: string;
      failedAtStep?: string;
      totalDurationMs: number;
    }
  | {
      type: "workflow_cancelled";
      runId: string;
    }
  | { type: "done"; runId: string };

// ── DAG 节点 / 边（前端内部使用）────────────────────────────

export interface DagNode {
  id: string;
  name: string;
  type: "shell" | "agent" | "llm" | "tool";
  status?: "pending" | "running" | "success" | "failed" | "skipped";
  x?: number;
  y?: number;
  width?: number;
  height?: number;
}

export interface DagEdge {
  from: string;
  to: string;
}
