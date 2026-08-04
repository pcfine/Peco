import { useState } from "react";
import { Link, useLocation, useNavigate } from "react-router-dom";
import { useSidebarStore } from "@/stores/sidebarStore";
import { useAuthStore } from "@/stores/authStore";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import {
  Sparkles,
  Settings2,
  Clock,
  Settings,
  ChevronLeft,
  ChevronDown,
  Bot,
  Puzzle,
  Plug,
  BookOpen,
  LogOut,
} from "lucide-react";

interface NavItem {
  to: string;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
  children?: {
    to: string;
    label: string;
    icon: React.ComponentType<{ className?: string }>;
  }[];
}

const NAV_ITEMS: NavItem[] = [
  { to: "/peco", label: "Peco", icon: Sparkles },
  {
    to: "/workspace",
    label: "空间",
    icon: Settings2,
    children: [
      { to: "/workspace/agents", label: "Agent", icon: Bot },
      { to: "/workspace/skills", label: "Skill", icon: Puzzle },
      { to: "/workspace/mcp", label: "MCP", icon: Plug },
      { to: "/workspace/knowledge", label: "KnowledgeBase", icon: BookOpen },
    ],
  },
  { to: "/tasks", label: "任务", icon: Clock },
  { to: "/settings", label: "设置", icon: Settings },
];

interface SidebarProps {
  forceExpanded?: boolean;
}

export function Sidebar({ forceExpanded }: SidebarProps) {
  const { collapsed: storeCollapsed, toggle } = useSidebarStore();
  const collapsed = forceExpanded ? false : storeCollapsed;
  const { user, logout } = useAuthStore();
  const location = useLocation();
  const navigate = useNavigate();

  // Accordion state
  const [expandedManage, setExpandedManage] = useState(false);

  const handleLogout = () => {
    logout();
    navigate("/login");
  };

  return (
    <aside
      className={cn(
        "flex h-full flex-col border-r bg-sidebar text-sidebar-foreground transition-all duration-300",
        collapsed ? "w-16" : "w-60",
      )}
    >
      {/* Logo / Toggle */}
      <div className="flex h-14 items-center justify-between px-3 border-b border-sidebar-border">
        {!collapsed && (
          <span className="font-semibold text-lg tracking-tight">Peco</span>
        )}
        <Button
          variant="ghost"
          size="icon"
          onClick={toggle}
          className="h-8 w-8 shrink-0"
        >
          <ChevronLeft
            className={cn(
              "h-4 w-4 transition-transform",
              collapsed && "rotate-180",
            )}
          />
        </Button>
      </div>

      {/* Navigation */}
      <nav className="flex-1 space-y-1 p-2 overflow-y-auto">
        {NAV_ITEMS.map((item) => {
          const isActive = location.pathname.startsWith(item.to);
          const isWorkspaceSection = item.to === "/workspace";

          return (
            <div key={item.to}>
              {/* Parent item */}
              <Link
                to={item.children ? "#" : item.to}
                onClick={(e) => {
                  if (isWorkspaceSection) {
                    e.preventDefault();
                    setExpandedManage(!expandedManage);
                  }
                }}
                className={cn(
                  "flex items-center gap-3 rounded-md px-3 py-2 text-sm font-medium transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground",
                  isActive &&
                    !item.children &&
                    "bg-sidebar-accent text-sidebar-accent-foreground",
                )}
              >
                <item.icon className="h-4 w-4 shrink-0" />
                {!collapsed && (
                  <>
                    <span className="flex-1">{item.label}</span>
                    {(isWorkspaceSection) && (
                      <ChevronDown
                        className={cn(
                          "h-3 w-3 transition-transform",
                          (isWorkspaceSection && expandedManage)
                            ? ""
                            : "-rotate-90",
                        )}
                      />
                    )}
                  </>
                )}
              </Link>

              {/* Workspace section children */}
              {isWorkspaceSection &&
                !collapsed &&
                expandedManage &&
                item.children && (
                  <div className="ml-4 mt-1 space-y-1 border-l border-sidebar-border pl-3">
                    {item.children.map((child) => {
                      const childActive = location.pathname.startsWith(
                        child.to,
                      );
                      return (
                        <Link
                          key={child.to}
                          to={child.to}
                          className={cn(
                            "flex items-center gap-2 rounded-md px-2 py-1.5 text-xs transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground",
                            childActive &&
                              "bg-sidebar-accent text-sidebar-accent-foreground",
                          )}
                        >
                          <child.icon className="h-3 w-3" />
                          <span>{child.label}</span>
                        </Link>
                      );
                    })}
                  </div>
                )}
            </div>
          );
        })}
      </nav>

      {/* User — anchored to bottom */}
      {user && (
        <div className="mt-auto border-t border-sidebar-border p-3">
          <div className="flex items-center gap-3">
            <Avatar className="h-8 w-8">
              <AvatarFallback className="text-xs">
                {user.username.slice(0, 2).toUpperCase()}
              </AvatarFallback>
            </Avatar>
            {!collapsed && (
              <>
                <div className="flex-1 min-w-0">
                  <p className="truncate text-sm font-medium">
                    {user.username}
                  </p>
                  <p className="truncate text-xs text-muted-foreground">
                    {user.email}
                  </p>
                </div>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-8 w-8"
                  onClick={handleLogout}
                >
                  <LogOut className="h-4 w-4" />
                </Button>
              </>
            )}
          </div>
        </div>
      )}
    </aside>
  );
}
