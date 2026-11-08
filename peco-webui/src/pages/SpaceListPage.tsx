// `/spaces` 工作室列表：三组行式表（进行中 / 已存在 / 已关闭）。
// 组头 sticky 但不做表头 —— 列义明显，4 行表头会吃掉密度。

import { Link } from "react-router-dom";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import {
  SPACE_GROUP_LABELS,
  SpaceStatusMark,
  spaceGroupOf,
  type SpaceGroup,
} from "@/components/layout/SpaceStatusMark";
import { spaceLabel } from "@/components/layout/spaceLabel";
import { Skeleton } from "@/components/ui/skeleton";
import { latestActivityByWs } from "@/components/board/panel-spaces";
import { useEnterSpace } from "@/hooks/useOpenSpaces";
import { errorText } from "@/lib/api";
import { relativeTime } from "@/lib/relative";
import { usePeco } from "@/queries/peco";
import { useSpaces } from "@/queries/spaces";
import { usePecoTodayTasks } from "@/queries/tasks";
import { useSpaceStore } from "@/stores/spaceStore";
import type { Space } from "@/types/daemon";

const GROUP_ORDER: SpaceGroup[] = ["running", "dormant", "closed"];

function SpaceRow({
  space,
  active,
  activityAt,
  onEnter,
}: {
  space: Space;
  active: boolean;
  activityAt: string | null;
  onEnter: () => void;
}) {
  const name = spaceLabel(space);
  return (
    <li
      data-ws-id={space.ws_id}
      className="flex h-16 items-center gap-2 border-b border-line px-3 py-1 @min-[640px]/work:h-14 @min-[900px]/work:h-10"
    >
      <button
        type="button"
        onClick={onEnter}
        aria-label={`进入空间 ${name}`}
        className="flex min-w-0 flex-1 flex-col justify-center gap-0.5 rounded-[4px] text-left hover:bg-line/30 @min-[900px]/work:flex-row @min-[900px]/work:items-center @min-[900px]/work:gap-2"
      >
        <span className="flex min-w-0 items-center gap-2">
          <SpaceStatusMark group={spaceGroupOf(space)} />
          <span className="min-w-0 truncate text-sm text-ink">{name}</span>
          {active && (
            <span className="shrink-0 rounded-[4px] border border-line px-1 text-2xs text-ink-faint">
              当前
            </span>
          )}
        </span>
        <span className="truncate pl-6 font-mono text-mono-sm text-ink-faint @min-[900px]/work:min-w-0 @min-[900px]/work:flex-1 @min-[900px]/work:pl-0">
          {space.work_dir}
        </span>
        <span className="truncate pl-6 font-mono text-2xs text-ink-faint @min-[640px]/work:hidden @min-[900px]/work:block @min-[900px]/work:w-[120px] @min-[900px]/work:shrink-0 @min-[900px]/work:pl-0">
          {space.team_id}
        </span>
        <span className="pl-6 tabular-nums text-2xs text-ink-faint @min-[640px]/work:hidden @min-[900px]/work:block @min-[900px]/work:w-14 @min-[900px]/work:shrink-0 @min-[900px]/work:pl-0 @min-[900px]/work:text-right">
          在飞 {space.inflight_dispatch}
        </span>
        <span className="pl-6 tabular-nums text-2xs text-ink-faint @min-[900px]/work:w-24 @min-[900px]/work:shrink-0 @min-[900px]/work:pl-0 @min-[900px]/work:text-right">
          {relativeTime(activityAt)}
        </span>
      </button>
      <Link
        to={`/spaces/${encodeURIComponent(space.ws_id)}/status`}
        aria-label={`${name} 状态`}
        className="grid h-6 shrink-0 place-items-center rounded-[4px] border border-line px-2 text-2xs text-ink-muted hover:bg-line hover:text-ink"
      >
        状态
      </Link>
    </li>
  );
}

export function SpaceListPage() {
  const { data: peco } = usePeco();
  const { data: spaces, error, isPending, refetch } = useSpaces();
  const { data: tasks } = usePecoTodayTasks(peco?.ws_id ?? null);
  const currentWsId = useSpaceStore((state) => state.currentWsId);
  const enterSpace = useEnterSpace();

  const activity = latestActivityByWs(tasks ?? []);
  const groups = GROUP_ORDER.map((group) => ({
    group,
    rows: (spaces ?? []).filter((space) => spaceGroupOf(space) === group),
  }));

  return (
    <div className="space-y-6">
      {error && (
        <ErrorBanner
          message={errorText(error)}
          onRetry={() => void refetch()}
        />
      )}

      <div className="flex h-6 items-center justify-end">
        <Link
          to="/spaces/new"
          className="text-xs text-primary-ink hover:underline"
        >
          新建空间
        </Link>
      </div>

      {isPending && (
        <div className="space-y-px">
          {[0, 1, 2].map((row) => (
            <Skeleton key={row} className="h-10 w-full" />
          ))}
        </div>
      )}

      {spaces && spaces.length === 0 && (
        <EmptyState
          title="还没有工作空间。空间 = 一个目录 + 一个团队。"
          action={
            <Link
              to="/spaces/new"
              className="text-xs text-primary-ink hover:underline"
            >
              新建空间
            </Link>
          }
        />
      )}

      {groups.map(({ group, rows }) =>
        rows.length === 0 ? null : (
          <section key={group}>
            <header className="sticky top-0 z-10 flex h-8 items-center gap-2 bg-canvas">
              <h2 className="text-xs font-semibold text-ink-muted">
                {SPACE_GROUP_LABELS[group]}
              </h2>
              <span className="tabular-nums text-2xs text-ink-faint">
                {rows.length}
              </span>
            </header>
            <ul>
              {rows.map((space) => (
                <SpaceRow
                  key={space.ws_id}
                  space={space}
                  active={space.ws_id === currentWsId}
                  activityAt={activity.get(space.ws_id) ?? null}
                  onEnter={() => enterSpace(space.ws_id)}
                />
              ))}
            </ul>
          </section>
        ),
      )}
    </div>
  );
}
