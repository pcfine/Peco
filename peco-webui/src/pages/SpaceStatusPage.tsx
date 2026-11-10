// `/spaces/:wsId/status` —— 空间的状态面：元信息、开关、在飞任务、台账、产物、文件、会话。
// 聊天页不再背这些 tab。main 不滚：头部与 tab 条固定，tab 内容各自滚。

import { Link, useParams, useSearchParams } from "react-router-dom";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { CloseSpaceDialog } from "@/components/spaces/CloseSpaceDialog";
import { SpaceArtifactsTab } from "@/components/spaces/SpaceArtifactsTab";
import { SpaceDetailHeader } from "@/components/spaces/SpaceDetailHeader";
import { SpaceFilesTab } from "@/components/spaces/SpaceFilesTab";
import { SpaceSessionsTab } from "@/components/spaces/SpaceSessionsTab";
import { SpaceTasksTab } from "@/components/spaces/SpaceTasksTab";
import { useCloseSpace } from "@/components/spaces/useSpaceLifecycle";
import {
  useSpace,
  useSpaceArtifacts,
  useSpaceTasks,
} from "@/queries/spaceDetail";
import { errorText } from "@/lib/api";
import { TASK_RUNNING_STATES } from "@/lib/taskStates";
import { cn } from "@/lib/utils";

type SpaceTab = "sessions" | "tasks" | "artifacts" | "files";

const TAB_ORDER: SpaceTab[] = ["sessions", "tasks", "artifacts", "files"];
const TAB_LABELS: Record<SpaceTab, string> = {
  sessions: "会话",
  tasks: "任务台账",
  artifacts: "产物",
  files: "文件",
};

function parseTab(raw: string | null): SpaceTab {
  return TAB_ORDER.includes(raw as SpaceTab) ? (raw as SpaceTab) : "sessions";
}

/** 在飞任务的一句话摘要：台账 tab 有明细，这里只回答「现在有没有人在写」。 */
function inflightSummary(tasks: { agent_name: string; task_state: string }[]) {
  if (tasks.length === 0) return "没有任务在跑。";
  return tasks
    .map((task) => `${task.agent_name} ${task.task_state}`)
    .join(" · ");
}

export function SpaceStatusPage() {
  const { wsId = "" } = useParams();
  const [searchParams, setSearchParams] = useSearchParams();
  const tab = parseTab(searchParams.get("tab"));
  const session = searchParams.get("session");

  const space = useSpace(wsId);
  const tasks = useSpaceTasks(wsId);
  const artifacts = useSpaceArtifacts(wsId);
  const close = useCloseSpace(wsId);

  const setParam = (key: string, value: string | null) => {
    const next = new URLSearchParams(searchParams);
    if (value === null || value === "") next.delete(key);
    else next.set(key, value);
    setSearchParams(next, { replace: true });
  };

  if (space.isPending) {
    return (
      <div className="space-y-px">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-10 w-full" />
        ))}
      </div>
    );
  }

  if (space.error || !space.data) {
    return (
      <div className="space-y-3">
        <ErrorBanner
          message={errorText(space.error)}
          onRetry={() => void space.refetch()}
        />
        <Link
          to="/spaces"
          className="tap-target -mx-2 self-start px-2 text-xs text-primary-ink hover:underline"
        >
          返回工作室
        </Link>
      </div>
    );
  }

  const inflight = (tasks.data ?? []).filter((task) =>
    TASK_RUNNING_STATES.includes(task.task_state),
  );
  const summary = inflightSummary(inflight);
  const counts: Record<SpaceTab, number | null> = {
    sessions: null,
    tasks: tasks.data?.length ?? null,
    artifacts: artifacts.data?.total ?? null,
    files: null,
  };

  return (
    <div className="flex h-full min-h-0 w-full flex-col">
      <SpaceDetailHeader
        space={space.data}
        onClose={close.close}
        closing={close.closing}
      />

      <div className="flex h-8 shrink-0 items-center gap-2 border-b border-line px-3 text-caption font-medium text-ink-faint">
        <span className="shrink-0">在飞任务</span>
        <span className="shrink-0 tabular-nums text-ink">
          {inflight.length}
        </span>
        <span className="min-w-0 truncate" title={summary}>
          {summary}
        </span>
        <Link
          to={`/spaces/${encodeURIComponent(wsId)}`}
          className="tap-target -mx-2 ml-auto shrink-0 px-2 text-primary-ink hover:underline"
        >
          进入聊天
        </Link>
      </div>

      <div
        role="tablist"
        className="flex h-9 shrink-0 items-stretch gap-1 overflow-x-auto border-b border-line"
      >
        {TAB_ORDER.map((item) => (
          <button
            key={item}
            type="button"
            role="tab"
            aria-selected={tab === item}
            onClick={() => setParam("tab", item)}
            className={cn(
              "my-1 shrink-0 rounded-[4px] px-3 text-sm",
              tab === item
                ? "bg-line text-ink"
                : "bg-transparent text-ink-muted hover:bg-line/40",
            )}
          >
            {TAB_LABELS[item]}
            {counts[item] !== null && (
              <span className="ml-1.5 tabular-nums text-2xs text-ink-faint">
                {counts[item]}
              </span>
            )}
          </button>
        ))}
      </div>

      <div className="@container/panel min-h-0 flex-1">
        {tab === "sessions" && (
          <SpaceSessionsTab
            wsId={wsId}
            session={session}
            onSelectSession={(id: string) => setParam("session", id)}
          />
        )}
        {tab === "tasks" && (
          <SpaceTasksTab
            tasks={tasks.data ?? []}
            isPending={tasks.isPending}
            error={tasks.error}
            onRetry={() => void tasks.refetch()}
          />
        )}
        {tab === "artifacts" && (
          <SpaceArtifactsTab
            wsId={wsId}
            artifacts={artifacts.data?.items ?? []}
            isPending={artifacts.isPending}
            error={artifacts.error}
            onRetry={() => void artifacts.refetch()}
          />
        )}
        {tab === "files" && <SpaceFilesTab wsId={wsId} />}
      </div>

      <CloseSpaceDialog
        open={close.pending !== null}
        inflight={close.pending}
        onWait={close.dismiss}
        onForce={() => close.forceClose()}
        forcing={close.forcing}
        error={close.error}
      />
    </div>
  );
}
