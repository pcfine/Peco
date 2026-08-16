import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import type { ExecutionSummary } from "@/types/workflow";
import { CheckCircle2, XCircle, Clock, Ban, Pause, Timer } from "lucide-react";
import { cn } from "@/lib/utils";

interface ExecutionHistoryTableProps {
  executions: ExecutionSummary[];
  onViewRun: (runId: string) => void;
  loading?: boolean;
}

const STATUS_CONFIG: Record<
  string,
  { icon: React.ReactNode; label: string; className: string }
> = {
  completed: {
    icon: <CheckCircle2 className="h-3.5 w-3.5" />,
    label: "成功",
    className: "text-green-600",
  },
  failed: {
    icon: <XCircle className="h-3.5 w-3.5" />,
    label: "失败",
    className: "text-red-600",
  },
  running: {
    icon: <Clock className="h-3.5 w-3.5 animate-pulse" />,
    label: "运行中",
    className: "text-blue-600",
  },
  cancelled: {
    icon: <Ban className="h-3.5 w-3.5" />,
    label: "已取消",
    className: "text-gray-500",
  },
  paused: {
    icon: <Pause className="h-3.5 w-3.5" />,
    label: "已暂停",
    className: "text-amber-600",
  },
  timed_out: {
    icon: <Timer className="h-3.5 w-3.5" />,
    label: "超时",
    className: "text-orange-600",
  },
};

function formatDuration(ms?: number): string {
  if (ms === undefined || ms === null) return "—";
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.floor(ms / 60000)}m ${Math.round((ms % 60000) / 1000)}s`;
}

export function ExecutionHistoryTable({
  executions,
  onViewRun,
  loading,
}: ExecutionHistoryTableProps) {
  if (loading) {
    return (
      <div className="text-sm text-muted-foreground py-8 text-center">
        加载中...
      </div>
    );
  }

  if (executions.length === 0) {
    return (
      <div className="text-sm text-muted-foreground py-8 text-center">
        暂无执行记录
      </div>
    );
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead className="w-[200px]">Run ID</TableHead>
          <TableHead>触发方式</TableHead>
          <TableHead>状态</TableHead>
          <TableHead className="text-right">步骤进度</TableHead>
          <TableHead className="text-right">耗时</TableHead>
          <TableHead>开始时间</TableHead>
          <TableHead className="w-[80px]"></TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {executions.map((exec) => {
          const statusCfg = STATUS_CONFIG[exec.status] ?? STATUS_CONFIG.failed;
          return (
            <TableRow key={exec.runId}>
              <TableCell className="font-mono text-xs">
                {exec.runId.slice(0, 8)}...
              </TableCell>
              <TableCell className="text-xs">
                {exec.triggerType === "scheduled" ? "⏰ 定时" : "🖐 手动"}
              </TableCell>
              <TableCell>
                <span
                  className={cn(
                    "inline-flex items-center gap-1 text-xs font-medium",
                    statusCfg.className,
                  )}
                >
                  {statusCfg.icon}
                  {statusCfg.label}
                </span>
              </TableCell>
              <TableCell className="text-right text-xs">
                {exec.stepsCompleted}/{exec.totalSteps}
                {exec.stepsFailed > 0 && (
                  <span className="text-red-500 ml-1">
                    ({exec.stepsFailed} 失败)
                  </span>
                )}
              </TableCell>
              <TableCell className="text-right text-xs font-mono">
                {formatDuration(exec.totalDurationMs)}
              </TableCell>
              <TableCell className="text-xs text-muted-foreground">
                {new Date(exec.startedAt).toLocaleString()}
              </TableCell>
              <TableCell>
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7 text-xs"
                  onClick={() => onViewRun(exec.runId)}
                >
                  详情
                </Button>
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}
