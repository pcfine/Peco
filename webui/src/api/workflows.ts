import api from "./client";
import type {
  WorkflowListItem,
  WorkflowDetailResponse,
  ExecuteResponse,
  ExecutionListResponse,
  ExecutionDetailResponse,
  ExecutionQueryParams,
  StatisticsResponse,
  StatisticsQuery,
  CreateWorkflowRequest,
  UpdateWorkflowRequest,
  ExecuteWorkflowRequest,
  ApproveRequest,
} from "@/types/workflow";
import type { SuccessResponse } from "@/types/common";
import { useAuthStore } from "@/stores/authStore";
import type { WorkflowSSEEvent } from "@/types/workflow";

// ── Workflow 定义 CRUD ──────────────────────────────────────

export async function listWorkflows(): Promise<WorkflowListItem[]> {
  const res = await api.get<WorkflowListItem[]>("/workflows");
  return res.data;
}

export async function getWorkflow(
  name: string,
): Promise<WorkflowDetailResponse> {
  const res = await api.get<WorkflowDetailResponse>(
    `/workflows/${encodeURIComponent(name)}`,
  );
  return res.data;
}

export async function createWorkflow(
  data: CreateWorkflowRequest,
): Promise<WorkflowDetailResponse> {
  const res = await api.post<WorkflowDetailResponse>("/workflows", data);
  return res.data;
}

export async function updateWorkflow(
  name: string,
  data: UpdateWorkflowRequest,
): Promise<WorkflowDetailResponse> {
  const res = await api.put<WorkflowDetailResponse>(
    `/workflows/${encodeURIComponent(name)}`,
    data,
  );
  return res.data;
}

export async function deleteWorkflow(name: string): Promise<SuccessResponse> {
  const res = await api.delete<SuccessResponse>(
    `/workflows/${encodeURIComponent(name)}`,
  );
  return res.data;
}

// ── 执行 ────────────────────────────────────────────────────

export async function executeWorkflow(
  name: string,
  data?: ExecuteWorkflowRequest,
): Promise<ExecuteResponse> {
  const res = await api.post<ExecuteResponse>(
    `/workflows/${encodeURIComponent(name)}/execute`,
    data ?? {},
  );
  return res.data;
}

export async function listExecutions(
  params?: ExecutionQueryParams,
): Promise<ExecutionListResponse> {
  const res = await api.get<ExecutionListResponse>("/workflows/executions", {
    params,
  });
  return res.data;
}

export async function getExecution(
  runId: string,
): Promise<ExecutionDetailResponse> {
  const res = await api.get<ExecutionDetailResponse>(
    `/workflows/executions/${runId}`,
  );
  return res.data;
}

export async function cancelExecution(runId: string): Promise<SuccessResponse> {
  const res = await api.post<SuccessResponse>(
    `/workflows/executions/${runId}/cancel`,
  );
  return res.data;
}

export async function approveExecution(
  runId: string,
  data: ApproveRequest,
): Promise<SuccessResponse> {
  const res = await api.post<SuccessResponse>(
    `/workflows/executions/${runId}/approve`,
    data,
  );
  return res.data;
}

// ── 统计 ────────────────────────────────────────────────────

export async function getStatistics(
  name: string,
  params?: StatisticsQuery,
): Promise<StatisticsResponse> {
  const res = await api.get<StatisticsResponse>(
    `/workflows/${encodeURIComponent(name)}/statistics`,
    { params },
  );
  return res.data;
}

// ── SSE 流 ──────────────────────────────────────────────────

/**
 * SSE stream for workflow execution.
 *
 * Reconnection strategy:
 * 1. On disconnect, check if status is terminal → stop
 * 2. If running/paused → fetch snapshot, hydrate, reconnect (max 3 retries, 1s/2s/4s backoff)
 * 3. After 3 failures → call onReconnectFailed, throw
 */
