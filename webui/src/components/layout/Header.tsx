import { useLocation } from 'react-router-dom'
import { useAuthStore } from '@/stores/authStore'
import { Avatar, AvatarFallback } from '@/components/ui/avatar'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { LogOut, User } from 'lucide-react'

const PAGE_TITLES: Record<string, string> = {
  '/peco': 'Peco',
  '/chat': '对话',
  '/manage/providers': 'Provider',
  '/manage/agents': 'Agent 管理',
  '/manage/skills': 'Skill',
  '/manage/mcp': 'MCP',
  '/manage/knowledge': '知识库',
  '/tasks': '定时任务',
  '/settings': '设置',
}

interface HeaderProps {
  children?: React.ReactNode
}

export function Header({ children }: HeaderProps) {
  const location = useLocation()
  const { user, logout } = useAuthStore()

  const title = Object.entries(PAGE_TITLES).find(([path]) =>
    location.pathname.startsWith(path),
  )?.[1] ?? 'Peco'

  return (
    <header className="flex h-14 items-center justify-between border-b px-4 md:px-6 bg-background">
      <div className="flex items-center gap-2">
        {children}
        <h1 className="text-lg font-semibold">{title}</h1>
      </div>
      {user && (
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button className="flex items-center gap-2 rounded-md p-1 hover:bg-accent">
              <Avatar className="h-7 w-7">
                <AvatarFallback className="text-xs">
                  {user.username.slice(0, 2).toUpperCase()}
                </AvatarFallback>
              </Avatar>
              <span className="text-sm font-medium">{user.username}</span>
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem disabled>
              <User className="mr-2 h-4 w-4" />
              {user.email}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={logout}>
              <LogOut className="mr-2 h-4 w-4" />
              退出登录
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      )}
    </header>
  )
}
