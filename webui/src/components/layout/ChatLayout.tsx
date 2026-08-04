// ChatLayout — Chat 应用独立布局（无 AppLayout 依赖）
//
// 提供全屏视图：顶部 mini header（40px）+ 剩余空间给子路由。
// 不依赖 Header/Sidebar/AppLayout，解决 Chat 页面布局 hack 问题。

import { Outlet, useParams, useNavigate } from "react-router-dom";
import { Button } from "@/components/ui/button";
import { ArrowLeft } from "lucide-react";

export function ChatLayout() {
  const { agentId } = useParams<{ agentId: string }>();
  const navigate = useNavigate();

  return (
    <div className="h-screen flex flex-col bg-background overflow-hidden">
      {/* Mini header */}
      <header className="flex h-10 items-center gap-3 border-b px-3 shrink-0 bg-background">
        <Button
          variant="ghost"
          size="sm"
          className="h-7 gap-1 text-xs px-2"
          onClick={() => navigate("/workspace/agents")}
        >
          <ArrowLeft className="h-3.5 w-3.5" />
          返回
        </Button>
        <span className="text-sm font-medium truncate">
          {agentId ? decodeURIComponent(agentId) : "对话"}
        </span>
      </header>

      {/* Main content area — fills remaining height */}
      <div className="flex-1 min-h-0">
        <Outlet />
      </div>
    </div>
  );
}
