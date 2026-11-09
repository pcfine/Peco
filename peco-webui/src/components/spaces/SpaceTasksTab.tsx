// 任务台账：状态是文字 + 方点，不是 pill（pill 会让一列 8 种状态变成 8 种颜色）。
// 摘要默认截断，点开整行展开全文 —— 台账是排查入口，不是阅读入口。

import { useState } from "react";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { ConnectionStatus } from "@/components/common/ConnectionStatus";
import { TaskStateMark } from "@/components/common/TaskStateMark";
import { isNotableWakeState, WakeMark } from "@/components/common/WakeMark";
import { useConnectionStore } from "@/stores/connectionStore";
import { errorText } from "@/lib/api";
import { formatCreatedAt, parseServerTimestamp } from "@/lib/format";
import { wakeDescription } from "@/lib/wakeLabels";
import { formatDuration } from "@/lib/time";
import { cn } from "@/lib/utils";
import type { Task } from "@/types/daemon";

interface SpaceTasksTabProps {
  tasks: Task[];
  isPending: boolean;
  error: unknown;
  onRetry: () => void;
}

/** 在飞任务没有终态时刻：耗时按「提交到现在」算，并标出这是进行中的一段。 */
function durationOf(task: Task): string | null {
  const start = parseServerTimestamp(task.created_at).getTime();
  const end = task.terminal_at
    ? parseServerTimestamp(task.terminal_at).getTime()
    : Date.now();
  if (Number.isNaN(start) || Number.isNaN(end)) return null;
  return formatDuration(end - start);
}

export function SpaceTasksTab({
  tasks,
  isPending,
  error,
  onRetry,
}: SpaceTasksTabProps) {
  const [expanded, setExpanded] = useState<string | null>(null);
  const eventsState = useConnectionStore((state) => state.events);

  if (error) {
    return (
      <div className="p-3">
        <ErrorBanner message={errorText(error)} onRetry={onRetry} />
      </div>
    );
  }

  if (isPending) {
    return (
      <div className="space-y-px p-3">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-9 w-full" />
        ))}
      </div>
    );
  }

  if (tasks.length === 0) {
    return (
      <div className="p-3">
        <EmptyState title="这个空间还没有任务。Peco 派发到这里后会出现在台账里。" />
      </div>
    );
  }

  return (
    <div className="h-full overflow-y-auto">
      {/* 列跟着面板宽度逐级出现：481 只留状态与 agent，616 再加耗时，840 以上才是六列。 */}
      <div className="sticky top-0 z-10 flex h-8 items-center gap-2 border-b border-line bg-panel px-3 text-caption font-medium text-ink-faint">
        <ConnectionStatus state={eventsState} className="mr-2 shrink-0" />
        <span className="w-36 shrink-0">状态</span>
        <span className="w-24 shrink-0">agent</span>
        <span className="hidden w-32 shrink-0 @min-[840px]/panel:block">
          来源
        </span>
        <span className="hidden w-20 shrink-0 text-right @min-[520px]/panel:block">
          耗时
        </span>
        <span className="hidden w-28 shrink-0 text-right @min-[840px]/panel:block">
          提交
        </span>
        <span className="min-w-0 flex-1">摘要</span>
      </div>
      <ul>
        {tasks.map((task) => {
          const open = expanded === task.id;
          return (
            <li key={task.id} className="border-b border-line">
              <button
                type="button"
                onClick={() => setExpanded(open ? null : task.id)}
                aria-expanded={open}
                className="flex h-9 w-full items-center gap-2 px-3 text-left hover:bg-line/30"
              >
                <span className="flex w-36 shrink-0 flex-col gap-0.5">
                  <TaskStateMark taskState={task.task_state} />
                  {isNotableWakeState(task.wake_state) && (
                    <WakeMark
                      wakeState={task.wake_state}
                      wakeSkipReason={task.wake_skip_reason}
                    />
                  )}
                </span>
                <span className="w-24 shrink-0 truncate text-sm text-ink">
                  {task.agent_name}
                </span>
                <span className="hidden w-32 shrink-0 truncate font-mono text-2xs text-ink-faint @min-[840px]/panel:block">
                  {task.origin_kind}
                </span>
                <span className="hidden w-20 shrink-0 text-right tabular-nums text-2xs text-ink-faint @min-[520px]/panel:block">
                  {durationOf(task) ?? "—"}
                </span>
                <span className="hidden w-28 shrink-0 text-right tabular-nums text-2xs text-ink-faint @min-[840px]/panel:block">
                  {formatCreatedAt(task.created_at)}
                </span>
                <span
                  className={cn(
                    "min-w-0 flex-1 truncate text-sm text-ink-muted",
                    !task.result_digest && "text-ink-faint",
                  )}
                >
                  {task.result_digest ?? task.task_id}
                </span>
              </button>
              {open && (
                <div className="space-y-1 border-t border-line bg-canvas/40 px-3 py-2">
                  <p className="font-mono text-caption font-medium text-ink-faint">
                    task_id {task.task_id} · 目标 {task.target_ws_id} · 来源{" "}
                    {task.origin_ws_id}
                  </p>
                  <p className="text-caption font-medium text-ink-faint">
                    提交{" "}
                    <span className="tabular-nums">
                      {formatCreatedAt(task.created_at)}
                    </span>
                  </p>
                  <div className="space-y-0.5">
                    <p className="text-caption font-medium text-ink-faint">
                      Peco 让它做什么
                    </p>
                    <p
                      className={cn(
                        "max-w-narrow whitespace-pre-wrap break-words text-sm",
                        task.prompt_digest ? "text-ink" : "text-ink-faint",
                      )}
                    >
                      {task.prompt_digest ?? "未记录"}
                    </p>
                  </div>
                  <p className="text-caption font-medium text-ink-faint">
                    结果摘要
                  </p>
                  <p className="max-w-narrow whitespace-pre-wrap break-words text-sm text-ink">
                    {task.result_digest ?? "这条任务没有摘要。"}
                  </p>
                  {isNotableWakeState(task.wake_state) && (
                    <p className="text-caption font-medium text-ink-muted">
                      唤醒：
                      {wakeDescription(task.wake_state, task.wake_skip_reason)}
                    </p>
                  )}
                </div>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
