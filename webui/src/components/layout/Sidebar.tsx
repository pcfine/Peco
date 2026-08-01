import { useEffect, useState } from 'react'
import { Link, useLocation, useNavigate } from 'react-router-dom'
import { useSidebarStore } from '@/stores/sidebarStore'
import { useAuthStore } from '@/stores/authStore'
import { listConversations } from '@/api/conversations'
import { listAgents } from '@/api/agents'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { Avatar, AvatarFallback } from '@/components/ui/avatar'
import {
  Sparkles,
  MessageSquare,
  Settings2,
  Clock,
  Settings,
  ChevronLeft,
  ChevronDown,
  Cpu,
  Bot,
  Puzzle,
  Plug,
  BookOpen,
  LogOut,
  Archive,
} from 'lucide-react'
import type { Conversation } from '@/types/chat'
import type { AgentListItem } from '@/types/agent'

interface ConversationGroup {
  agentName: string
  conversations: Conversation[]
}

interface NavItem {
  to: string
  label: string
  icon: React.ComponentType<{ className?: string }>
  children?: { to: string; label: string; icon: React.ComponentType<{ className?: string }> }[]
}

const NAV_ITEMS: NavItem[] = [
  { to: '/peco', label: 'Peco', icon: Sparkles },
  { to: '/chat', label: '对话', icon: MessageSquare },
  {
    to: '/manage',
    label: '管理',
    icon: Settings2,
    children: [
      { to: '/manage/providers', label: 'Provider', icon: Cpu },
      { to: '/manage/agents', label: 'Agent', icon: Bot },
      { to: '/manage/skills', label: 'Skill', icon: Puzzle },
      { to: '/manage/mcp', label: 'MCP', icon: Plug },
      { to: '/manage/knowledge', label: 'KnowledgeBase', icon: BookOpen },
    ],
  },
  { to: '/tasks', label: '任务', icon: Clock },
  { to: '/settings', label: '设置', icon: Settings },
]

interface SidebarProps {
  forceExpanded?: boolean
}

