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
    <div className="flex h-full overflow-hidden bg-background">
      {/* Desktop sidebar */}
      <div className="hidden md:block h-full shrink-0">
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

      <div className="flex flex-1 flex-col overflow-hidden">
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
        <main className="flex-1 overflow-y-auto p-4 md:p-6">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
