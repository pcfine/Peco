// 空间右栏的「任务」：优先读 TaskHub 的实时快照，快照里没有这份历史时回落台账。
//
// 两种来源对应两种保真度：实时快照有输出尾巴与进度，台账只有 512 字摘要。
// 数据源写不出来的话，用户会把摘要当成完整结果，所以顶部一定要标 —— 只在真有
// 台账行可标时才标，空面板上挂「摘要」会让人以为结果被截断。
//
// 快照里没有 agent 与 prompt（Hub 只存执行体无关的部分），这两列按 `task_id`
// 从台账行里取；台账行比快照晚一步落地的那一瞬间，两列就是空。

import { useState } from "react";
import { Loader2 } from "lucide-react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { TaskStateMark } from "@/components/common/TaskStateMark";
import { SpaceTasksTab } from "@/components/spaces/SpaceTasksTab";
import { TaskFidelityNote } from "@/components/spaces/TaskFidelityNote";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { cancelSpaceTask } from "@/api/spaces";
import { useSpaceLiveTasks, useSpaceTasks } from "@/queries/spaceDetail";
import { ApiError, errorText } from "@/lib/api";
import {
  liveTaskElapsedMs,
  outcomeOutputText,
  progressLabel,
  sortLiveTasks,
  tailTextForDisplay,
} from "@/lib/liveTasks";
import { isTerminalTaskState } from "@/lib/taskStates";
import { formatDuration } from "@/lib/time";
import { cn } from "@/lib/utils";
import type { LiveTaskView, Task } from "@/types/daemon";

/** 409 是「这个空间的 Hub 不在」，与真正的失败走两条路。 */
function isSpaceNotOpen(error: unknown): boolean {
  return error instanceof ApiError && error.code === "space_not_open";
}

export function SpaceLiveTasksTab({ wsId }: { wsId: string }) {
  const live = useSpaceLiveTasks(wsId);
  // 台账只为补 agent / prompt 两列，以及与摘要回落共用一份查询。
  const ledger = useSpaceTasks(wsId);

  if (live.isPending) {
    return (
      <div className="space-y-px p-3">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-9 w-full" />
        ))}
      </div>
    );
  }

  if (live.error && !isSpaceNotOpen(live.error)) {
    return (
      <div className="p-3">
        <ErrorBanner
          message={errorText(live.error)}
          onRetry={() => void live.refetch()}
        />
      </div>
    );
  }

  // 409：这个空间的 Hub 不在，只剩台账里那 512 字摘要。判 `live.error` 而不是只看
  // `data` 有没有行：空间在别处被关掉后，`data` 里还留着关之前的快照，照着画就是
  // 一份再也没有后续的「实时」进度。
  if (live.error) {
    return (
      <DigestTasks ledger={ledger} detail="空间未打开，只剩台账里的摘要" />
    );
  }

  // daemon 重启后空间会自动恢复打开，Hub 里没有历史，快照是 200 加空数组。
  // 当空态处理的话，整份台账就会显示成「还没有任务」。
  if (!live.data?.length) {
    return (
      <DigestTasks ledger={ledger} detail="实时快照为空，只剩台账里的摘要" />
    );
  }

  const tasks = sortLiveTasks(live.data);
  const ledgerByTaskId = new Map(
    (ledger.data ?? []).map((task) => [task.task_id, task]),
  );

  return (
    <div className="flex h-full min-h-0 flex-col">
      <TaskFidelityNote fidelity="live" />
      <ul className="min-h-0 flex-1 overflow-y-auto">
        {tasks.map((task) => (
          <LiveTaskRow
            key={task.id}
            wsId={wsId}
            task={task}
            ledgerRow={ledgerByTaskId.get(task.id)}
          />
        ))}
      </ul>
    </div>
  );
}

type LedgerQuery = ReturnType<typeof useSpaceTasks>;

/**
 * 只剩台账可读时的面板。
 *
 * 标注跟着「台账里到底有没有行」走：加载中或出错时说不出保真度，空台账也没有
 * 摘要可标，两种情况都不挂来源说明。
 */
function DigestTasks({
  ledger,
  detail,
}: {
  ledger: LedgerQuery;
  detail: string;
}) {
  const tasks = ledger.data ?? [];

  return (
    <div className="flex h-full min-h-0 flex-col">
      {tasks.length > 0 && (
        <TaskFidelityNote fidelity="digest" detail={detail} />
      )}
      <div className="min-h-0 flex-1">
        <SpaceTasksTab
          tasks={tasks}
          isPending={ledger.isPending}
          error={ledger.error}
          onRetry={() => void ledger.refetch()}
        />
      </div>
    </div>
  );
}

interface LiveTaskRowProps {
  wsId: string;
  task: LiveTaskView;
  ledgerRow: Task | undefined;
}

