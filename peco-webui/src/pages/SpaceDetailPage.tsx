// `/spaces/:wsId` —— 头部 + 4 个 tab。main 不滚：头部与 tab 条固定，
// tab 内容各自滚，会话 tab 的输入框因此永远贴在底部。

import { useSearchParams, useParams, Link } from "react-router-dom";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { CloseSpaceDialog } from "@/components/spaces/CloseSpaceDialog";
import { SpaceArtifactsTab } from "@/components/spaces/SpaceArtifactsTab";
import { SpaceDetailHeader } from "@/components/spaces/SpaceDetailHeader";
import { SpaceFilesTab } from "@/components/spaces/SpaceFilesTab";
import { SpaceSessionsTab } from "@/components/spaces/SpaceSessionsTab";
import { SpaceTasksTab } from "@/components/spaces/SpaceTasksTab";
import {
  useSpace,
  useSpaceArtifacts,
  useSpaceTasks,
} from "@/queries/spaceDetail";
import { useCloseSpace } from "@/components/spaces/useSpaceLifecycle";
import { errorText } from "@/lib/api";
import { cn } from "@/lib/utils";

export type SpaceTab = "sessions" | "tasks" | "artifacts" | "files";

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

export function SpaceDetailPage() {
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
        <Link to="/spaces" className="text-xs text-primary-ink hover:underline">
          返回工作室
        </Link>
      </div>
    );
  }

  const counts: Record<SpaceTab, number | null> = {
    sessions: null,
    tasks: tasks.data?.length ?? null,
    artifacts: artifacts.data?.total ?? null,
    files: null,
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <SpaceDetailHeader
        space={space.data}
        onClose={close.close}
        closing={close.closing}
      />

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
              "shrink-0 border-b-2 px-3 text-sm",
              tab === item
                ? "border-brand text-ink"
                : "border-transparent text-ink-muted hover:text-ink",
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

      <div className="min-h-0 flex-1">
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