export async function streamExecution(
  runId: string,
  onEvent: (e: WorkflowSSEEvent) => void,
  options?: {
    signal?: AbortSignal;
    maxRetries?: number;
    onReconnecting?: (attempt: number) => void;
    onReconnectFailed?: () => void;
  },
): Promise<void> {
  const maxRetries = options?.maxRetries ?? 3;
  let retries = 0;

  while (retries <= maxRetries) {
    try {
      await connectStream(runId, onEvent, options?.signal);
      return; // Normal completion
    } catch (err) {
      if (options?.signal?.aborted) return;

      if (retries >= maxRetries) {
        options?.onReconnectFailed?.();
        throw err;
      }

      retries++;
      options?.onReconnecting?.(retries);

      // Hydrate from snapshot before reconnecting
      try {
        const detail = await getExecution(runId);
        hydrateFromSnapshot(detail, onEvent);
      } catch {
        // If snapshot fetch fails, still try to reconnect
      }

      // Exponential backoff: 1s, 2s, 4s
      await new Promise((r) => setTimeout(r, Math.pow(2, retries - 1) * 1000));
    }
  }
}

async function connectStream(
  runId: string,
  onEvent: (e: WorkflowSSEEvent) => void,
  signal?: AbortSignal,
): Promise<void> {
  const token = useAuthStore.getState().token;
  const response = await fetch(`/api/workflows/executions/${runId}/stream`, {
    headers: {
      Authorization: `Bearer ${token}`,
      Accept: "text/event-stream",
    },
    signal,
  });

  if (!response.ok) {
    throw new Error(`SSE stream failed: ${response.status}`);
  }

  const reader = response.body?.getReader();
  if (!reader) throw new Error("No readable stream");

  const decoder = new TextDecoder();
  let buffer = "";

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;

      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split("\n");
      buffer = lines.pop() ?? "";

      for (const line of lines) {
        if (line.startsWith("data: ")) {
          try {
            const parsed = JSON.parse(line.slice(6));
            // peco-server wraps SSE as {event, data}, but workflow events may
            // arrive directly with a "type" field
            const event = parsed.data ?? parsed;
            if (event.type) {
              onEvent(event as WorkflowSSEEvent);
            }
          } catch {
            // Skip unparseable lines
          }
        }
      }
    }
  } finally {
    reader.releaseLock();
  }
}

/**
 * Hydrate store state from a GET /executions/:runId snapshot.
 * Replays step results as synthetic SSE events so the UI state is rebuilt.
 */
function hydrateFromSnapshot(
  detail: ExecutionDetailResponse,
  onEvent: (e: WorkflowSSEEvent) => void,
): void {
  // Replay workflow_started
  onEvent({
    type: "workflow_started",
    runId: detail.summary.runId,
    workflowName: detail.summary.workflowName,
    totalSteps: detail.summary.totalSteps,
  });

  // Replay each step result
  for (const step of detail.stepResults) {
    switch (step.outcome) {
      case "success":
        onEvent({
          type: "step_completed",
          runId: detail.summary.runId,
          stepId: step.stepId,
          stepName: step.stepName,
          output: step.output ?? "",
          durationMs: step.durationMs,
          attempt: step.attempt,
        });
        break;
      case "failed":
        onEvent({
          type: "step_failed",
          runId: detail.summary.runId,
          stepId: step.stepId,
          stepName: step.stepName,
          error: step.error ?? step.output ?? "Unknown error",
          durationMs: step.durationMs,
          attempt: step.attempt,
          failurePolicy: "abort",
        });
        break;
      case "skipped":
        onEvent({
          type: "step_skipped",
          runId: detail.summary.runId,
          stepId: step.stepId,
          stepName: step.stepName,
          reason: step.reason ?? step.error ?? "Condition not met",
        });
        break;
    }
  }

  // Replay terminal state
  switch (detail.summary.status) {
    case "completed":
      onEvent({
        type: "workflow_completed",
        runId: detail.summary.runId,
        totalDurationMs: detail.summary.totalDurationMs ?? 0,
        stepsCompleted: detail.summary.stepsCompleted,
        stepsFailed: detail.summary.stepsFailed,
        stepsSkipped: detail.summary.stepsSkipped,
      });
      break;
    case "failed":
      onEvent({
        type: "workflow_failed",
        runId: detail.summary.runId,
        error: detail.error ?? "Unknown error",
        totalDurationMs: detail.summary.totalDurationMs ?? 0,
      });
      break;
    case "cancelled":
      onEvent({
        type: "workflow_cancelled",
        runId: detail.summary.runId,
      });
      break;
    case "paused":
      onEvent({
        type: "workflow_paused",
        runId: detail.summary.runId,
        reason: "Awaiting approval",
      });
      break;
  }
}
