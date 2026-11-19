// 一行一条定时任务：名称/目标、表达式、下次运行、最近状态、启用开关、操作菜单。
// 运行历史在行内展开 —— 历史不是要跳出去的页面，展开就是全部动作。

import { Link } from "react-router-dom";
import { MoreHorizontal } from "lucide-react";
import { toast } from "sonner";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cronPresetLabel } from "@/components/schedules/CronPresets";
import {
  useRunSchedule,
  useScheduleRuns,
  useUpdateSchedule,
} from "@/queries/schedules";
import { errorText } from "@/lib/api";
import { formatCreatedAt, parseServerTimestamp } from "@/lib/format";
import { formatDuration } from "@/lib/time";
import { relativeUntil } from "@/lib/relative";
import { runErrorText } from "@/lib/scheduleRuns";
import { cn } from "@/lib/utils";
import type {
  ScheduleRun,
  ScheduleRunStatus,
  ScheduleView,
} from "@/types/daemon";

/** 服务端的重试预算：`pending` 行按它报「第 N/10 次」。 */
const RETRY_BUDGET = 10;

const STATUS_LABELS: Record<ScheduleRunStatus, string> = {
  pending: "等待重试",
  running: "运行中",
  completed: "已完成",
  failed: "失败",
  skipped: "跳过",
  timeout: "超时",
  interrupted: "中断",
};

/** 状态色只是辅助，文字标签同时给 —— 只靠颜色分不出「失败」和「超时」。 */
const STATUS_DOT: Record<ScheduleRunStatus, string> = {
  pending: "bg-warn-mark",
  running: "bg-brand",
  completed: "bg-ok",
  failed: "bg-danger",
  skipped: "bg-ink-faint",
  timeout: "bg-danger",
  interrupted: "bg-ink-faint",
};

function StatusMark({
  status,
  className,
}: {
  status: ScheduleRunStatus;
  className?: string;
}) {
  return (
    <span className={cn("inline-flex min-w-0 items-center gap-1.5", className)}>
      <span
        aria-hidden="true"
        className={cn("size-2 shrink-0 rounded-[1px]", STATUS_DOT[status])}
      />
      <span className="truncate text-caption text-ink-muted">
        {STATUS_LABELS[status]}
      </span>
    </span>
  );
}

/** 本地时刻，解析不了给「—」：空串会在列里留下一个看不出原因的洞。 */
function localTime(raw: string | null): string {
  if (!raw) return "—";
  return formatCreatedAt(raw) || "—";
}

function runDuration(run: ScheduleRun): string | null {
  if (run.finished_at === null) return null;
  const started = parseServerTimestamp(run.started_at).getTime();
  const finished = parseServerTimestamp(run.finished_at).getTime();
  if (Number.isNaN(started) || Number.isNaN(finished)) return null;
  return formatDuration(finished - started);
}

/** 摘要列：等待重试要说清原因，其余优先给结果，没有结果才退回错误。 */
function runSummary(run: ScheduleRun): string {
  if (run.status === "pending") {
    return `等待重试（第 ${run.attempts}/${RETRY_BUDGET} 次，目标会话忙）`;
  }
  if (run.result_digest) return run.result_digest;
  if (run.error) return runErrorText(run.error);
  return run.status === "running" ? "运行中…" : "—";
}

