// 空间状态：每空间一行。最近活动取台账里该空间最新的任务时刻 ——
// `/api/spaces` 不返回时间戳，台账才是这台机器上「有活动」的权威记录。

import { Link } from "react-router-dom";
import { PanelShell } from "@/components/board/PanelShell";
import { EmptyState } from "@/components/common/EmptyState";
import { spaceLabel } from "@/components/layout/spaceLabel";
import { usePeco } from "@/queries/peco";
import { usePecoTodayTasks } from "@/queries/tasks";
import { useSpaces } from "@/queries/spaces";
import { relativeTime } from "@/lib/relative";
import type { Space, Task } from "@/types/daemon";

/** 面板里最多几行；其余折成一条真链接（它确实跳转）。 */
const VISIBLE_ROWS = 6;

/** target_ws_id → 最近一次任务时刻（同一次请求里已拿到，不额外查）。 */
export function latestActivityByWs(tasks: Task[]): Map<string, string> {
  const map = new Map<string, string>();
  for (const task of tasks) {
    const at = task.terminal_at ?? task.created_at;
    const current = map.get(task.target_ws_id);
    if (!current || at > current) map.set(task.target_ws_id, at);
  }
  return map;
}

export function PanelSpaces() {
  const { data: peco } = usePeco();
  const { data: spaces, error, isPending, isFetching, refetch } = useSpaces();
  const { data: tasks } = usePecoTodayTasks(peco?.ws_id ?? null);
  const activity = latestActivityByWs(tasks ?? []);

  const rows = (spaces ?? []).slice(0, VISIBLE_ROWS);
  const rest = (spaces ?? []).length - rows.length;

  return (
    <PanelShell
      title="空间状态"
      isPending={isPending}
      isFetching={isFetching}
      error={error}
      hasData={Boolean(spaces)}
      onRetry={() => void refetch()}
    >
      {spaces && spaces.length === 0 ? (
        <EmptyState
          title="还没有工作空间。空间 = 一个目录 + 一个团队。"
          action={
            <Link
              to="/spaces"
              className="text-xs text-primary-ink hover:underline"
            >
              新建空间
            </Link>
          }
        />
      ) : (
        <ul className="divide-y divide-line">
          {rows.map((space: Space) => (
            <li
              key={space.ws_id}
              className="flex h-12 flex-col justify-center @min-[640px]/work:h-8 @min-[640px]/work:flex-row @min-[640px]/work:items-center @min-[640px]/work:gap-2"
            >
              <span className="min-w-0 flex-1 truncate text-sm text-ink">
                {spaceLabel(space)}
              </span>
              <span className="shrink-0 truncate font-mono text-2xs text-ink-faint @min-[640px]/work:w-[120px]">
                {space.team_id}
              </span>
              <span className="shrink-0 tabular-nums text-2xs text-ink-faint @min-[640px]/work:w-14 @min-[640px]/work:text-right">
                在飞 {space.inflight_dispatch}
              </span>
              <span className="shrink-0 tabular-nums text-2xs text-ink-faint @min-[640px]/work:w-24 @min-[640px]/work:text-right">
                {relativeTime(activity.get(space.ws_id) ?? null)}
              </span>
            </li>
          ))}
        </ul>
      )}
      {rest > 0 && (
        <Link
          to="/spaces"
          className="mt-1 block text-2xs text-ink-faint hover:text-ink"
        >
          查看全部 {(spaces ?? []).length} 个空间 →
        </Link>
      )}
    </PanelShell>
  );
}
