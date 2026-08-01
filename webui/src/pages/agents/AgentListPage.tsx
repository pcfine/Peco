import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { LoadingSpinner } from '@/components/common/LoadingSpinner'
import { EmptyState } from '@/components/common/EmptyState'
import { listAgents, deleteAgent } from '@/api/agents'
import type { AgentListItem } from '@/types/agent'
import { Plus, Trash2, Bot, Pencil, Wrench, Database, Cpu } from 'lucide-react'
import { toast } from 'sonner'

export function AgentListPage() {
  const [agents, setAgents] = useState<AgentListItem[]>([])
  const [loading, setLoading] = useState(true)

  const load = () => {
    listAgents()
      .then(setAgents)
      .catch(() => toast.error('加载 Agent 列表失败'))
      .finally(() => setLoading(false))
  }

  useEffect(() => { load() }, [])

  const handleDelete = async (id: string) => {
    try {
      await deleteAgent(id)
      setAgents((prev) => prev.filter((a) => a.id !== id))
      toast.success('Agent 已删除')
    } catch {
      toast.error('删除失败')
    }
  }

  if (loading) return <LoadingSpinner />

  return (
    <div className="max-w-4xl mx-auto space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-bold">Agent 管理</h2>
        <Link to="/manage/agents/new">
          <Button><Plus className="mr-2 h-4 w-4" />创建 Agent</Button>
        </Link>
      </div>

      {agents.length === 0 ? (
        <EmptyState icon={Bot} title="暂无 Agent" description="创建一个 Agent 开始使用" />
      ) : (
        <div className="grid gap-3 md:grid-cols-2">
          {agents.map((a) => (
            <Link key={a.id} to={`/manage/agents/${a.id}/edit`} className="block">
              <Card className="group h-[180px] hover:border-primary/50 transition-colors cursor-pointer">
                <CardContent className="p-4 h-full flex flex-col">
                  {/* 顶部：图标 + 名称 + 操作按钮 */}
                  <div className="flex items-start gap-3">
                    <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg text-xl" style={{ background: a.color + '20' }}>
                      {a.icon}
                    </div>
                    <div className="flex-1 min-w-0">
                      <p className="font-medium truncate">{a.name}</p>
                      <p className="text-xs text-muted-foreground truncate">{a.description || ''}</p>
                    </div>
                    <div className="flex gap-1 opacity-0 group-hover:opacity-100 shrink-0">
                      <Button
                        variant="ghost"
                        size="icon"
                        className="pointer-events-none"
                        tabIndex={-1}
                      >
                        <Pencil className="h-4 w-4" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={(e) => { e.stopPropagation(); e.preventDefault(); handleDelete(a.id) }}
                      >
                        <Trash2 className="h-4 w-4 text-destructive" />
                      </Button>
                    </div>
                  </div>

                  {/* 元数据区域 */}
                  <div className="flex-1 min-h-0 mt-2 space-y-1.5">
                    {/* 模型 */}
                    {a.model && (
                      <div className="flex items-center gap-1 text-xs text-muted-foreground">
                        <Cpu className="h-3 w-3 shrink-0" />
                        <span className="truncate">{a.model}</span>
                      </div>
                    )}

                    {/* 工具 — 单行，最多显示 3 个 */}
                    {(a.tools ?? []).length > 0 && (
                      <div className="flex items-center gap-1 overflow-hidden">
                        <Wrench className="h-3 w-3 shrink-0 text-muted-foreground" />
                        {(a.tools ?? []).slice(0, 3).map((t) => (
                          <span key={t} className="rounded bg-accent px-1.5 py-0.5 text-xs truncate max-w-[90px]">{t}</span>
                        ))}
                        {(a.tools ?? []).length > 3 && (
                          <span className="text-xs text-muted-foreground shrink-0">+{(a.tools ?? []).length - 3}</span>
                        )}
                      </div>
                    )}

                    {/* 知识库 — 单行，最多显示 3 个 */}
                    {(a.knowledge_bases ?? []).length > 0 && (
                      <div className="flex items-center gap-1 overflow-hidden">
                        <Database className="h-3 w-3 shrink-0 text-muted-foreground" />
                        {(a.knowledge_bases ?? []).slice(0, 3).map((kb) => (
                          <span key={kb} className="rounded bg-accent px-1.5 py-0.5 text-xs truncate max-w-[90px]">{kb}</span>
                        ))}
                        {(a.knowledge_bases ?? []).length > 3 && (
                          <span className="text-xs text-muted-foreground shrink-0">+{(a.knowledge_bases ?? []).length - 3}</span>
                        )}
                      </div>
                    )}
                  </div>
                </CardContent>
              </Card>
            </Link>
          ))}
        </div>
      )}
    </div>
  )
}