function RunHistory({ schedule }: { schedule: ScheduleView }) {
  const runs = useScheduleRuns(schedule.id);

  if (runs.isPending) {
    return (
      <div className="space-y-px px-3 py-2">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-6 w-full" />
        ))}
      </div>
    );
  }

  if (runs.error) {
    return (
      <div className="px-3 py-2">
        <ErrorBanner
          message={errorText(runs.error)}
          onRetry={() => void runs.refetch()}
        />
      </div>
    );
  }

  const items = runs.data ?? [];
  if (items.length === 0) {
    return (
      <p className="px-3 py-2 text-caption text-ink-faint">
        这个任务还没有运行记录
      </p>
    );
  }

  return (
    <div className="px-3 py-2">
      <ul>
        {items.map((run) => (
          <li
            key={run.id}
            className="flex min-w-0 flex-col gap-0.5 border-b border-line py-1 last:border-b-0 @min-[900px]/work:flex-row @min-[900px]/work:items-center @min-[900px]/work:gap-3 @min-[900px]/work:py-0.5"
          >
            <span
              className="shrink-0 tabular-nums text-caption text-ink-muted @min-[900px]/work:w-32"
              title={`计划 ${localTime(run.scheduled_for)}`}
            >
              {localTime(run.started_at)}
            </span>
            <span className="shrink-0 text-caption text-ink-faint @min-[900px]/work:w-12">
              {run.trigger_kind === "manual" ? "手动" : "定时"}
            </span>
            <StatusMark
              status={run.status}
              className="@min-[900px]/work:w-24 @min-[900px]/work:shrink-0"
            />
            <span className="shrink-0 tabular-nums text-caption text-ink-muted @min-[900px]/work:w-20">
              {runDuration(run) ?? "—"}
            </span>
            <span className="min-w-0 flex-1 break-words text-caption text-ink">
              {runSummary(run)}
            </span>
            {run.status === "completed" && (
              <Link
                to={`/spaces/${schedule.target_ws_id}`}
                className="inline-flex h-8 w-fit shrink-0 items-center text-caption font-medium text-primary-ink hover:underline"
              >
                去看这一轮
              </Link>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

interface ScheduleRowProps {
  schedule: ScheduleView;
  expanded: boolean;
  onToggleHistory: () => void;
  onEdit: () => void;
  onDelete: () => void;
}

export function ScheduleRow({
  schedule,
  expanded,
  onToggleHistory,
  onEdit,
  onDelete,
}: ScheduleRowProps) {
  const update = useUpdateSchedule();
  const run = useRunSchedule();

  const target = schedule.target_label.trim() || schedule.target_ws_id;
  const preset = cronPresetLabel(schedule.cron_expr);

  return (
    <li className="border-b border-line">
      <div className="flex min-w-0 flex-col gap-1 py-2 @min-[900px]/work:flex-row @min-[900px]/work:items-center @min-[900px]/work:gap-3">
        <div className="flex min-w-0 flex-col @min-[900px]/work:w-56 @min-[900px]/work:shrink-0">
          <span
            className="min-w-0 truncate text-sm font-medium text-ink"
            title={schedule.name}
          >
            {schedule.name}
          </span>
          <span
            className="min-w-0 truncate text-caption text-ink-muted"
            title={target}
          >
            {target}
          </span>
        </div>

        <div className="flex min-w-0 flex-1 items-baseline gap-2">
          <code
            translate="no"
            className="min-w-0 font-mono text-caption text-ink-muted"
          >
            {schedule.cron_expr}
          </code>
          {preset && (
            <span className="min-w-0 truncate text-caption text-ink-faint">
              {preset}
            </span>
          )}
        </div>

        {/* 下次运行与最近状态在窄档共用一行：单行一列会把手机上的行拉到四层。 */}
        <div className="flex shrink-0 items-center gap-3">
          <div className="tabular-nums text-caption text-ink-muted @min-[900px]/work:w-28">
            <span className="text-ink-faint">下次 </span>
            <span
              title={
                schedule.next_run_at
                  ? localTime(schedule.next_run_at)
                  : undefined
              }
            >
              {schedule.next_run_at
                ? relativeUntil(schedule.next_run_at)
                : "已停用"}
            </span>
          </div>

          <div className="@min-[900px]/work:w-24">
            {schedule.last_status ? (
              <StatusMark status={schedule.last_status} />
            ) : (
              <span className="text-caption text-ink-faint">未运行</span>
            )}
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-1">
          <Switch
            checked={schedule.enabled}
            disabled={update.isPending}
            aria-label={`启用定时任务 ${schedule.name}`}
            className="relative after:absolute after:inset-x-0 after:-inset-y-2 after:content-['']"
            onCheckedChange={(next) =>
              update.mutate(
                { id: schedule.id, body: { enabled: next } },
                { onError: (error) => toast.error(errorText(error)) },
              )
            }
          />
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button
                type="button"
                aria-label={`${schedule.name} 的操作`}
                className="grid size-8 place-items-center rounded-md text-ink-faint hover:bg-line hover:text-ink"
              >
                <MoreHorizontal className="size-4" />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-40">
              <DropdownMenuItem onSelect={onEdit}>
                编辑定时任务
              </DropdownMenuItem>
              <DropdownMenuItem
                disabled={run.isPending}
                onSelect={() =>
                  run.mutate(schedule.id, {
                    onError: (error) => toast.error(errorText(error)),
                  })
                }
              >
                立即运行
              </DropdownMenuItem>
              <DropdownMenuItem onSelect={onToggleHistory}>
                {expanded ? "收起运行历史" : "运行历史"}
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem
                className="text-danger focus:text-danger"
                onSelect={onDelete}
              >
                删除定时任务
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      {expanded && (
        <div className="border-t border-line bg-canvas/40">
          <RunHistory schedule={schedule} />
        </div>
      )}
    </li>
  );
}
