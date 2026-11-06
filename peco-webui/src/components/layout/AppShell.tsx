// 壳：rail（视口 <900 时离屏）+ 顶栏 + main（唯一页面级滚动）+ dock。
// main 的档位由 `@container/main`（main-inner 宽）判定，不用视口前缀。

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
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { useResolveCurrentSpace } from "@/hooks/useResolveCurrentSpace";
import { useTaskEvents } from "@/hooks/useTaskEvents";
import { useUiStore } from "@/stores/uiStore";

export function AppShell() {
  const wide = useMediaQuery("(min-width: 900px)");
  const railOpen = useUiStore((state) => state.railOpen);
  const [navOpen, setNavOpen] = useState(false);
  useTaskEvents();
  useResolveCurrentSpace();

  return (
    <div className="flex h-full overflow-hidden bg-canvas text-ink">
      {wide ? (
        <Rail collapsed={!railOpen} />
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
            <Rail collapsed={false} />
          </SheetContent>
        </Sheet>
      )}

      <div className="flex min-w-0 flex-1 flex-col">
        <TopBar onOpenNav={() => setNavOpen(true)} />
        <div className="@container/shell flex min-h-0 flex-1">
          <main className="min-w-0 flex-1 overflow-y-auto p-5 @min-[1024px]/shell:p-6">
            {/* 高度必须确定：详情页的 tab 内容靠 flex-1 min-h-0 自滚，
                `min-h-full` 会让它的 `h-full` 解析回 auto，表格就没有滚动区。 */}
            <div className="@container/main h-full">
              <Outlet />
            </div>
          </main>
          <DockShell />
        </div>
      </div>
    </div>
  );
}
