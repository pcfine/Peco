// 看板栅格：1/2/6/12 四档由 `@container/work`（main 内层宽）判定。
// 拼合靠 gap:1px + 底色 --line —— 相邻面板共享同一条 1px 线，不是卡片墙。

import type { ReactNode } from "react";
import type { BoardPanelId } from "@/lib/board";
import { useBoardStore } from "@/stores/boardStore";
import { isPanelId, visiblePanels } from "@/lib/board";
import { cn } from "@/lib/utils";

/** 每块面板在四档下的跨列数（narrow / mid / wide / full）。 */
const PANEL_SPAN: Record<BoardPanelId, string> = {
  today:
    "col-span-1 @min-[640px]/work:col-span-2 @min-[1024px]/work:col-span-6 @min-[1400px]/work:col-span-7",
  spaces:
    "col-span-1 @min-[640px]/work:col-span-2 @min-[1024px]/work:col-span-6 @min-[1400px]/work:col-span-5",
  teams:
    "col-span-1 @min-[640px]/work:col-span-1 @min-[1024px]/work:col-span-3 @min-[1400px]/work:col-span-4",
  results:
    "col-span-1 @min-[640px]/work:col-span-1 @min-[1024px]/work:col-span-3 @min-[1400px]/work:col-span-8",
};

type PanelRenderers = Record<BoardPanelId, ReactNode>;

export function BoardGrid({ panels }: { panels: PanelRenderers }) {
  const order = useBoardStore((state) => state.order);
  const hidden = useBoardStore((state) => state.hidden);
  // 存档里可能有旧版本的 id：不认识的面板不渲染，也不占栅格位。
  const shown = visiblePanels({ order, hidden }).filter(isPanelId);

  return (
    <div className="board-grid grid grid-cols-1 overflow-hidden rounded-[8px] border border-line @min-[640px]/work:grid-cols-2 @min-[1024px]/work:grid-cols-6 @min-[1400px]/work:grid-cols-12">
      {shown.map((id) => (
        <div
          key={id}
          data-testid={`panel-${id}`}
          className={cn("flex min-w-0 flex-col", PANEL_SPAN[id])}
        >
          {panels[id]}
        </div>
      ))}
    </div>
  );
}
