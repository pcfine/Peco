// 台账与成果页共用的状态刻度：8px 方点 + 文字状态。状态清单来自 lib/taskStates，
// 组件只负责把分组画成颜色。

import { cn } from "@/lib/utils";
import { taskStateGroup, type TaskStateGroup } from "@/lib/taskStates";

const DOT_CLASS: Record<TaskStateGroup, string> = {
  active: "bg-brand",
  waiting: "bg-warn",
  done: "bg-ok",
  failed: "bg-danger",
  orphan: "bg-ink-faint",
};

export function TaskStateMark({
  taskState,
  className,
}: {
  taskState: string;
  className?: string;
}) {
  const group = taskStateGroup(taskState);
  return (
    <span className={cn("inline-flex items-center gap-1.5", className)}>
      <span
        aria-hidden="true"
        className={cn("size-2 shrink-0 rounded-[1px]", DOT_CLASS[group])}
      />
      <span className="font-mono text-2xs text-ink-muted">{taskState}</span>
    </span>
  );
}
