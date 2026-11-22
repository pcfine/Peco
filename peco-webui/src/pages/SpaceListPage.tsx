// `/spaces` 工作室列表：两个页签 —— 工作空间（三组行式表）与回收站。
// 组头 sticky 但不做表头 —— 列义明显，4 行表头会吃掉密度。
//
// 删除是「移入回收站」：会话与记忆留着，目录里的文件不动，撤销的入口在回收站。
// 页签走 URL（`?tab=trash`），刷新与前进后退都回得到同一处。

import { useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import {
  SPACE_GROUP_LABELS,
  SpaceStatusMark,
  spaceGroupOf,
  type SpaceGroup,
} from "@/components/layout/SpaceStatusMark";
import { PageFrame } from "@/components/layout/PageFrame";
import { spaceLabel } from "@/components/layout/spaceLabel";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { latestActivityByWs } from "@/components/board/panel-spaces";
import { SpaceRemovalDialog } from "@/components/spaces/SpaceRemovalDialog";
import { SpaceTrashList } from "@/components/spaces/SpaceTrashList";
import { useCloseSpace } from "@/components/spaces/useSpaceLifecycle";
import { useEnterSpace } from "@/hooks/useOpenSpaces";
import { errorText } from "@/lib/api";
import { relativeTime } from "@/lib/relative";
import { usePeco } from "@/queries/peco";
import { useSpaces, useTrashSpace, useTrashSpaces } from "@/queries/spaces";
import { usePecoTodayTasks } from "@/queries/tasks";
import { useSpaceStore } from "@/stores/spaceStore";
import { cn } from "@/lib/utils";
import type { Space } from "@/types/daemon";

const GROUP_ORDER: SpaceGroup[] = ["running", "dormant", "closed"];

type SpacesTab = "workspaces" | "trash";

const TAB_ORDER: SpacesTab[] = ["workspaces", "trash"];

const TAB_LABELS: Record<SpacesTab, string> = {
  workspaces: "工作空间",
  trash: "回收站",
};

function parseTab(raw: string | null): SpacesTab {
  return TAB_ORDER.includes(raw as SpacesTab)
    ? (raw as SpacesTab)
    : "workspaces";
}

function SpaceRow({
  space,
  active,
  activityAt,
  onEnter,
  onRemove,
}: {
  space: Space;
  active: boolean;
  activityAt: string | null;
  onEnter: () => void;
  onRemove: () => void;
}) {
  const name = spaceLabel(space);
  const group = spaceGroupOf(space);
  // 在飞任务含外来；蓝点跟在最近打开（本页即当前）的空间上。
  const inflight = space.inflight_dispatch + space.inflight_foreign;
  const close = useCloseSpace(space.ws_id);
  // 只有空闲的已打开空间能关：关闭会卸载实例，在飞的活没有落点。
  const closable = group === "running" && inflight === 0;
  const closeHint =
    group !== "running"
      ? "空间未打开，无需关闭"
      : inflight > 0
        ? `还有 ${inflight} 个任务在飞，等工作结束再关闭`
        : "关闭空间";
  return (
    <li
      data-ws-id={space.ws_id}
      className="flex h-16 items-center gap-2 border-b border-line px-3 py-1 @min-[640px]/work:h-14 @min-[900px]/work:h-10"
    >
      <button
        type="button"
        onClick={onEnter}
        aria-label={`进入空间 ${name}`}
        className="flex h-full min-w-0 flex-1 flex-col justify-center gap-0.5 rounded-sm text-left hover:bg-line/30 @min-[900px]/work:flex-row @min-[900px]/work:items-center @min-[900px]/work:gap-2"
      >
        <span className="flex min-w-0 items-center gap-2">
          <SpaceStatusMark
            group={group}
            recent={active}
            inflight={inflight}
          />
          <span className="min-w-0 truncate text-sm text-ink">{name}</span>
          {active && (
            <span className="shrink-0 rounded-sm bg-line/60 px-1 text-caption font-medium text-ink-faint">
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
        <span className="pl-6 tabular-nums text-caption font-medium text-ink-faint @min-[640px]/work:hidden @min-[900px]/work:block @min-[900px]/work:w-14 @min-[900px]/work:shrink-0 @min-[900px]/work:pl-0 @min-[900px]/work:text-right">
          在飞 {inflight}
        </span>
        <span className="pl-6 tabular-nums text-caption font-medium text-ink-faint @min-[900px]/work:w-24 @min-[900px]/work:shrink-0 @min-[900px]/work:pl-0 @min-[900px]/work:text-right">
          {relativeTime(activityAt)}
        </span>
      </button>
      <button
        type="button"
        onClick={close.close}
        disabled={!closable}
        title={closeHint}
        aria-label={`关闭空间 ${name}`}
        className="grid h-6 shrink-0 place-items-center rounded-sm bg-line/60 px-2 text-caption font-medium text-ink-muted transition-colors duration-[120ms] enabled:hover:bg-line enabled:hover:text-ink disabled:cursor-not-allowed disabled:opacity-40"
      >
        关闭
      </button>
      {/* 保留空间删不掉（daemon 400 `reserved_space`）：不给一个注定失败的按钮，
          但槽位照留 —— 它少一截会把整列「在飞」推到别的行对不齐。 */}
      {space.reserved ? (
        <span
          aria-hidden="true"
          className="invisible grid h-6 shrink-0 place-items-center rounded-sm px-2 text-caption font-medium"
        >
          删除
        </span>
      ) : (
        <button
          type="button"
          onClick={onRemove}
          aria-label={`删除空间 ${name}（移入回收站）`}
          className="grid h-6 shrink-0 place-items-center rounded-sm px-2 text-caption font-medium text-ink-faint transition-colors duration-[120ms] hover:bg-line/60 hover:text-danger"
        >
          删除
        </button>
      )}
    </li>
  );
}

export function SpaceListPage() {
  const { data: peco } = usePeco();
  const { data: spaces, error, isPending, refetch } = useSpaces();
  const { data: tasks } = usePecoTodayTasks(peco?.ws_id ?? null);
  const trash = useTrashSpaces();
  const [searchParams, setSearchParams] = useSearchParams();
  const tab = parseTab(searchParams.get("tab"));
  const currentWsId = useSpaceStore((state) => state.currentWsId);
  const enterSpace = useEnterSpace();
  const remove = useTrashSpace();
  const [removing, setRemoving] = useState<Space | null>(null);

  const activity = latestActivityByWs(tasks ?? []);
  const groups = GROUP_ORDER.map((group) => ({
    group,
    rows: (spaces ?? []).filter((space) => spaceGroupOf(space) === group),
  }));
  const trashCount = trash.data?.length ?? 0;

  const selectTab = (next: SpacesTab) => {
    const params = new URLSearchParams(searchParams);
    // 缺省页签不进 URL：`/spaces` 与 `/spaces?tab=workspaces` 是同一处，不必两份。
    if (next === "workspaces") params.delete("tab");
    else params.set("tab", next);
    setSearchParams(params, { replace: true });
  };

  return (
    <PageFrame
      title="工作室"
      actions={
        <Button asChild size="sm">
          <Link to="/spaces/new">新建空间</Link>
        </Button>
      }
    >
      <div className="space-y-6">
        <div
          role="tablist"
          aria-label="工作室视图"
          className="flex h-9 items-stretch gap-1 border-b border-line"
        >
          {TAB_ORDER.map((item) => (
            <button
              key={item}
              type="button"
              role="tab"
              aria-selected={tab === item}
              onClick={() => selectTab(item)}
              className={cn(
                "my-1 shrink-0 rounded-sm px-3 text-sm",
                tab === item
                  ? "bg-line text-ink"
                  : "bg-transparent text-ink-muted hover:bg-line/40",
              )}
            >
              {TAB_LABELS[item]}
              {item === "trash" && trashCount > 0 && (
                <span className="ml-1.5 tabular-nums text-2xs text-ink-faint">
                  {trashCount}
                </span>
              )}
            </button>
          ))}
        </div>

        {tab === "trash" ? (
          <SpaceTrashList />
        ) : (
          <>
            {error && (
              <ErrorBanner
                message={errorText(error)}
                onRetry={() => void refetch()}
              />
            )}

            {isPending && (
              <div className="space-y-px">
                {[0, 1, 2].map((row) => (
                  <Skeleton key={row} className="h-10 w-full" />
                ))}
              </div>
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
                  <ul className="min-w-0 overflow-x-auto">
                    {rows.map((space) => (
                      <SpaceRow
                        key={space.ws_id}
                        space={space}
                        active={space.ws_id === currentWsId}
                        activityAt={activity.get(space.ws_id) ?? null}
                        onEnter={() => enterSpace(space.ws_id)}
                        onRemove={() => setRemoving(space)}
                      />
                    ))}
                  </ul>
                </section>
              ),
            )}
          </>
        )}
      </div>

      {removing && (
        <SpaceRemovalDialog
          name={spaceLabel(removing)}
          mode="trash"
          run={(force) =>
            remove.mutate(
              { wsId: removing.ws_id, force },
              { onSuccess: () => setRemoving(null) },
            )
          }
          pending={remove.isPending}
          error={remove.error}
          onClose={() => {
            setRemoving(null);
            remove.reset();
          }}
        />
      )}
    </PageFrame>
  );
}