export function Sidebar({ forceExpanded }: SidebarProps) {
  const { collapsed: storeCollapsed, toggle } = useSidebarStore()
  const collapsed = forceExpanded ? false : storeCollapsed
  const { user, logout } = useAuthStore()
  const location = useLocation()
  const navigate = useNavigate()

  // Accordion state
  const [expandedChat, setExpandedChat] = useState(false)
  const [expandedManage, setExpandedManage] = useState(false)
  const [chatGroups, setChatGroups] = useState<ConversationGroup[]>([])
  const [agents, setAgents] = useState<AgentListItem[]>([])

  // Load conversation groups
  useEffect(() => {
    listAgents()
      .then((allAgents) => {
        const visible = allAgents.filter((a) => !a.name.startsWith('@'))
        setAgents(visible)
        return Promise.all(
          visible.map(async (agent) => {
            try {
              const convs = await listConversations(agent.name, 'active')
              return { agentName: agent.name, conversations: convs }
            } catch {
              return { agentName: agent.name, conversations: [] as Conversation[] }
            }
          }),
        )
      })
      .then((groups) => {
        setChatGroups(groups.filter((g) => g.conversations.length > 0))
      })
      .catch(() => {})
  }, [])

  // Auto-expand chat when there are conversations
  useEffect(() => {
    if (chatGroups.length > 0) {
      setExpandedChat(true)
    }
  }, [chatGroups])

  const handleLogout = () => {
    logout()
    navigate('/login')
  }

  const hasConversations = chatGroups.length > 0
  const hasAgents = agents.length > 0

  return (
    <aside
      className={cn(
        'flex h-full flex-col border-r bg-sidebar text-sidebar-foreground transition-all duration-300',
        collapsed ? 'w-16' : 'w-60',
      )}
    >
      {/* Logo / Toggle */}
      <div className="flex h-14 items-center justify-between px-3 border-b border-sidebar-border">
        {!collapsed && <span className="font-semibold text-lg tracking-tight">Peco</span>}
        <Button variant="ghost" size="icon" onClick={toggle} className="h-8 w-8 shrink-0">
          <ChevronLeft className={cn('h-4 w-4 transition-transform', collapsed && 'rotate-180')} />
        </Button>
      </div>

      {/* Navigation */}
      <nav className="flex-1 space-y-1 p-2 overflow-y-auto">
        {NAV_ITEMS.map((item) => {
          const isActive = location.pathname.startsWith(item.to)
          const isChatSection = item.to === '/chat'
          const isManageSection = item.to === '/manage'

          return (
            <div key={item.to}>
              {/* Parent item */}
              <Link
                to={item.children ? '#' : item.to}
                onClick={(e) => {
                  if (isChatSection) {
                    e.preventDefault()
                    setExpandedChat(!expandedChat)
                  } else if (isManageSection) {
                    e.preventDefault()
                    setExpandedManage(!expandedManage)
                  }
                }}
                className={cn(
                  'flex items-center gap-3 rounded-md px-3 py-2 text-sm font-medium transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground',
                  isActive && !item.children && 'bg-sidebar-accent text-sidebar-accent-foreground',
                )}
              >
                <item.icon className="h-4 w-4 shrink-0" />
                {!collapsed && (
                  <>
                    <span className="flex-1">{item.label}</span>
                    {(isChatSection || isManageSection) && (
                      <ChevronDown
                        className={cn(
                          'h-3 w-3 transition-transform',
                          (isChatSection && expandedChat) || (isManageSection && expandedManage)
                            ? ''
                            : '-rotate-90',
                        )}
                      />
                    )}
                  </>
                )}
              </Link>

              {/* Chat section children — conversation groups */}
              {isChatSection && !collapsed && expandedChat && (
                <div className="ml-4 mt-1 space-y-1 border-l border-sidebar-border pl-3">
                  {!hasConversations && !hasAgents && (
                    <p className="text-xs text-muted-foreground py-2 px-2">
                      💡 暂无对话
                      <br />
                      前往「管理 &gt; Agent」创建第一个 Agent
                    </p>
                  )}
                  {!hasConversations && hasAgents && (
                    <div>
                      {agents.map((agent) => (
                        <Link
                          key={agent.id}
                          to={`/chat/${agent.name}`}
                          className="flex items-center justify-between rounded-md px-2 py-1 text-xs hover:bg-sidebar-accent"
                        >
                          <span>
                            {agent.icon} {agent.name}
                          </span>
                          <span className="text-muted-foreground">+ 新对话</span>
                        </Link>
                      ))}
                    </div>
                  )}
                  {hasConversations &&
                    chatGroups.map((group) => (
                      <div key={group.agentName} className="mb-2">
                        <Link
                          to={`/chat/${group.agentName}`}
                          className="block text-xs font-medium px-2 py-1 text-muted-foreground hover:text-foreground"
                        >
                          {group.agentName}
                        </Link>
                        {group.conversations.map((conv) => (
                          <Link
                            key={conv.id}
                            to={`/chat/${group.agentName}/${conv.id}`}
                            className={cn(
                              'block rounded-md px-2 py-1 text-xs truncate hover:bg-sidebar-accent',
                              location.pathname.includes(conv.id) && 'bg-sidebar-accent',
                            )}
                          >
                            📝 {conv.title}
                          </Link>
                        ))}
                      </div>
                    ))}
                  {/* Archived link */}
                  {hasConversations && (
                    <Link
                      to="#"
                      className="flex items-center gap-1 text-xs text-muted-foreground px-2 py-1 hover:text-foreground"
                    >
                      <Archive className="h-3 w-3" />
                      已归档
                    </Link>
                  )}
                </div>
              )}

              {/* Manage section children */}
              {isManageSection && !collapsed && expandedManage && item.children && (
                <div className="ml-4 mt-1 space-y-1 border-l border-sidebar-border pl-3">
                  {item.children.map((child) => {
                    const childActive = location.pathname.startsWith(child.to)
                    return (
                      <Link
                        key={child.to}
                        to={child.to}
                        className={cn(
                          'flex items-center gap-2 rounded-md px-2 py-1.5 text-xs transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground',
                          childActive && 'bg-sidebar-accent text-sidebar-accent-foreground',
                        )}
                      >
                        <child.icon className="h-3 w-3" />
                        <span>{child.label}</span>
                      </Link>
                    )
                  })}
                </div>
              )}
            </div>
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
              <>
                <div className="flex-1 min-w-0">
                  <p className="truncate text-sm font-medium">{user.username}</p>
                  <p className="truncate text-xs text-muted-foreground">{user.email}</p>
                </div>
                <Button variant="ghost" size="icon" className="h-8 w-8" onClick={handleLogout}>
                  <LogOut className="h-4 w-4" />
                </Button>
              </>
            )}
          </div>
        </div>
      )}
    </aside>
  )
}
