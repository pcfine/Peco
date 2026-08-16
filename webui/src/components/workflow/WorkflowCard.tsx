import { Card, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { DagPreview } from "./DagPreview";
import type { WorkflowListItem, DagNode, DagEdge } from "@/types/workflow";
import {
  Clock,
  Play,
  Pencil,
  MoreHorizontal,
  CheckCircle2,
  XCircle,
  Timer,
} from "lucide-react";

interface WorkflowCardProps {
  workflow: WorkflowListItem;
  onExecute: (name: string) => void;
  onEdit: (name: string) => void;
  onView: (name: string) => void;
}

/** Extract simple DAG topology from step count (we don't have step details in list) */
function buildMiniDag(workflow: WorkflowListItem): {
  nodes: DagNode[];
  edges: DagEdge[];
} {
  // Phase 1: list endpoint doesn't include full step definitions.
  // The card shows step count and schedule info; detailed DAG shown on detail page.
  return { nodes: [], edges: [] };
}

function formatDuration(ms?: number): string {
  if (ms === undefined || ms === null) return "—";
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.floor(ms / 60000)}m ${Math.round((ms % 60000) / 1000)}s`;
}

function timeAgo(dateStr?: string): string {
  if (!dateStr) return "—";
  const diff = Date.now() - new Date(dateStr).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "刚刚";
  if (mins < 60) return `${mins}分钟前`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}小时前`;
  return `${Math.floor(hours / 24)}天前`;
}

export function WorkflowCard({
  workflow,
  onExecute,
  onEdit,
  onView,
}: WorkflowCardProps) {
  const { nodes, edges } = buildMiniDag(workflow);
  const hasDag = nodes.length > 0;
  const lastExec = workflow.lastExecution;

  return (
    <Card
      className="group hover:border-primary/40 transition-colors cursor-pointer"
      onClick={() => onView(workflow.name)}
    >
      <CardContent className="p-4 space-y-3">
        {/* Header */}
        <div className="flex items-start justify-between gap-2">
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <span className="font-semibold truncate">{workflow.name}</span>
              {workflow.version && (
                <span className="shrink-0 text-xs text-muted-foreground bg-muted px-1.5 py-0.5 rounded">
                  v{workflow.version}
                </span>
              )}
            </div>
            {workflow.description && (
              <p className="text-xs text-muted-foreground line-clamp-2 mt-0.5">
                {workflow.description}
              </p>
            )}
          </div>

          {/* Actions — visible on hover */}
          <div
            className="flex gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity shrink-0"
            onClick={(e) => e.stopPropagation()}
          >
            <Button
              variant="ghost"
              size="icon"
              className="h-8 w-8"
              onClick={() => onExecute(workflow.name)}
              title="执行"
            >
              <Play className="h-4 w-4" />
            </Button>
            <Button
              variant="ghost"
              size="icon"
              className="h-8 w-8"
              onClick={() => onEdit(workflow.name)}
              title="编辑"
            >
              <Pencil className="h-4 w-4" />
            </Button>
          </div>
        </div>

        {/* Mini DAG or step count */}
        {hasDag ? (
          <DagPreview
            nodes={nodes}
            edges={edges}
            width={300}
            height={80}
            className="rounded bg-muted/20"
          />
        ) : (
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <span className="bg-muted px-2 py-0.5 rounded">
              {workflow.stepCount} 个步骤
            </span>
          </div>
        )}

        {/* Schedule info */}
        {workflow.schedule ? (
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <Clock className="h-3 w-3" />
            <span>
              {workflow.schedule.cron}
              {workflow.schedule.enabled ? (
                <span className="ml-1 text-green-600 font-medium">已启用</span>
              ) : (
                <span className="ml-1 text-amber-600 font-medium">已暂停</span>
              )}
            </span>
          </div>
        ) : (
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <span className="text-xs">🖐 仅手动触发</span>
          </div>
        )}

        {/* Last execution summary */}
        {lastExec && (
          <div className="flex items-center gap-2 text-xs text-muted-foreground border-t pt-2">
            {lastExec.status === "completed" ? (
              <CheckCircle2 className="h-3 w-3 text-green-500" />
            ) : lastExec.status === "failed" ? (
              <XCircle className="h-3 w-3 text-red-500" />
            ) : lastExec.status === "timed_out" ? (
              <Timer className="h-3 w-3 text-orange-500" />
            ) : (
              <Timer className="h-3 w-3" />
            )}
            <span>{timeAgo(lastExec.startedAt)}</span>
            <span>·</span>
            <span>{formatDuration(lastExec.totalDurationMs)}</span>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
