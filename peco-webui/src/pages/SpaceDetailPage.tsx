// `/spaces/:wsId` —— 空间聊天页：左边一条 @master 对话，右边一块 tab 面板。
//
// 双开时两块均分整宽：右栏的列表行本来就是通栏，钉死一个宽度在宽屏上只是白留缝。
// 两块都能藏起自己、让另一块占满整宽：窄屏拥挤没有第二种解法，覆盖抽屉那套因此删掉，
// 页面只剩一种形态。谁被藏起来，谁就从对面的头部接一个「展开」按钮回来。
//
// 藏起来 = `display:none`，不是不渲染：卸载会掐断在途的那一轮（见下）。
//
// 打开集里的每个空间都挂一个聊天壳，只有当前空间可见。React Router 在
// `/spaces/A` → `/spaces/B` 之间复用同一个路由元素，页面因此不重挂载，
// 切走的空间继续跑它的流；左栏切换只改激活态与路由。
//
// 已知限制：进 `/spaces/:wsId/status` 或 `/peco` 会离开这个路由元素，
// 隐藏空间的流随组件卸载中断。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { ClearConversationDialog } from "@/components/spaces/ClearConversationDialog";
import { CloseSpaceDialog } from "@/components/spaces/CloseSpaceDialog";
import { SpaceChatHeader } from "@/components/spaces/SpaceChatHeader";
import { SpaceChatShell } from "@/components/spaces/SpaceChatShell";
import { SpacePanelHeader } from "@/components/spaces/SpacePanelHeader";
import { SpaceSidePanel } from "@/components/spaces/SpaceSidePanel";
import {
  SPACE_PANEL_MODES,
  useSpacePanelState,
} from "@/components/spaces/useSpacePanelState";
import { useConversationClear } from "@/components/spaces/useConversationClear";
import {
  useCloseSpace,
  useOpenSpace,
} from "@/components/spaces/useSpaceLifecycle";
import { WORK_INLINE_MIN, workWidthOf } from "@/components/layout/shellMetrics";
import { useViewportWidth } from "@/hooks/useViewportWidth";
import { useInflightTaskCount, useSpace } from "@/queries/spaceDetail";
import { useOpenSpacesStore } from "@/stores/openSpacesStore";
import { useUiStore } from "@/stores/uiStore";
import { errorText } from "@/lib/api";
import { cn } from "@/lib/utils";
import type { Space } from "@/types/daemon";

/**
 * 对话列里唯一能启动空间的地方：左头只是一行标题，没有动作位。
 * 少了这一处，「空间还没打开」就只剩一句死胡同式的说明。
 */
function SpaceClosedBanner({ space }: { space: Space }) {
  const { open, opening } = useOpenSpace(
    space.ws_id,
    space.work_dir,
    space.team_id,
  );

  return (
    <div className="flex shrink-0 items-start gap-2 py-3">
      <div className="min-w-0 flex-1">
        <ErrorBanner message="空间还没打开，消息发不出去。" />
      </div>
      <Button size="sm" className="h-7" disabled={opening} onClick={open}>
        打开
      </Button>
    </div>
  );
}

export function SpaceDetailPage() {
  const { wsId = "" } = useParams();
  const space = useSpace(wsId);
  const close = useCloseSpace(wsId);
  const openIds = useOpenSpacesStore((state) => state.openIds);
  const viewport = useViewportWidth();
  const railOpen = useUiStore((state) => state.railOpen);
  const [searchParams, setSearchParams] = useSearchParams();

  // 与壳的栅格同源：work 均分后每块不足 412 时，两块互斥。
  const exclusive = workWidthOf(viewport, railOpen) < WORK_INLINE_MIN;
  const panel = useSpacePanelState(wsId, "space", {
    exclusive,
    modeFromUrl: searchParams.get("panel"),
    // 只改 `panel`，`face` 之类的其它参数原样留着；replace 不往历史里塞条目。
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

  // 右栏「任务」tab 的角标与聊天壳里的冲突提示读同一份在飞计数（实时优先、台账兜底）。
  const inflight = useInflightTaskCount(wsId);

  // 哪个空间的对话正在跑。`connectionStore.chat` 是全局一份、隐藏空间的流也写它，
  // 拿它判断本页就会把别人的「进行中」算到自己头上：这里按 wsId 各记一位。
  const [streamingWs, setStreamingWs] = useState<Record<string, boolean>>({});
  const setWsStreaming = useCallback((id: string, streaming: boolean) => {
    setStreamingWs((previous) =>
      previous[id] === streaming ? previous : { ...previous, [id]: streaming },
    );
  }, []);

  const clear = useConversationClear(wsId, streamingWs[wsId] ?? false);

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

  // 直接进 URL 的空间不一定在打开集里：当前路由永远有一个壳，
  // 否则页面自己就成了白屏。
  const shellIds = useMemo(() => {
    const ids = [...openIds];
    if (wsId && !ids.includes(wsId)) ids.push(wsId);
    return ids;
  }, [openIds, wsId]);

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

  // 独占时右栏是唯一的一块：它接手整行，内容按 880 居中而不是拉满。
  const solePanel = panel.chatCollapsed;

  return (
    <div className="flex h-full min-h-0">
      <div
        data-testid="space-chat-column"
        data-collapsed={panel.chatCollapsed ? "true" : "false"}
        className={cn(
          "min-w-0 flex-1 basis-0 flex-col",
          panel.chatCollapsed ? "hidden" : "flex",
        )}
      >
        <SpaceChatHeader
          space={space.data}
          panelCollapsed={panel.collapsed}
          onExpandPanel={togglePanel}
          onCollapseChat={toggleChat}
          expandPanelRef={expandPanelRef}
          onClearConversation={clear.openDialog}
          clearDisabled={clear.disabled}
          clearDisabledReason={clear.disabledReason}
        />
        <div className="flex min-h-0 flex-1 flex-col px-4 pb-3">
          {/* 未打开的空间没有实例：历史还能看，但每一轮都会 409。
              先说明白，别让用户在 composer 里撞一次错误才发现。 */}
          {space.data.status === "closed" && (
            <SpaceClosedBanner space={space.data} />
          )}
          {/* 折叠只改这块的显隐：壳按 wsId 常驻，收起对话不会掐断在途的一轮。 */}
          {shellIds.map((id) => (
            <SpaceChatShell
              key={id}
              wsId={id}
              visible={id === wsId}
              onStreamingChange={setWsStreaming}
            />
          ))}
        </div>
      </div>

      {!panel.collapsed && (
        <aside
          data-testid="space-panel"
          data-mode={panel.mode}
          aria-label="空间右栏"
          className="flex min-w-0 flex-1 basis-0 flex-col border-l border-line bg-side"
        >
          <SpacePanelHeader
            mode={panel.mode}
            modes={SPACE_PANEL_MODES.space}
            inflight={inflight}
            chatCollapsed={panel.chatCollapsed}
            standalone={solePanel}
            onSelect={panel.selectMode}
            onExpandChat={toggleChat}
            onCollapsePanel={togglePanel}
            expandChatRef={expandChatRef}
          />
          <div
            className={cn(
              "@container/panel min-h-0 flex-1",
              solePanel && "mx-auto w-full max-w-mid px-3",
            )}
          >
            <SpaceSidePanel wsId={wsId} mode={panel.mode} />
          </div>
        </aside>
      )}

      <ClearConversationDialog
        open={clear.dialogOpen}
        pending={clear.pending}
        onCancel={clear.cancel}
        onConfirm={clear.confirm}
      />

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
