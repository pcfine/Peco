// 今日任务：三列按「需要动作的紧迫度」而非字母序堆叠（窄屏纵排时 进行中 在最上）。
// 失败单独计数、以小徽标挂在「已完成」列头 —— 失败混进已完成会让这一格在骗人。

import { usePecoTodayTasks } from "@/queries/tasks";
import { isToday, relativeTime } from "@/lib/relative";
import { taskStateGroup } from "@/lib/taskStates";
import { cn } from "@/lib/utils";
import type { Task } from "@/types/daemon";
import { PanelShell } from "@/components/board/PanelShell";
import { EmptyState } from "@/components/common/EmptyState";
import { useDockStore } from "@/stores/dockStore";

/** 每列最多几行：看板一格不滚动到看不见底部。 */
const ROWS_PER_COLUMN = 5;

interface TodayBuckets {
  completed: Task[];
  failed: number;
  active: Task[];
  waiting: Task[];
}

export function bucketToday(
  tasks: Task[],
  now: number = Date.now(),
): TodayBuckets {
  const buckets: TodayBuckets = {
    completed: [],
    failed: 0,
    active: [],
    waiting: [],
  };
  for (const task of tasks) {
    const group = taskStateGroup(task.task_state);
    if (group === "active") {
      // 在飞看当下状态，不看「今天」：昨天提交、现在还在跑的任务必须显形。
      buckets.active.push(task);
    } else if (group === "waiting") {
      buckets.waiting.push(task);
    } else if (group === "done") {
      if (isToday(task.terminal_at ?? task.created_at, now)) {
        buckets.completed.push(task);
      }
    } else if (
      group === "failed" &&
      isToday(task.terminal_at ?? task.created_at, now)
    ) {
      buckets.failed += 1;
    }
  }
  return buckets;
}

function TaskRow({ task }: { task: Task }) {
  return (
    <li className="flex h-7 items-center gap-2">
      <span className="min-w-0 flex-1 truncate font-mono text-caption font-medium text-ink-muted">
        {task.result_digest ?? task.task_id}
      </span>
      {/* 420–719 只留标题+时间：agent/团队元信息在窄列里挤成噪音。 */}
      <span className="hidden shrink-0 text-2xs text-ink-faint @min-[720px]/kanban:inline">
        {task.agent_name}
      </span>
      <span className="shrink-0 tabular-nums text-caption font-medium text-ink-faint">
        {relativeTime(task.terminal_at ?? task.created_at)}
      </span>
    </li>
  );
}

function Column({
  title,
  count,
  badge,
  tasks,
  className,
}: {
  title: string;
  count: number;
  badge?: number;
  tasks: Task[];
  className?: string;
}) {
  return (
    <div className={cn("min-w-0 px-2 first:pl-0 last:pr-0", className)}>
      <div className="flex h-6 items-center gap-1.5">
        <span className="text-xs text-ink-muted">{title}</span>
        <span className="tabular-nums text-2xs text-ink-faint">{count}</span>
        {badge !== undefined && badge > 0 && (
          <span
            data-testid="today-failed-badge"
            title={`失败 ${badge}`}
            className="rounded-sm bg-danger/15 px-1 tabular-nums text-2xs text-danger"
          >
            {badge}
          </span>
        )}
      </div>
      <div className="max-h-[320px] overflow-y-auto">
        {tasks.length === 0 ? (
          <div className="grid h-24 place-items-center text-xs text-ink-faint">
            —
          </div>
        ) : (
          <ul>
            {tasks.slice(0, ROWS_PER_COLUMN).map((task) => (
              <TaskRow key={task.task_id} task={task} />
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

export function PanelToday({ pecoWsId }: { pecoWsId: string | null }) {
  const { data, error, isPending, isFetching, refetch } =
    usePecoTodayTasks(pecoWsId);
  const setDockOpen = useDockStore((state) => state.setOpen);
  const buckets = bucketToday(data ?? []);
  const total =
    buckets.completed.length +
    buckets.failed +
    buckets.active.length +
    buckets.waiting.length;

  return (
    <PanelShell
      title="今日任务"
      isPending={isPending}
      isFetching={isFetching}
      error={error}
      hasData={Boolean(data)}
      onRetry={() => void refetch()}
      skeletonRows={3}
    >
      {total === 0 ? (
        <EmptyState
          title="今日没有任务，Peco 派发的任务会出现在这里"
          action={
            <button
              type="button"
              onClick={() => setDockOpen(true)}
              className="tap-target -mx-2 px-2 text-xs text-primary-ink hover:underline"
            >
              让 Peco 开始一件事
            </button>
          }
        />
      ) : (
        <div className="@container/kanban grid grid-cols-1 divide-y divide-line @min-[420px]/kanban:grid-cols-3 @min-[420px]/kanban:divide-x @min-[420px]/kanban:divide-y-0">
          <Column
            title="进行中"
            count={buckets.active.length}
            tasks={buckets.active}
            className="order-1 py-1 @min-[420px]/kanban:order-2 @min-[420px]/kanban:py-0"
          />
          <Column
            title="等待中"
            count={buckets.waiting.length}
            tasks={buckets.waiting}
            className="order-2 py-1 @min-[420px]/kanban:order-3 @min-[420px]/kanban:py-0"
          />
          <Column
            title="已完成"
            count={buckets.completed.length}
            badge={buckets.failed}
            tasks={buckets.completed}
            className="order-3 py-1 @min-[420px]/kanban:order-1 @min-[420px]/kanban:py-0"
          />
        </div>
      )}
    </PanelShell>
  );
}
