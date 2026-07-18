import { Link, useLocation, useNavigate } from 'react-router-dom'
import { useSidebarStore } from '@/stores/sidebarStore'
import { useAuthStore } from '@/stores/authStore'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { Avatar, AvatarFallback } from '@/components/ui/avatar'
import {
  MessageSquare,
  Bot,
  BookOpen,
  Clock,
  Settings,
  ChevronLeft,
  LogOut,
} from 'lucide-react'

const NAV_ITEMS = [
  { to: '/chat', label: '对话', icon: MessageSquare },
  { to: '/agents', label: 'Agent 管理', icon: Bot },
  { to: '/knowledge', label: '知识库', icon: BookOpen },
  { to: '/tasks', label: '定时任务', icon: Clock },
  { to: '/settings', label: '设置', icon: Settings },
]

interface SidebarProps {
  /** When true, always render expanded (e.g. mobile Sheet) */
  forceExpanded?: boolean
}

export function Sidebar({ forceExpanded }: SidebarProps) {
  const { collapsed: storeCollapsed, toggle } = useSidebarStore()
  const collapsed = forceExpanded ? false : storeCollapsed
  const { user, logout } = useAuthStore()
  const location = useLocation()
  const navigate = useNavigate()

  const handleLogout = () => {
    logout()
    navigate('/login')
  }

  return (
    <aside
      className={cn(
        'flex h-full flex-col border-r bg-sidebar text-sidebar-foreground transition-all duration-300',
        collapsed ? 'w-16' : 'w-60',
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
            className={cn('h-4 w-4 transition-transform', collapsed && 'rotate-180')}
          />
        </Button>
      </div>

      {/* Navigation */}
      <nav className="space-y-1 p-2">
        {NAV_ITEMS.map((item) => {
          const isActive = location.pathname.startsWith(item.to)
          return (
            <Link
              key={item.to}
              to={item.to}
              className={cn(
                'flex items-center gap-3 rounded-md px-3 py-2 text-sm font-medium transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground',
                isActive && 'bg-sidebar-accent text-sidebar-accent-foreground',
              )}
            >
              <item.icon className="h-4 w-4 shrink-0" />
              {!collapsed && <span>{item.label}</span>}
            </Link>
          )
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
              <div className="flex-1 min-w-0">
                <p className="truncate text-sm font-medium">{user.username}</p>
                <p className="truncate text-xs text-muted-foreground">{user.email}</p>
              </div>
            )}
            {!collapsed && (
              <Button variant="ghost" size="icon" className="h-8 w-8" onClick={handleLogout}>
                <LogOut className="h-4 w-4" />
              </Button>
            )}
          </div>
        </div>
      )}
    </aside>
  )
}
