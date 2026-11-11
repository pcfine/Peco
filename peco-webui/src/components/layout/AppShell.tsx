// 壳：rail（视口 <900 时离屏）+ 顶栏（仅 <1100）+ main。
// Peco 抽屉是覆盖层，不占列也不占行，所以这里只有一列。

import { useState } from "react";
import { Outlet } from "react-router-dom";
import { Rail } from "@/components/layout/Rail";
import { TopBar } from "@/components/layout/TopBar";
import { DockShell } from "@/components/dock/DockShell";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetTitle,
} from "@/components/ui/sheet";
import { useResolveCurrentSpace } from "@/hooks/useResolveCurrentSpace";
import { useSeedOpenSpaces } from "@/hooks/useOpenSpaces";
import { useTaskEvents } from "@/hooks/useTaskEvents";
import { useViewportWidth } from "@/hooks/useViewportWidth";
import { railWidth, useUiStore } from "@/stores/uiStore";

export function AppShell() {
  const viewport = useViewportWidth();
  const railOpen = useUiStore((state) => state.railOpen);
  const [navOpen, setNavOpen] = useState(false);
  useTaskEvents();
  useResolveCurrentSpace();
  useSeedOpenSpaces();

  const rail = railWidth(viewport, railOpen);

  return (
    <div className="flex h-full overflow-hidden bg-canvas text-ink">
      {viewport >= 900 ? (
        <Rail collapsed={rail !== 208} collapsible={viewport >= 1100} />
      ) : (
        <Sheet open={navOpen} onOpenChange={setNavOpen}>
          <SheetContent
            side="left"
            className="w-[280px] gap-0 border-r border-line p-0"
          >
            <SheetTitle className="sr-only">导航</SheetTitle>
            <SheetDescription className="sr-only">
              空间、团队与跨空间索引
            </SheetDescription>
            <Rail collapsed={false} collapsible={false} />
          </SheetContent>
        </Sheet>
      )}

      {/* 容器查询的容器查不到自己，所以 `@container/shell` 挂在 main 的父级：
          它也就是 shell 的宽度（视口 − rail）。 */}
      <div className="@container/shell flex min-w-0 flex-1 flex-col">
        {/* 宽屏不再有标题栏：页面名由内容区的 h1 承担，main 从 y=0 起。 */}
        {viewport < 1100 && <TopBar onOpenNav={() => setNavOpen(true)} />}
        <a
          href="#main"
          className="sr-only focus:not-sr-only focus:absolute focus:left-3 focus:top-2 focus:z-50 focus:rounded-sm focus:bg-raised focus:px-2 focus:py-1 focus:text-xs focus:text-ink"
        >
          跳到内容
        </a>
        {/* main 一律不留白：gutter 由页面自己承担 —— 工作台页在列内部，
            栏目页在 `PageFrame`。分档只存在于页面内，壳不再维护路由白名单。 */}
        <main
          id="main"
          tabIndex={-1}
          className="min-h-0 min-w-0 flex-1 overflow-hidden"
        >
          {/* 高度必须确定：详情页的 tab 内容靠 flex-1 min-h-0 自滚，
              `min-h-full` 会让它的 `h-full` 解析回 auto，表格就没有滚动区。 */}
          <div className="@container/work h-full">
            <Outlet />
          </div>
        </main>
      </div>
      {/* 抽屉挂在容器查询元素之外：`container-type` 带 layout containment，
          会成为 fixed 后代的包含块，抽屉的 `left` 就会从 rail 右缘再偏一次。 */}
      <DockShell />
    </div>
  );
}
