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

import { useEffect, useMemo, useRef } from "react";
import { Loader2 } from "lucide-react";
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
  type SpaceOpenNoticePhase,
  useCloseSpace,
  useOpenSpaceOnEnter,
} from "@/components/spaces/useSpaceLifecycle";
import { WORK_INLINE_MIN, workWidthOf } from "@/components/layout/shellMetrics";
import { useViewportWidth } from "@/hooks/useViewportWidth";
import { useInflightTaskCount, useSpace } from "@/queries/spaceDetail";
import { useOpenSpacesStore } from "@/stores/openSpacesStore";
import { useSpaceStreaming } from "@/stores/spaceActivityStore";
import { useUiStore } from "@/stores/uiStore";
import { errorText } from "@/lib/api";
import { cn } from "@/lib/utils";

/**
 * 未打开的空间进来就自动开：「空间没实例」时每一轮对话都会 409，先把空间打通。
 *
 * 自动打开只是省一次点击，不是取消手动兜底：自动尝试按 `ws_id` 只发生一次，
 * 用户随后在同一页里关掉空间就不会再有第二次，此时这行说明与「重新打开」
 * 是唯一的路。打开中只报一句进度，不占错误色 —— 这是正常过程，不是故障；
 * 失败则把判断交回用户：重试是显式动作，不由轮询代劳。
 *
 * 「还没打开」与「已关闭」分开说：前者是等待，后者是用户的决定，把两者写成
 * 一句话会让刚点过「关闭」的人以为它自己又开了。
 */
function SpaceOpenNotice({
  phase,
  error,
  onRetry,
}: {
  phase: SpaceOpenNoticePhase;
  error: unknown;
  onRetry: () => void;
}) {
  if (phase === "opening") {
    return (
      <p
        role="status"
        data-slot="space-opening-notice"
        className="flex shrink-0 items-center gap-1.5 py-3 text-caption font-medium text-ink-muted"
      >
        <Loader2
          aria-hidden="true"
          className="size-3 shrink-0 motion-safe:animate-spin"
        />
        <span className="min-w-0">正在打开空间…</span>
      </p>
    );
  }

  if (phase === "failed") {
    return (
      <div className="flex shrink-0 items-start gap-2 py-3">
        <div className="min-w-0 flex-1">
          <ErrorBanner message={errorText(error)} />
        </div>
        <Button size="sm" className="h-7" onClick={onRetry}>
          重试
        </Button>
      </div>
    );
  }

  const closed = phase === "closed";
  return (
    <div className="flex shrink-0 items-center gap-2 py-3">
      <p className="min-w-0 flex-1 text-caption font-medium text-ink-muted">
        {closed ? "空间已关闭，消息发不出去。" : "空间还没打开。"}
      </p>
      <Button size="sm" className="h-7" onClick={onRetry}>
        {closed ? "重新打开" : "打开"}
      </Button>
    </div>
  );
}

export function SpaceDetailPage() {
  const { wsId = "" } = useParams();
  const space = useSpace(wsId);
  const close = useCloseSpace(wsId);
  const autoOpen = useOpenSpaceOnEnter(space.data);
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

  // 对话是否在跑由聊天壳按 wsId 上报到活动 store；清除按钮按 wsId 读取。
  const streaming = useSpaceStreaming(wsId);

  const clear = useConversationClear(wsId, streaming);

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

  return (
    <div className="flex h-full min-h-0">
      <div
        data-testid="space-chat-column"
        data-collapsed={panel.chatCollapsed ? "true" : "false"}
        // 列自己就是宽度容器：对话内容按它的宽度分档，不看视口。
        className={cn(
          "@container/chat min-w-0 flex-1 basis-0 flex-col",
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
              所以进来就自动打开，把「打开」这一步从用户手里拿走。 */}
          {autoOpen.phase !== "open" && (
            <SpaceOpenNotice
              phase={autoOpen.phase}
              error={autoOpen.error}
              onRetry={autoOpen.retry}
            />
          )}
          {/* 折叠只改这块的显隐：壳按 wsId 常驻，收起对话不会掐断在途的一轮。 */}
          {shellIds.map((id) => (
            <SpaceChatShell
              key={id}
              wsId={id}
              visible={id === wsId}
              // 隐藏的壳拿着当前路由空间的团队也无所谓：切到它时路由已换，
              // 这一行会跟着重渲染成正确的团队。
              teamId={space.data.team_id}
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
            onSelect={panel.selectMode}
            onExpandChat={toggleChat}
            onCollapsePanel={togglePanel}
            expandChatRef={expandChatRef}
          />
          {/* 收起对话后右栏接手整行：内容跟着吃满可用宽度，不再按 880 居中留白。 */}
          <div className="@container/panel min-h-0 flex-1">
            <SpaceSidePanel
              wsId={wsId}
              mode={panel.mode}
              open={space.data.status === "open"}
            />
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
