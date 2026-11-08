// 成果时间线：按天分组，行 h-44。展开的详情留在行内 —— 台账不是要跳出去的页面，
// 展开就是全部动作。

import { useState } from "react";
import { TaskStateMark } from "@/components/common/TaskStateMark";
import { isNotableWakeState, WakeMark } from "@/components/common/WakeMark";
import { spaceLabel } from "@/components/layout/spaceLabel";
import { relativeTime } from "@/lib/relative";
import { parseServerTimestamp } from "@/lib/format";
import { wakeDescription } from "@/lib/wakeLabels";
import { cn } from "@/lib/utils";
import type { Space, Task } from "@/types/daemon";

interface DispatchTimelineProps {
  tasks: Task[];
  spaces: Space[];
}

function dayKey(task: Task): string {
  const raw = task.terminal_at ?? task.created_at;
  const date = parseServerTimestamp(raw);
  if (Number.isNaN(date.getTime())) return "未知日期";
  return `${date.getMonth() + 1}月${date.getDate()}日`;
}

/** 同一天的行聚在一起；顺序沿用服务端给的（新→旧），不重新排。 */
function groupByDay(tasks: Task[]): { day: string; rows: Task[] }[] {
  const groups: { day: string; rows: Task[] }[] = [];
  for (const task of tasks) {
    const day = dayKey(task);
    const last = groups[groups.length - 1];
    if (last && last.day === day) last.rows.push(task);
    else groups.push({ day, rows: [task] });
  }
  return groups;
}

export function DispatchTimeline({ tasks, spaces }: DispatchTimelineProps) {
  const [expanded, setExpanded] = useState<string | null>(null);
  const nameOf = (wsId: string) => {
    const space = spaces.find((item) => item.ws_id === wsId);
    return space ? spaceLabel(space) : wsId;
  };

  return (
    <div>
      {groupByDay(tasks).map((group) => (
        <section key={group.day}>
          <h2 className="flex h-8 items-center text-xs font-semibold text-ink-muted">
            {group.day}
            <span className="ml-2 tabular-nums text-2xs text-ink-faint">
              {group.rows.length}
            </span>
          </h2>
          <ul>
            {group.rows.map((task) => {
              const open = expanded === task.id;
              return (
                <li key={task.id} className="border-b border-line">
                  <button
                    type="button"
                    onClick={() => setExpanded(open ? null : task.id)}
                    aria-expanded={open}
                    className="flex min-h-11 w-full flex-col justify-center gap-0.5 py-1 text-left @min-[900px]/work:h-11 @min-[900px]/work:flex-row @min-[900px]/work:items-center @min-[900px]/work:gap-2 @min-[900px]/work:py-0"
                  >
                    <span className="shrink-0 tabular-nums text-2xs text-ink-faint @min-[900px]/work:w-24">
                      {relativeTime(task.terminal_at ?? task.created_at)}
                    </span>
                    <TaskStateMark
                      taskState={task.task_state}
                      className="shrink-0"
                    />
                    {isNotableWakeState(task.wake_state) && (
                      <WakeMark
                        wakeState={task.wake_state}
                        wakeSkipReason={task.wake_skip_reason}
                        className="shrink-0"
                      />
                    )}
                    <span className="min-w-0 shrink-0 truncate font-mono text-2xs text-ink-faint @min-[900px]/work:w-56">
                      {nameOf(task.origin_ws_id)} → {nameOf(task.target_ws_id)}
                    </span>
                    <span className="shrink-0 truncate text-2xs text-ink-muted @min-[900px]/work:w-24">
                      {task.agent_name}
                    </span>
                    <span className="min-w-0 flex-1 truncate text-sm text-ink">
                      {task.result_digest ?? task.task_id}
                    </span>
                  </button>
                  {open && (
                    <div className="space-y-1 border-t border-line bg-canvas/40 px-3 py-2">
                      <p className="font-mono text-2xs text-ink-faint">
                        task_id {task.task_id} · 来源 {task.origin_kind} · agent{" "}
                        {task.agent_name}
                      </p>
                      <p
                        className={cn(
                          "max-w-[640px] whitespace-pre-wrap break-words text-sm",
                          task.result_digest ? "text-ink" : "text-ink-faint",
                        )}
                      >
                        {task.result_digest ?? "这条任务没有摘要。"}
                      </p>
                      {isNotableWakeState(task.wake_state) && (
                        <p className="text-2xs text-ink-muted">
                          唤醒：
                          {wakeDescription(
                            task.wake_state,
                            task.wake_skip_reason,
                          )}
                        </p>
                      )}
                      <p className="font-mono text-2xs text-ink-faint">
                        {task.created_at} → {task.terminal_at ?? "未终态"}
                      </p>
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        </section>
      ))}
    </div>
  );
}
