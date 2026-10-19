import { useState } from "react";
import { Sidebar } from "./Sidebar";
import { Outlet } from "react-router-dom";
import { Sheet, SheetContent } from "@/components/ui/sheet";
import { Menu } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useSidebarStore } from "@/stores/sidebarStore";

export function AppLayout() {
  const [mobileOpen, setMobileOpen] = useState(false);
  const collapsed = useSidebarStore((s) => s.collapsed);

  return (
    <div className="relative flex h-full overflow-hidden bg-background">
      {/* 画布氛围光晕，强度见 --glow-strength */}
      <div
        aria-hidden="true"
        className="canvas-glow pointer-events-none absolute inset-0"
      />

      {/* Desktop sidebar */}
      <div className="relative z-10 hidden h-full shrink-0 md:block">
        <Sidebar />
      </div>

      {/* Mobile sidebar (Sheet) */}
      <Sheet open={mobileOpen} onOpenChange={setMobileOpen}>
        <SheetContent side="left" className="p-0 w-60">
          <div className="h-full">
            {/* Use collapsed=false for mobile sheet even if desktop is collapsed */}
            <Sidebar forceExpanded />
          </div>
        </SheetContent>
      </Sheet>

      <div className="relative z-10 flex flex-1 flex-col overflow-hidden">
        {/* 移动端专用菜单栏：仅保留汉堡按钮用于打开侧边栏 */}
        <div className="md:hidden flex h-10 items-center border-b shrink-0">
          <Button
            variant="ghost"
            size="icon"
            className="h-9 w-9"
            onClick={() => setMobileOpen(true)}
          >
            <Menu className="h-5 w-5" />
          </Button>
        </div>
        <main className="flex-1 overflow-y-auto p-4 md:p-6 xl:p-8">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
