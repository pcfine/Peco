// `/spaces/:wsId` —— 空间聊天页：唯一 @master 对话 + 可收起的右栏（文件 / 任务 / 记忆）。
//
// 打开集里的每个空间都挂一个聊天壳，只有当前空间可见。React Router 在
// `/spaces/A` → `/spaces/B` 之间复用同一个路由元素，页面因此不重挂载，
// 切走的空间继续跑它的流；左栏切换只改激活态与路由。
//
// 已知限制：进 `/spaces/:wsId/status` 或 `/peco` 会离开这个路由元素，
// 隐藏空间的流随组件卸载中断。

import { useMemo } from "react";
import { Link, useParams } from "react-router-dom";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { CloseSpaceDialog } from "@/components/spaces/CloseSpaceDialog";
import { SpaceChatShell } from "@/components/spaces/SpaceChatShell";
import { SpaceDetailHeader } from "@/components/spaces/SpaceDetailHeader";
import { SpacePanelModeBar } from "@/components/spaces/SpacePanelModeBar";
import { SpaceSidePanel } from "@/components/spaces/SpaceSidePanel";
import {
  SPACE_PANEL_MODES,
  useSpacePanelState,
} from "@/components/spaces/useSpacePanelState";
import { useCloseSpace } from "@/components/spaces/useSpaceLifecycle";
import { useInflightTaskCount, useSpace } from "@/queries/spaceDetail";
import { useOpenSpacesStore } from "@/stores/openSpacesStore";
import { errorText } from "@/lib/api";

export function SpaceDetailPage() {
  const { wsId = "" } = useParams();
  const space = useSpace(wsId);
  const close = useCloseSpace(wsId);
  const openIds = useOpenSpacesStore((state) => state.openIds);
  const panel = useSpacePanelState(wsId);

  // 页头的「任务 N」角标与聊天壳里的冲突提示读同一份在飞计数（实时优先、台账兜底）。
  const inflight = useInflightTaskCount(wsId);

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
        <Link to="/spaces" className="text-xs text-primary-ink hover:underline">
          返回工作室
        </Link>
      </div>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <SpaceDetailHeader
        space={space.data}
        onClose={close.close}
        closing={close.closing}
        actions={
          <SpacePanelModeBar
            mode={panel.mode}
            collapsed={panel.collapsed}
            inflight={inflight}
            modes={SPACE_PANEL_MODES.space}
            onSelect={panel.selectMode}
            onToggleCollapsed={panel.toggleCollapsed}
          />
        }
      />

      <div className="flex min-h-0 flex-1">
        <div className="flex min-h-0 min-w-0 flex-1 flex-col p-3">
          {/* 未打开的空间没有实例：历史还能看，但每一轮都会 409。
              先说明白，别让用户在 composer 里撞一次错误才发现。 */}
          {space.data.status === "closed" && (
            <div className="shrink-0 pb-2">
              <ErrorBanner
                message="空间还没打开，消息发不出去。用页头的「打开」启动它。"
                onRetry={() => void space.refetch()}
              />
            </div>
          )}
          {shellIds.map((id) => (
            <SpaceChatShell key={id} wsId={id} visible={id === wsId} />
          ))}
        </div>

        {!panel.collapsed && (
          <aside
            data-testid="space-panel"
            data-mode={panel.mode}
            aria-label="空间右栏"
            className="flex w-80 shrink-0 flex-col border-l border-line"
          >
            <div className="min-h-0 flex-1">
              <SpaceSidePanel wsId={wsId} mode={panel.mode} />
            </div>
          </aside>
        )}
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
