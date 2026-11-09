// 壳：rail（视口 <900 时离屏）+ 顶栏（仅 <1100）+ main。
// Peco 抽屉是覆盖层，不占列也不占行，所以这里只有一列。

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
import { railWidth, useUiStore } from "@/stores/uiStore";
import { cn } from "@/lib/utils";

/** 工作台页自带列 gutter：main 的留白归零，否则两侧重复留白吃掉聊天列宽度。 */
function isWorkbench(pathname: string): boolean {
  if (pathname === "/peco") return true;
  // 团队详情是全高面板页（左分类导航 + 右内容各自滚动）：main 一留白，
  // 分类导航就和 rail 之间多出一条缝，且 max-w-wide 居中在宽屏留下更大空白。
  if (/^\/teams\/[^/]+$/.test(pathname)) return true;
  if (pathname === "/spaces/new") return false;
  return /^\/spaces\/[^/]+(\/status)?$/.test(pathname);
}

export function AppShell() {
  const viewport = useViewportWidth();
  const railOpen = useUiStore((state) => state.railOpen);
  const { pathname } = useLocation();
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
          className="sr-only focus:not-sr-only focus:absolute focus:left-3 focus:top-2 focus:z-50 focus:rounded-[4px] focus:bg-raised focus:px-2 focus:py-1 focus:text-xs focus:text-ink"
        >
          跳到内容
        </a>
        <main
          id="main"
          tabIndex={-1}
          className={cn(
            "min-h-0 min-w-0 flex-1 overflow-y-auto",
            isWorkbench(pathname) ? "p-0" : "p-4 @min-[900px]/shell:p-6",
          )}
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
