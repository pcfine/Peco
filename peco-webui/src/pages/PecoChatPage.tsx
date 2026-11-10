// `/peco` —— Peco 回到主区：左聊天、右栏四模式（默认总览）。
//
// 聊天面与空间页同构：两块各占一半，两侧头部各留一条「展开」的回来路径，没有横跨
// 整宽的页头。能力配置从左头的 `·peco` 链接进团队配置页，与其它团队共用同一份面板；
// Peco 的特殊之处（不能再建空间、记忆按空间看）在那里特例化。
//
// 文件模式要一个已打开的 Peco 空间作 `X-Peco-Space`；空间没打开时聊天列与文件模式
// 统一摆 `PecoClosedNotice`，不各写一句。

import { useEffect, useRef, type ReactNode } from "react";
import type { UseQueryResult } from "@tanstack/react-query";
import { useSearchParams } from "react-router-dom";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { ClearConversationDialog } from "@/components/spaces/ClearConversationDialog";
import { PecoClosedNotice } from "@/components/spaces/PecoClosedNotice";
import { SpaceChatHeader } from "@/components/spaces/SpaceChatHeader";
import { SpaceChatShell } from "@/components/spaces/SpaceChatShell";
import { SpacePanelHeader } from "@/components/spaces/SpacePanelHeader";
import { SpaceSidePanel } from "@/components/spaces/SpaceSidePanel";
import { useConversationClear } from "@/components/spaces/useConversationClear";
import {
  SPACE_PANEL_MODES,
  useSpacePanelState,
} from "@/components/spaces/useSpacePanelState";
import { WORK_INLINE_MIN, workWidthOf } from "@/components/layout/shellMetrics";
import { Skeleton } from "@/components/ui/skeleton";
import { useViewportWidth } from "@/hooks/useViewportWidth";
import { errorText } from "@/lib/api";
import { TASK_RUNNING_STATES } from "@/lib/taskStates";
import { cn } from "@/lib/utils";
import { usePeco } from "@/queries/peco";
import { useSpaces } from "@/queries/spaces";
import { usePecoTodayTasks } from "@/queries/tasks";
import { useSpaceStreaming } from "@/stores/spaceActivityStore";
import { useUiStore } from "@/stores/uiStore";
import type { PecoIdentity, Space } from "@/types/daemon";

export function PecoChatPage() {
  const peco = usePeco();

  if (peco.isPending) {
    return (
      <div className="space-y-px">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-10 w-full" />
        ))}
      </div>
    );
  }

  if (peco.error || !peco.data) {
    return (
      <ErrorBanner
        message={errorText(peco.error)}
        onRetry={() => void peco.refetch()}
      />
    );
  }

  return <PecoWorkspace peco={peco.data} />;
}

/** 空间清单还没到手、拉取失败或未打开时，聊天列与文件面板共用这一块。 */
function spaceUnavailableNotice(
  space: Space | null,
  open: boolean,
  spaces: UseQueryResult<Space[], Error>,
): ReactNode {
  if (open) return null;
  if (spaces.error) {
    return (
      <div className="p-3">
        <ErrorBanner
          message={errorText(spaces.error)}
          onRetry={() => void spaces.refetch()}
        />
      </div>
    );
  }
  if (!space) {
    return (
      <div className="space-y-2 p-3">
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-10 w-full" />
      </div>
    );
  }
  return <PecoClosedNotice space={space} />;
}

type HeaderSpace = Pick<
  Space,
  "label" | "work_dir" | "team_id" | "status" | "closed_at"
>;

/**
 * 左头要一个 `Space` 才能画状态点与标题。空间清单还没到手时用身份顶上：
 * 全页唯一的 h1 与三个动作都长在这一行，缺了它加载窗口里就没有标题。
 * 状态按「已存在」算（`closed` 且没被显式关过）—— 那时确实还没有实例。
 */
function headerSpaceOf(peco: PecoIdentity, space: Space | null) {
  if (space) return space;
  return {
    label: "Peco",
    work_dir: peco.work_dir,
    team_id: peco.team_id,
    status: "closed",
    closed_at: null,
  } satisfies HeaderSpace;
}

