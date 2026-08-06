import type { TimelineEntry } from "@/stores/workflowStore";
import { cn } from "@/lib/utils";
import {
  Play,
  CheckCircle2,
  XCircle,
  SkipForward,
  Pause,
  Flag,
  Loader2,
} from "lucide-react";

interface EventTimelineProps {
  timeline: TimelineEntry[];
  className?: string;
}

function eventIcon(eventType: string) {
  const cls = "h-4 w-4 shrink-0";
  switch (eventType) {
    case "workflow_started":
      return <Play className={cls} style={{ color: "#3b82f6" }} />;
    case "step_started":
      return (
        <Loader2
          className={cn(cls, "animate-spin")}
          style={{ color: "#3b82f6" }}
        />
      );
    case "step_completed":
      return <CheckCircle2 className={cls} style={{ color: "#22c55e" }} />;
    case "step_failed":
      return <XCircle className={cls} style={{ color: "#ef4444" }} />;
    case "step_skipped":
      return <SkipForward className={cls} style={{ color: "#fbbf24" }} />;
    case "workflow_paused":
      return <Pause className={cls} style={{ color: "#f59e0b" }} />;
    case "workflow_completed":
      return <CheckCircle2 className={cls} style={{ color: "#22c55e" }} />;
    case "workflow_failed":
      return <XCircle className={cls} style={{ color: "#ef4444" }} />;
    case "workflow_cancelled":
      return <Flag className={cls} style={{ color: "#6b7280" }} />;
    default:
      return <div className={cn(cls, "rounded-full bg-muted-foreground/30")} />;
  }
}

function eventLabel(entry: TimelineEntry): string {
  const { event } = entry;
  switch (event.type) {
    case "workflow_started":
      return `Workflow 开始 — ${event.workflowName}（${event.totalSteps} 步骤）`;
    case "step_started":
      return `${event.stepName} (${event.stepType}) 开始执行`;
    case "step_completed":
      return `${event.stepName} 完成 (${event.durationMs}ms, 第 ${event.attempt} 次尝试)`;
    case "step_failed":
      return `${event.stepName} 失败: ${event.error} (${event.durationMs}ms)`;
    case "step_skipped":
      return `${event.stepName} 跳过: ${event.reason}`;
    case "workflow_paused":
      return `Workflow 暂停: ${event.reason}${event.pausedAtStep ? ` (步骤: ${event.pausedAtStep})` : ""}`;
    case "workflow_resumed":
      return "Workflow 继续执行";
    case "workflow_completed":
      return `Workflow 完成 — ${event.stepsCompleted} 成功, ${event.stepsFailed} 失败, ${event.stepsSkipped} 跳过 (${event.totalDurationMs}ms)`;
    case "workflow_failed":
      return `Workflow 失败: ${event.error}${event.failedAtStep ? ` (步骤: ${event.failedAtStep})` : ""}`;
    case "workflow_cancelled":
      return "Workflow 已取消";
    case "done":
      return "SSE 流结束";
    default:
      return event.type;
  }
}

/**
 * Vertical timeline of SSE events for a workflow execution.
 */
export function EventTimeline({ timeline, className }: EventTimelineProps) {
  if (timeline.length === 0) {
    return (
      <div
        className={cn(
          "text-sm text-muted-foreground py-8 text-center",
          className,
        )}
      >
        暂无事件
      </div>
    );
  }

  return (
    <div className={cn("space-y-0", className)}>
      {timeline.map((entry, i) => (
        <div key={i} className="relative flex gap-3 pb-3">
          {/* Vertical line */}
          {i < timeline.length - 1 && (
            <div className="absolute left-[11px] top-6 bottom-0 w-px bg-border" />
          )}

          {/* Icon */}
          <div className="relative z-10 flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-background border mt-0.5">
            {eventIcon(entry.event.type)}
          </div>

          {/* Content */}
          <div className="flex-1 min-w-0">
            <p className="text-sm leading-relaxed">{eventLabel(entry)}</p>
            <p className="text-xs text-muted-foreground mt-0.5">
              {new Date(entry.timestamp).toLocaleTimeString()}
            </p>
          </div>
        </div>
      ))}
    </div>
  );
}
