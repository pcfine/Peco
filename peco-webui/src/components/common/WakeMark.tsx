// 唤醒态刻度：与任务状态刻度同样的方点 + 文案，但只在「没叫醒」时出现 ——
// 正常投递不该在台账里多占一列注意力。`title` 给完整一句，行内只放短文案。

import { cn } from "@/lib/utils";
import { wakeDescription } from "@/lib/wakeLabels";
import type { WakeSkipReason } from "@/types/daemon";

/** 只有这两种唤醒态值得展示：其余都是正常流程的一部分。 */
export function isNotableWakeState(wakeState: string): boolean {
  return wakeState === "skipped" || wakeState === "failed";
}

export function WakeMark({
  wakeState,
  wakeSkipReason,
  className,
}: {
  wakeState: string;
  wakeSkipReason: WakeSkipReason | string | null;
  className?: string;
}) {
  const description = wakeDescription(wakeState, wakeSkipReason);
  return (
    <span
      title={description}
      className={cn("inline-flex min-w-0 items-center gap-1.5", className)}
    >
      <span
        aria-hidden="true"
        className={cn(
          "size-2 shrink-0 rounded-[1px]",
          wakeState === "failed" ? "bg-danger" : "bg-warn",
        )}
      />
      {/* 窄列里靠省略号收尾：完整文案在 title 与展开详情里。 */}
      <span className="min-w-0 truncate font-mono text-caption font-medium text-ink-muted">
        {description}
      </span>
    </span>
  );
}
