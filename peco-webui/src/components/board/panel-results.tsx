// 成果流：最近完成的台账行。两行 40px —— 第一行是摘要，第二行是路由。

import { PanelShell } from "@/components/board/PanelShell";
import { EmptyState } from "@/components/common/EmptyState";
import { spaceLabel } from "@/components/layout/spaceLabel";
import { useRecentResults } from "@/queries/tasks";
import { useSpaces } from "@/queries/spaces";
import { relativeTime } from "@/lib/relative";
import type { Space } from "@/types/daemon";

/** 面板最多渲染几条；再多就不叫「流」了。 */
const VISIBLE_ROWS = 20;

export function PanelResults() {
  const { data, error, isPending, isFetching, refetch } = useRecentResults(50);
  const { data: spaces } = useSpaces();

  const nameOf = (wsId: string): string => {
    const space = (spaces ?? []).find((item: Space) => item.ws_id === wsId);
    return space ? spaceLabel(space) : wsId;
  };

  const rows = (data ?? []).slice(0, VISIBLE_ROWS);

  return (
    <PanelShell
      title="成果流"
      isPending={isPending}
      isFetching={isFetching}
      error={error}
      hasData={Boolean(data)}
      onRetry={() => void refetch()}
    >
      {data && data.length === 0 ? (
        <EmptyState title="最近没有完成的成果。" />
      ) : (
        <div className="max-h-[344px] overflow-y-auto">
          <ul className="grid grid-cols-1 gap-px bg-line @min-[560px]/work:grid-cols-2">
            {rows.map((task) => (
              <li
                key={task.task_id}
                className="flex h-10 min-w-0 flex-col justify-center bg-panel pr-2"
              >
                <span className="truncate text-sm text-ink">
                  {task.result_digest ?? task.task_id}
                </span>
                <span className="truncate font-mono text-caption font-medium text-ink-faint">
                  {nameOf(task.origin_ws_id)} → {nameOf(task.target_ws_id)} ·{" "}
                  {relativeTime(task.terminal_at ?? task.created_at)}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </PanelShell>
  );
}
