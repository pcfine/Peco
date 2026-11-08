// `/results` —— 跨空间台账。状态分组在前端做：`/api/tasks` 只接受单个 `task_state`，
// 而「进行中」是四个状态的合集 —— 为了一个 chip 连发四次请求不如在已取的窗口里筛。

import { useMemo, useState } from "react";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { ConnectionStatus } from "@/components/common/ConnectionStatus";
import { DispatchTimeline } from "@/components/results/DispatchTimeline";
import { taskStateGroup, type TaskStateGroup } from "@/lib/taskStates";
import { spaceLabel } from "@/components/layout/spaceLabel";
import { useSpaces } from "@/queries/spaces";
import { useTasks } from "@/queries/tasks";
import { useConnectionStore } from "@/stores/connectionStore";
import { errorText } from "@/lib/api";
import { cn } from "@/lib/utils";

type StateFilter = "all" | TaskStateGroup;

const STATE_FILTERS: { key: StateFilter; label: string }[] = [
  { key: "all", label: "全部" },
  { key: "active", label: "进行中" },
  { key: "done", label: "已完成" },
  { key: "failed", label: "失败" },
  { key: "orphan", label: "孤儿" },
];

const LIMITS = [50, 100, 200];

export function ResultsPage() {
  const [state, setState] = useState<StateFilter>("all");
  const [origin, setOrigin] = useState("");
  const [limit, setLimit] = useState(100);

  const eventsState = useConnectionStore((state) => state.events);
  const spaces = useSpaces();
  const tasks = useTasks(
    { origin_ws_id: origin || undefined, limit },
    { refetchInterval: 15_000 },
  );

  const rows = useMemo(() => {
    const all = tasks.data ?? [];
    if (state === "all") return all;
    return all.filter((task) =>
      state === "active"
        ? ["active", "waiting"].includes(taskStateGroup(task.task_state))
        : taskStateGroup(task.task_state) === state,
    );
  }, [tasks.data, state]);

  return (
    <div className="mx-auto w-full max-w-wide space-y-4">
      <div className="flex h-8 items-center">
        <h1 className="min-w-0 truncate text-xl font-semibold text-ink">
          成果
        </h1>
      </div>

      <div className="flex min-h-10 flex-wrap items-center gap-2 border-b border-line pb-2">
        <div className="flex items-center gap-1">
          {STATE_FILTERS.map((filter) => (
            <button
              key={filter.key}
              type="button"
              onClick={() => setState(filter.key)}
              aria-pressed={state === filter.key}
              className={cn(
                "h-7 rounded-[4px] px-2 text-xs",
                state === filter.key
                  ? "bg-line text-ink"
                  : "text-ink-muted hover:text-ink",
              )}
            >
              {filter.label}
            </button>
          ))}
        </div>
        <ConnectionStatus state={eventsState} className="ml-auto" />
        <label className="flex items-center gap-1 text-caption font-medium text-ink-faint">
          来源
          <select
            value={origin}
            onChange={(event) => setOrigin(event.target.value)}
            className="h-8 rounded-[6px] border border-line-strong bg-panel px-1 text-xs text-ink"
          >
            <option value="">全部空间</option>
            {(spaces.data ?? []).map((space) => (
              <option key={space.ws_id} value={space.ws_id}>
                {spaceLabel(space)}
              </option>
            ))}
          </select>
        </label>
        <label className="flex items-center gap-1 text-caption font-medium text-ink-faint">
          条数
          <select
            value={limit}
            onChange={(event) => setLimit(Number(event.target.value))}
            className="h-8 rounded-[6px] border border-line-strong bg-panel px-1 text-xs text-ink"
          >
            {LIMITS.map((value) => (
              <option key={value} value={value}>
                {value}
              </option>
            ))}
          </select>
        </label>
      </div>

      {tasks.error && (
        <ErrorBanner
          message={errorText(tasks.error)}
          onRetry={() => void tasks.refetch()}
        />
      )}

      {tasks.isPending && (
        <div className="space-y-px">
          {[0, 1, 2].map((row) => (
            <Skeleton key={row} className="h-11 w-full" />
          ))}
        </div>
      )}

      {tasks.data && tasks.data.length === 0 && (
        <EmptyState title="台账里没有记录。派发过的任务会在完成后写入。" />
      )}
      {tasks.data && tasks.data.length > 0 && rows.length === 0 && (
        <EmptyState title="没有符合当前筛选的记录。" />
      )}

      {rows.length > 0 && (
        <DispatchTimeline tasks={rows} spaces={spaces.data ?? []} />
      )}
    </div>
  );
}