function PecoWorkspace({ peco }: { peco: PecoIdentity }) {
  const [searchParams, setSearchParams] = useSearchParams();

  const viewport = useViewportWidth();
  const railOpen = useUiStore((state) => state.railOpen);
  // 与空间页、壳的栅格同源：work 均分后每块不足 412 时，两块互斥且聊天优先。
  const exclusive = workWidthOf(viewport, railOpen) < WORK_INLINE_MIN;

  const spaces = useSpaces();
  const space =
    (spaces.data ?? []).find((item) => item.ws_id === peco.ws_id) ?? null;
  const spaceOpen = space?.status === "open";

  const panel = useSpacePanelState(peco.ws_id, "peco", {
    exclusive,
    modeFromUrl: searchParams.get("panel"),
    // 只改 `panel`：`face` 决定的是另一件事，切模式不该把它带掉。replace 不加历史条目。
    onModeChange: (mode) =>
      setSearchParams(
        (previous) => {
          const next = new URLSearchParams(previous);
          next.set("panel", mode);
          return next;
        },
        { replace: true },
      ),
  });
  const tasks = usePecoTodayTasks(peco.ws_id);
  const inflight = (tasks.data ?? []).filter((task) =>
    TASK_RUNNING_STATES.includes(task.task_state),
  ).length;

  const unavailable = spaceUnavailableNotice(space, spaceOpen, spaces);

  // 对话是否在跑由聊天壳按 wsId 上报到活动 store，与空间页同一份事实。
  const streaming = useSpaceStreaming(peco.ws_id);

  // 空间没打开时不发 ensure：先去「打开」，别让用户在清除对话上先吃一个 409。
  const clear = useConversationClear(peco.ws_id, streaming, {
    enabled: spaceOpen,
  });

  const expandChatRef = useRef<HTMLButtonElement>(null);
  const expandPanelRef = useRef<HTMLButtonElement>(null);
  const toggledByUser = useRef(false);

  // 折叠把被点的那枚按钮一起带走，焦点会掉回 body：落到对面刚出现的「展开」上，
  // 读屏也就跟着念出落点。两块都在时无需移动。
  useEffect(() => {
    if (!toggledByUser.current) return;
    toggledByUser.current = false;
    const target = panel.chatCollapsed
      ? expandChatRef
      : panel.collapsed
        ? expandPanelRef
        : null;
    target?.current?.focus();
  }, [panel.chatCollapsed, panel.collapsed]);

  const toggleChat = () => {
    toggledByUser.current = true;
    panel.toggleChat();
  };

  const togglePanel = () => {
    toggledByUser.current = true;
    panel.toggleCollapsed();
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex min-h-0 flex-1">
        <div
          data-testid="peco-chat-column"
          className={cn(
            "min-w-0 flex-1 basis-0 flex-col",
            panel.chatCollapsed ? "hidden" : "flex",
          )}
        >
          <SpaceChatHeader
            space={headerSpaceOf(peco, space)}
            panelCollapsed={panel.collapsed}
            onExpandPanel={togglePanel}
            onCollapseChat={toggleChat}
            expandPanelRef={expandPanelRef}
            onClearConversation={clear.openDialog}
            clearDisabled={clear.disabled}
            clearDisabledReason={clear.disabledReason}
            title="Peco"
            titleHint={peco.work_dir}
          />
          <div className="flex min-h-0 flex-1 flex-col px-4 pb-3">
            {unavailable ?? (
              <SpaceChatShell
                wsId={peco.ws_id}
                visible
                teamId={space?.team_id ?? peco.team_id}
              />
            )}
          </div>
        </div>

        {!panel.collapsed && (
          <aside
            data-testid="peco-panel"
            data-mode={panel.mode}
            aria-label="Peco 右栏"
            className="flex min-w-0 flex-1 basis-0 flex-col border-l border-line bg-side"
          >
            <SpacePanelHeader
              mode={panel.mode}
              modes={SPACE_PANEL_MODES.peco}
              inflight={inflight}
              chatCollapsed={panel.chatCollapsed}
              onSelect={panel.selectMode}
              onExpandChat={toggleChat}
              onCollapsePanel={togglePanel}
              expandChatRef={expandChatRef}
            />
            <div className="@container/panel min-h-0 flex-1">
              {panel.mode === "files" && unavailable ? (
                unavailable
              ) : (
                <SpaceSidePanel
                  wsId={peco.ws_id}
                  mode={panel.mode}
                  variant="peco"
                />
              )}
            </div>
          </aside>
        )}
      </div>

      <ClearConversationDialog
        open={clear.dialogOpen}
        pending={clear.pending}
        onCancel={clear.cancel}
        onConfirm={clear.confirm}
      />
    </div>
  );
}
