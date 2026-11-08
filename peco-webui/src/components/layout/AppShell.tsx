// 壳：rail（视口 <900 时离屏）+ 顶栏 + grid（main + dock）。
// dock 在 shell ≥1184 时占第二列，否则占第二行（底部停靠条）；
// 这个档位同时写在 CSS 的 `@min-[1184px]/shell` 与 JS 的 `useDockForm` 里，两处必须同值。

import { useState } from "react";
import { Outlet, useLocation } from "react-router-dom";
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
import { useDockStore } from "@/stores/dockStore";
import { railWidth, useUiStore } from "@/stores/uiStore";
import { cn } from "@/lib/utils";

/** 工作台页自带列 gutter：main 的留白归零，否则两侧重复留白吃掉聊天列宽度。 */
function isWorkbench(pathname: string): boolean {
  if (pathname === "/peco") return true;
  if (pathname === "/spaces/new") return false;
  return /^\/spaces\/[^/]+(\/status)?$/.test(pathname);
}

export function AppShell() {
  const viewport = useViewportWidth();
  const railOpen = useUiStore((state) => state.railOpen);
  const dockOpen = useDockStore((state) => state.open);
  const { pathname } = useLocation();
  const [navOpen, setNavOpen] = useState(false);
  useTaskEvents();
  useResolveCurrentSpace();
  useSeedOpenSpaces();

  const rail = railWidth(viewport, railOpen);

  return (
    <div className="flex h-full overflow-hidden bg-canvas text-ink">
      {viewport >= 900 ? (
        <Rail collapsed={rail !== 256} collapsible={viewport >= 1100} />
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

      {/* 容器查询的容器查不到自己，所以 `@container/shell` 挂在 grid 的父级；
          它也就是 shell 的宽度（视口 − rail），与 JS 侧的 `useDockForm` 同值。 */}
      <div className="@container/shell flex min-w-0 flex-1 flex-col">
        <TopBar onOpenNav={() => setNavOpen(true)} />
        <a
          href="#main"
          className="sr-only focus:not-sr-only focus:absolute focus:left-3 focus:top-2 focus:z-50 focus:rounded-[4px] focus:bg-raised focus:px-2 focus:py-1 focus:text-xs focus:text-ink"
        >
          跳到内容
        </a>
        <div
          className={cn(
            "grid min-h-0 min-w-0 flex-1 grid-cols-1",
            "grid-rows-[minmax(0,1fr)_auto] @min-[1184px]/shell:grid-rows-[minmax(0,1fr)]",
            // dock 关着时不要留空列：宽度全给 main。
            dockOpen && "@min-[1184px]/shell:grid-cols-[minmax(0,1fr)_360px]",
          )}
        >
          <main
            id="main"
            tabIndex={-1}
            className={cn(
              "min-h-0 min-w-0 overflow-y-auto",
              isWorkbench(pathname) ? "p-0" : "p-4 @min-[900px]/shell:p-6",
            )}
          >
            {/* 高度必须确定：详情页的 tab 内容靠 flex-1 min-h-0 自滚，
                `min-h-full` 会让它的 `h-full` 解析回 auto，表格就没有滚动区。 */}
            <div className="@container/work h-full">
              <Outlet />
            </div>
          </main>
          <DockShell />
        </div>
      </div>
    </div>
  );
}
