import { create } from "zustand";
import type {
  WorkflowSSEEvent,
  DagNode,
  DagEdge,
  StepResultResponse,
} from "@/types/workflow";
import { streamExecution, getExecution } from "@/api/workflows";

// ── Types ────────────────────────────────────────────────────

export interface TimelineEntry {
  timestamp: string;
  event: WorkflowSSEEvent;
}

export type RunStatus =
  | "idle"
  | "running"
  | "paused"
  | "completed"
  | "failed"
  | "cancelled"
  | "timed_out";

export interface WorkflowRunState {
  runId: string | null;
  workflowName: string;
  status: RunStatus;
  totalSteps: number;
  stepsCompleted: number;
  stepsFailed: number;
  stepsSkipped: number;
  totalDurationMs: number;
  error: string | null;
  dagNodes: DagNode[];
  dagEdges: DagEdge[];
  timeline: TimelineEntry[];
  /** Whether the SSE stream is currently connected */
  streamConnected: boolean;
  /** Whether reconnection is in progress */
  reconnecting: boolean;
  /** Whether reconnection has permanently failed */
  reconnectFailed: boolean;
}

interface WorkflowStore {
  // ── State ──────────────────────────────────────────────────
  currentRun: WorkflowRunState;

  // ── Actions ─────────────────────────────────────────────────
  /** Reset state then start streaming for a new run */
  startRun: (runId: string, workflowName: string, totalSteps: number) => void;
  /** Handle an incoming SSE event */
  handleSSEEvent: (event: WorkflowSSEEvent) => void;
  /** Set DAG topology (nodes + edges) from the workflow definition */
  setDagTopology: (nodes: DagNode[], edges: DagEdge[]) => void;
  /** Mark stream as disconnected */
  setStreamConnected: (connected: boolean) => void;
  /** Mark reconnection state */
  setReconnecting: (reconnecting: boolean) => void;
  /** Mark reconnection as permanently failed */
  setReconnectFailed: (failed: boolean) => void;
  /** Bring down the current run and reset state */
  resetRun: () => void;
}

// ── Initial State ────────────────────────────────────────────

const initialRunState: WorkflowRunState = {
  runId: null,
  workflowName: "",
  status: "idle",
  totalSteps: 0,
  stepsCompleted: 0,
  stepsFailed: 0,
  stepsSkipped: 0,
  totalDurationMs: 0,
  error: null,
  dagNodes: [],
  dagEdges: [],
  timeline: [],
  streamConnected: false,
  reconnecting: false,
  reconnectFailed: false,
};

// ── Store ────────────────────────────────────────────────────

export const useWorkflowStore = create<WorkflowStore>((set, get) => ({
  currentRun: { ...initialRunState },

  startRun: (runId, workflowName, totalSteps) => {
    const prev = get().currentRun;
    set({
      currentRun: {
        ...initialRunState,
        runId,
        workflowName,
        totalSteps,
        status: "running",
        streamConnected: true,
        // Preserve DAG topology if already set
        dagNodes: prev.dagNodes.length > 0 ? prev.dagNodes : [],
        dagEdges: prev.dagEdges.length > 0 ? prev.dagEdges : [],
      },
    });
  },

  handleSSEEvent: (event) => {
    set((state) => {
      const run = { ...state.currentRun };
      const ts = new Date().toISOString();

      // Append to timeline
      run.timeline = [...run.timeline, { timestamp: ts, event }];

      switch (event.type) {
        case "workflow_started":
          run.runId = event.runId;
          run.workflowName = event.workflowName;
          run.totalSteps = event.totalSteps;
          run.status = "running";
          break;

        case "step_started":
          run.dagNodes = run.dagNodes.map((n) =>
            n.id === event.stepId ? { ...n, status: "running" } : n,
          );
          break;

        case "step_completed":
          run.stepsCompleted++;
          run.dagNodes = run.dagNodes.map((n) =>
            n.id === event.stepId ? { ...n, status: "success" } : n,
          );
          break;

        case "step_skipped":
          run.stepsSkipped++;
          run.dagNodes = run.dagNodes.map((n) =>
            n.id === event.stepId ? { ...n, status: "skipped" } : n,
          );
          break;

        case "step_failed":
          run.stepsFailed++;
          run.dagNodes = run.dagNodes.map((n) =>
            n.id === event.stepId ? { ...n, status: "failed" } : n,
          );
          break;

        case "workflow_paused":
          run.status = "paused";
          break;

        case "workflow_resumed":
          run.status = "running";
          break;

        case "workflow_completed":
          run.status = "completed";
          run.totalDurationMs = event.totalDurationMs;
          run.stepsCompleted = event.stepsCompleted;
          run.stepsFailed = event.stepsFailed;
          run.stepsSkipped = event.stepsSkipped;
          break;

        case "workflow_failed":
          run.status = "failed";
          run.error = event.error;
          run.totalDurationMs = event.totalDurationMs;
          break;

        case "workflow_cancelled":
          run.status = "cancelled";
          break;

        case "workflow_timed_out":
          run.status = "timed_out";
          run.error = event.error;
          run.totalDurationMs = event.totalDurationMs;
          break;

        case "done":
          run.streamConnected = false;
          break;
      }

      return { currentRun: run };
    });
  },

  setDagTopology: (nodes, edges) => {
    set((state) => ({
      currentRun: {
        ...state.currentRun,
        dagNodes: nodes,
        dagEdges: edges,
      },
    }));
  },

  setStreamConnected: (connected) => {
    set((state) => ({
      currentRun: { ...state.currentRun, streamConnected: connected },
    }));
  },

  setReconnecting: (reconnecting) => {
    set((state) => ({
      currentRun: { ...state.currentRun, reconnecting },
    }));
  },

  setReconnectFailed: (failed) => {
    set((state) => ({
      currentRun: { ...state.currentRun, reconnectFailed: failed },
    }));
  },

  resetRun: () => {
    set({ currentRun: { ...initialRunState } });
  },
}));

// ── Stream helper (not in store — call from components) ──────

/**
 * Connect to the SSE stream for a workflow execution and dispatch events
 * to the store. Handles reconnection automatically.
 *
 * Returns an AbortController that the caller can use to cancel the stream.
 */
export function connectWorkflowStream(runId: string): AbortController {
  const controller = new AbortController();
  const store = useWorkflowStore.getState();

  streamExecution(
    runId,
    (event) => {
      useWorkflowStore.getState().handleSSEEvent(event);
    },
    {
      signal: controller.signal,
      maxRetries: 3,
      onReconnecting: (attempt) => {
        useWorkflowStore.getState().setReconnecting(true);
        useWorkflowStore.getState().setStreamConnected(false);
      },
      onReconnectFailed: () => {
        useWorkflowStore.getState().setReconnectFailed(true);
        useWorkflowStore.getState().setReconnecting(false);
        useWorkflowStore.getState().setStreamConnected(false);
      },
    },
  )
    .then(() => {
      useWorkflowStore.getState().setStreamConnected(false);
      useWorkflowStore.getState().setReconnecting(false);
    })
    .catch(() => {
      // Already handled by onReconnectFailed or abort
    });

  return controller;
}
