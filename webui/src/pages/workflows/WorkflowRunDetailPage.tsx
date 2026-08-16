import { useEffect, useRef, useCallback } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { Button } from "@/components/ui/button";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { DagExecutionView } from "@/components/workflow/DagExecutionView";
import { EventTimeline } from "@/components/workflow/EventTimeline";
import {
  useWorkflowStore,
  connectWorkflowStream,
} from "@/stores/workflowStore";
import { getExecution, hydrateFromSnapshot } from "@/api/workflows";
import {
  ArrowLeft,
  RefreshCw,
  AlertTriangle,
  Wifi,
  WifiOff,
} from "lucide-react";
import { toast } from "sonner";
import { cn } from "@/lib/utils";

export function WorkflowRunDetailPage() {
  const navigate = useNavigate();
  const { runId } = useParams<{ runId: string }>();
  const store = useWorkflowStore();
  const { currentRun } = store;
  const abortRef = useRef<AbortController | null>(null);
  const unmountedRef = useRef(false);

  // Load initial execution data then connect to SSE
  useEffect(() => {
    if (!runId) return;
    unmountedRef.current = false;

    const init = async () => {
      // Load initial snapshot for DAG topology and current state
      try {
        const detail = await getExecution(runId);
        if (unmountedRef.current) return;

        // Set DAG topology from step results
        const nodes = detail.stepResults.map((s) => ({
          id: s.stepId,
          name: s.stepName,
          type: (s.stepType as "shell" | "agent" | "llm" | "tool") || "shell",
          status:
            s.outcome === "success"
              ? ("success" as const)
              : s.outcome === "failed"
                ? ("failed" as const)
                : s.outcome === "skipped"
                  ? ("skipped" as const)
                  : ("pending" as const),
        }));

        store.setDagTopology(nodes, []);

        // If already terminal, hydrate the full snapshot (step results + terminal
        // state) via the shared mapper, so step counts and status stay correct.
        if (
          detail.summary.status === "completed" ||
          detail.summary.status === "failed" ||
          detail.summary.status === "cancelled" ||
          detail.summary.status === "timed_out"
        ) {
          store.startRun(
            runId,
            detail.summary.workflowName,
            detail.summary.totalSteps,
          );
          hydrateFromSnapshot(detail, store.handleSSEEvent);
          return;
        }

        // Start and connect SSE
        store.startRun(
          runId,
          detail.summary.workflowName,
          detail.summary.totalSteps,
        );
        const controller = connectWorkflowStream(runId);
        if (!unmountedRef.current) abortRef.current = controller;
      } catch (err) {
        if (!unmountedRef.current) {
          toast.error("加载执行详情失败");
        }
      }
    };

    init();

    return () => {
      unmountedRef.current = true;
      abortRef.current?.abort();
    };
  }, [runId]);

  const handleRetry = useCallback(() => {
    if (!runId) return;
    abortRef.current?.abort();
    store.setReconnectFailed(false);
    const controller = connectWorkflowStream(runId);
    if (!unmountedRef.current) abortRef.current = controller;
  }, [runId]);

  const isTerminal =
    currentRun.status === "completed" ||
    currentRun.status === "failed" ||
    currentRun.status === "cancelled" ||
    currentRun.status === "timed_out";

  const statusLabel = {
    idle: "空闲",
    running: "运行中",
    paused: "已暂停",
    completed: "已完成",
    failed: "失败",
    cancelled: "已取消",
    timed_out: "超时",
  }[currentRun.status];

  const statusColor = {
    idle: "text-muted-foreground",
    running: "text-blue-600",
    paused: "text-amber-600",
    completed: "text-green-600",
    failed: "text-red-600",
    cancelled: "text-gray-500",
    timed_out: "text-orange-600",
  }[currentRun.status];

  if (!runId) {
    return (
      <div className="text-center py-12 text-muted-foreground">缺少 Run ID</div>
    );
  }

  return (
    <div className="max-w-5xl mx-auto space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate("/workflows")}
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <div className="flex items-center gap-2">
              <h2 className="text-lg font-bold">
                {currentRun.workflowName || "Workflow 执行"}
              </h2>
              <span className={cn("text-sm font-medium", statusColor)}>
                {statusLabel}
              </span>
              {/* Connection indicator */}
              {!isTerminal &&
                (currentRun.streamConnected ? (
                  <Wifi
                    className="h-3.5 w-3.5 text-green-500"
                    title="SSE 已连接"
                  />
                ) : currentRun.reconnecting ? (
                  <WifiOff
                    className="h-3.5 w-3.5 text-amber-500 animate-pulse"
                    title="重连中..."
                  />
                ) : (
                  <WifiOff
                    className="h-3.5 w-3.5 text-red-500"
                    title="已断开"
                  />
                ))}
            </div>
            <p className="text-xs text-muted-foreground font-mono mt-0.5">
              Run ID: {runId}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {isTerminal && (
            <span className="text-xs text-muted-foreground">
              总耗时:{" "}
              {currentRun.totalDurationMs > 0
                ? `${(currentRun.totalDurationMs / 1000).toFixed(1)}s`
                : "—"}
            </span>
          )}
        </div>
      </div>

      {/* Reconnection failed banner */}
      {currentRun.reconnectFailed && (
        <div className="flex items-center gap-2 rounded-md bg-amber-50 border border-amber-200 p-3 text-sm">
          <AlertTriangle className="h-4 w-4 text-amber-600 shrink-0" />
          <span className="text-amber-800 flex-1">
            实时连接失败。以下为最后一次获取的快照数据。
          </span>
          <Button variant="outline" size="sm" onClick={handleRetry}>
            <RefreshCw className="mr-1 h-3 w-3" />
            重试连接
          </Button>
        </div>
      )}

      {/* DAG + Timeline */}
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
        {/* Live DAG */}
        <div className="lg:col-span-2 border rounded-md p-4">
          <h4 className="text-sm font-medium mb-3">执行状态</h4>
          <DagExecutionView
            nodes={currentRun.dagNodes}
            edges={currentRun.dagEdges}
          />
        </div>

        {/* Summary sidebar */}
        <div className="space-y-4">
          <div className="border rounded-md p-3 space-y-2">
            <div className="flex justify-between text-xs">
              <span className="text-muted-foreground">步骤进度</span>
              <span className="font-mono">
                {currentRun.stepsCompleted}/{currentRun.totalSteps}
              </span>
            </div>
            <div className="flex justify-between text-xs">
              <span className="text-muted-foreground">失败</span>
              <span className="font-mono text-red-600">
                {currentRun.stepsFailed}
              </span>
            </div>
            <div className="flex justify-between text-xs">
              <span className="text-muted-foreground">跳过</span>
              <span className="font-mono text-amber-600">
                {currentRun.stepsSkipped}
              </span>
            </div>
          </div>

          {/* Event timeline */}
          <div>
            <h4 className="text-sm font-medium mb-2">事件时间线</h4>
            <EventTimeline timeline={currentRun.timeline} />
          </div>
        </div>
      </div>
    </div>
  );
}