function LiveTaskRow({ wsId, task, ledgerRow }: LiveTaskRowProps) {
  const [open, setOpen] = useState(false);
  const terminal = isTerminalTaskState(task.state);
  const promptDigest = ledgerRow?.prompt_digest ?? null;
  const progress = progressLabel(task.progress);
  const duration = formatDuration(liveTaskElapsedMs(task));

  return (
    <li className="border-b border-line">
      <div className="flex items-start gap-1 pr-1">
        <button
          type="button"
          onClick={() => setOpen(!open)}
          aria-expanded={open}
          className="min-w-0 flex-1 px-3 py-1.5 text-left outline-none hover:bg-line/30 focus-visible:ring-[3px] focus-visible:ring-ring/35"
        >
          <span className="flex min-w-0 items-center gap-2">
            <TaskStateMark taskState={task.state} className="shrink-0" />
            <span
              className={cn(
                "min-w-0 flex-1 truncate text-sm",
                promptDigest ? "text-ink" : "text-ink-faint",
              )}
            >
              {promptDigest ?? "未记录"}
            </span>
            <span className="shrink-0 tabular-nums text-2xs text-ink-faint">
              {duration ?? "—"}
            </span>
          </span>
          <span className="mt-0.5 flex min-w-0 items-center gap-1.5 text-2xs text-ink-faint">
            <span className="truncate font-mono">{task.kind}</span>
            <span aria-hidden="true">·</span>
            <span className="truncate">{ledgerRow?.agent_name ?? "—"}</span>
            {progress && (
              <>
                <span aria-hidden="true">·</span>
                <span className="shrink-0 tabular-nums">{progress}</span>
              </>
            )}
          </span>
        </button>
        {!terminal && (
          <CancelTaskButton
            wsId={wsId}
            taskId={task.id}
            promptDigest={promptDigest}
          />
        )}
      </div>
      {open && <LiveTaskDetail task={task} promptDigest={promptDigest} />}
    </li>
  );
}

function LiveTaskDetail({
  task,
  promptDigest,
}: {
  task: LiveTaskView;
  promptDigest: string | null;
}) {
  const terminal = isTerminalTaskState(task.state);
  const tail = tailTextForDisplay(task.output.tail);
  const outcome = outcomeOutputText(task.outcome?.output);
  const error = task.outcome?.error ?? null;

  return (
    <div className="space-y-2 border-t border-line bg-canvas/40 px-3 py-2">
      <p className="font-mono text-2xs text-ink-faint">task_id {task.id}</p>
      <div className="space-y-0.5">
        <p className="text-caption font-medium text-ink-faint">
          Peco 让它做什么
        </p>
        <p
          className={cn(
            "max-w-narrow whitespace-pre-wrap break-words text-sm",
            promptDigest ? "text-ink" : "text-ink-faint",
          )}
        >
          {promptDigest ?? "未记录"}
        </p>
      </div>
      {/* 终态优先看快照里的完整结果；没有结果（被取消、失败）时才退到输出窗口。 */}
      {outcome ? (
        <OutputBlock label="结果输出" text={outcome} />
      ) : tail.text ? (
        <OutputBlock
          label={tail.truncated ? "输出（最近 4000 字）" : "输出"}
          text={tail.text}
        />
      ) : (
        !terminal && (
          <p className="text-caption font-medium text-ink-faint">
            还没有输出。
          </p>
        )
      )}
      {error && (
        <p className="max-w-narrow break-words text-caption font-medium text-danger">
          失败：{error.kind} · {error.message}
        </p>
      )}
      <p
        data-testid="live-task-artifacts"
        className="text-caption font-medium text-ink-faint"
      >
        产物 <span className="tabular-nums">{task.artifacts.length}</span> 个
      </p>
    </div>
  );
}

function OutputBlock({ label, text }: { label: string; text: string }) {
  return (
    <div className="space-y-0.5">
      <p className="text-caption font-medium text-ink-faint">{label}</p>
      <pre className="max-h-60 max-w-narrow overflow-auto whitespace-pre-wrap break-words font-mono text-2xs text-ink">
        {text}
      </pre>
    </div>
  );
}

/** 取消成功后立刻失效两条查询：等下一次轮询会把按钮留在「取消中」好几秒。 */
function CancelTaskButton({
  wsId,
  taskId,
  promptDigest,
}: {
  wsId: string;
  taskId: string;
  promptDigest: string | null;
}) {
  const queryClient = useQueryClient();
  const cancel = useMutation({
    mutationFn: () => cancelSpaceTask(wsId, taskId),
    onSuccess: () => {
      void queryClient.invalidateQueries({
        queryKey: ["space-live-tasks", wsId],
      });
      void queryClient.invalidateQueries({ queryKey: ["space-tasks", wsId] });
    },
    onError: (error) => toast.error(errorText(error)),
  });

  // 无障碍名要说清取消的是哪一条：只有 UUID 对读屏用户没有意义。
  const label = promptDigest
    ? `取消任务：${promptDigest}`
    : `取消任务 ${taskId}`;

  return (
    <Button
      variant="ghost"
      size="xs"
      className="mt-1 shrink-0 text-ink-muted"
      aria-label={label}
      aria-busy={cancel.isPending}
      disabled={cancel.isPending}
      onClick={() => cancel.mutate()}
    >
      {cancel.isPending && (
        <Loader2 aria-hidden="true" className="size-3 animate-spin" />
      )}
      取消
    </Button>
  );
}
