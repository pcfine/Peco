// 台账与成果页共用的状态刻度：8px 方点 + 文字状态。两处各写一份，
// 「哪个状态算失败」就会有两份定义，某天只改一处就会让两页对同一条任务各说各话。

import { cn } from "@/lib/utils";

export type TaskStateGroup =
  "active" | "waiting" | "done" | "failed" | "orphan";

const DOT_CLASS: Record<TaskStateGroup, string> = {
  active: "bg-brand",
  waiting: "bg-warn",
  done: "bg-ok",
  failed: "bg-danger",
  orphan: "bg-ink-faint",
};

/** 状态的组别；未列举的状态按「在飞」处理，比按失败处理更保守。 */
export function taskStateGroup(state: string): TaskStateGroup {
  if (state === "completed") return "done";
  if (state === "orphan") return "orphan";
  if (state === "input_required" || state === "auth_required") return "waiting";
  if (
    state === "failed" ||
    state === "canceled" ||
    state === "timed_out" ||
    state === "rejected"
  ) {
    return "failed";
  }
  return "active";
}

export function TaskStateMark({
  state,
  className,
}: {
  state: string;
  className?: string;
}) {
  const group = taskStateGroup(state);
  return (
    <span className={cn("inline-flex items-center gap-1.5", className)}>
      <span
        aria-hidden="true"
        className={cn("size-2 shrink-0 rounded-[1px]", DOT_CLASS[group])}
      />
      <span className="font-mono text-2xs text-ink-muted">{state}</span>
    </span>
  );
}
