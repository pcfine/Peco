import { useState } from "react";
import { Link, useLocation, useNavigate } from "react-router-dom";
import { useSidebarStore } from "@/stores/sidebarStore";
import { useAuthStore } from "@/stores/authStore";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { ThemeToggle } from "@/components/layout/ThemeToggle";
import { BrandMark } from "@/components/common/BrandMark";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import {
  Sparkles,
  Settings2,
  Workflow,
  Settings,
  ChevronLeft,
  ChevronDown,
  Bot,
  Puzzle,
  Plug,
  BookOpen,
  BrainCircuit,
  LogOut,
} from "lucide-react";

interface NavChild {
  to: string;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
}

interface NavItem {
  to: string;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
  children?: NavChild[];
}

const NAV_ITEMS: NavItem[] = [
  { to: "/peco", label: "Peco", icon: Sparkles },
  { to: "/memory", label: "记忆", icon: BrainCircuit },
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
  { to: "/workflows", label: "工作流", icon: Workflow },
  { to: "/settings", label: "设置", icon: Settings },
];

interface SidebarProps {
  forceExpanded?: boolean;
}

/* 激活态用淡靛蓝底，避免高饱和色块在多导航项间造成噪声。 */
export function Sidebar({ forceExpanded }: SidebarProps) {
  const { collapsed: storeCollapsed, toggle } = useSidebarStore();
  const collapsed = forceExpanded ? false : storeCollapsed;
  const { user, logout } = useAuthStore();
  const location = useLocation();
  const navigate = useNavigate();
  const [workspaceOpen, setWorkspaceOpen] = useState(false);

  const handleLogout = () => {
    logout();
    navigate("/login");
  };

  return (
    <aside
      className={cn(
        "flex h-full flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground transition-[width] duration-300 ease-out",
        collapsed ? "w-16" : "w-60",
      )}
    >
      <div className="flex h-14 items-center gap-1.5 px-3">
        {collapsed ? (
          /* 折叠态：整块头部即展开按钮。 */
          <button
            type="button"
            onClick={toggle}
            title="展开侧栏"
            aria-label="展开侧栏"
            className="mx-auto grid place-items-center rounded-lg p-1 transition-colors hover:bg-sidebar-accent/60"
          >
            <BrandMark size={30} />
          </button>
        ) : (
          <>
            <Link
              to="/peco"
              className="flex min-w-0 flex-1 items-center gap-2.5"
              aria-label="Peco"
            >
              <BrandMark size={30} />
              <span className="text-brand-gradient truncate text-[1.0625rem] font-semibold tracking-tight">
                Peco
              </span>
            </Link>
            <Button
              variant="ghost"
              size="icon-sm"
              onClick={toggle}
              className="text-muted-foreground hover:text-foreground"
              aria-label="收起侧栏"
            >
              <ChevronLeft className="h-4 w-4" />
            </Button>
          </>
        )}
      </div>

      <nav className="flex-1 space-y-0.5 overflow-y-auto px-2 py-1">
        {NAV_ITEMS.map((item) => {
          const isWorkspace = item.to === "/workspace";
          const childActive = (item.children ?? []).some((c) =>
            location.pathname.startsWith(c.to),
          );
          const isActive = isWorkspace
            ? childActive
            : location.pathname.startsWith(item.to);
          const expanded = isWorkspace ? workspaceOpen || childActive : false;

          return (
            <div key={item.to}>
              <Link
                to={item.children ? "#" : item.to}
                onClick={(e) => {
                  if (isWorkspace) {
                    e.preventDefault();
                    setWorkspaceOpen(!workspaceOpen);
                  }
                }}
                title={collapsed ? item.label : undefined}
                className={cn(
                  "group flex items-center gap-3 rounded-lg px-2.5 py-2 text-sm font-medium transition-colors",
                  "hover:bg-sidebar-accent/60",
                  isActive
                    ? "bg-sidebar-accent text-sidebar-accent-foreground"
                    : "text-sidebar-foreground/75",
                  collapsed && "justify-center px-0",
                )}
              >
                <item.icon
                  className={cn(
                    "h-4 w-4 shrink-0 transition-colors",
                    isActive
                      ? "text-sidebar-primary"
                      : "text-muted-foreground group-hover:text-sidebar-foreground",
                  )}
                />
                {!collapsed && (
                  <>
                    <span className="flex-1 truncate">{item.label}</span>
                    {isWorkspace && (
                      <ChevronDown
                        className={cn(
                          "h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform duration-200",
                          !expanded && "-rotate-90",
                        )}
                      />
                    )}
                  </>
                )}
              </Link>

              {isWorkspace && !collapsed && expanded && item.children && (
                <div className="mt-0.5 ml-4 space-y-0.5 border-l border-sidebar-border pl-3">
                  {item.children.map((child) => {
                    const active = location.pathname.startsWith(child.to);
                    return (
                      <Link
                        key={child.to}
                        to={child.to}
                        className={cn(
                          "flex items-center gap-2 rounded-md px-2 py-1.5 text-xs transition-colors",
                          active
                            ? "bg-sidebar-accent/70 font-medium text-sidebar-accent-foreground"
                            : "text-muted-foreground hover:bg-sidebar-accent/50 hover:text-sidebar-foreground",
                        )}
                      >
                        <child.icon className="h-3.5 w-3.5 shrink-0" />
                        <span className="truncate">{child.label}</span>
                      </Link>
                    );
                  })}
                </div>
              )}
            </div>
          );
        })}
      </nav>

      {user && (
        <div className="border-t border-sidebar-border p-2.5">
          {collapsed ? (
            <div className="flex flex-col items-center gap-2">
              <Avatar className="h-8 w-8">
                <AvatarFallback className="bg-primary/12 text-xs font-medium text-primary">
                  {user.username.slice(0, 2).toUpperCase()}
                </AvatarFallback>
              </Avatar>
              <ThemeToggle />
              <Button
                variant="ghost"
                size="icon-sm"
                className="text-muted-foreground"
                onClick={handleLogout}
                aria-label="退出登录"
              >
                <LogOut className="h-4 w-4" />
              </Button>
            </div>
          ) : (
            <div className="flex items-center gap-2">
              <Avatar className="h-8 w-8">
                <AvatarFallback className="bg-primary/12 text-xs font-medium text-primary">
                  {user.username.slice(0, 2).toUpperCase()}
                </AvatarFallback>
              </Avatar>
              <div className="min-w-0 flex-1">
                <p className="truncate text-sm font-medium">{user.username}</p>
                <p className="truncate text-xs text-muted-foreground">
                  {user.email}
                </p>
              </div>
              <ThemeToggle />
              <Button
                variant="ghost"
                size="icon-sm"
                className="text-muted-foreground"
                onClick={handleLogout}
                aria-label="退出登录"
              >
                <LogOut className="h-4 w-4" />
              </Button>
            </div>
          )}
        </div>
      )}
    </aside>
  );
}
