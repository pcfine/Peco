// 任务面板的数据来源标注：看到的是实时输出，还是台账里那 512 字摘要。
//
// 两者可信度不同，混着看会让「这条任务到底做完没有」变成猜测，故每条面板顶部
// 都要说明自己读的是哪一份。

import { cn } from "@/lib/utils";

export type TaskFidelity = "live" | "digest";

const LABEL: Record<TaskFidelity, string> = { live: "实时", digest: "摘要" };

const DETAIL: Record<TaskFidelity, string> = {
  live: "内存快照，每 3 秒刷新",
  digest: "台账记录，只有摘要",
};

interface TaskFidelityNoteProps {
  fidelity: TaskFidelity;
  /** 覆盖默认说明：回落台账时要讲清「为什么只剩摘要」。 */
  detail?: string;
}

export function TaskFidelityNote({ fidelity, detail }: TaskFidelityNoteProps) {
  return (
    <div
      data-testid="task-fidelity"
      data-fidelity={fidelity}
      className="flex h-8 shrink-0 items-center gap-2 border-b border-line px-3 text-caption font-medium"
    >
      <span
        className={cn(
          "inline-flex min-h-6 shrink-0 items-center rounded-[4px] bg-line/60 px-1.5",
          fidelity === "live" ? "text-ok" : "text-ink-muted",
        )}
      >
        {LABEL[fidelity]}
      </span>
      <span className="min-w-0 truncate text-ink-faint">
        {detail ?? DETAIL[fidelity]}
      </span>
    </div>
  );
}
