import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { LoadingSpinner } from '@/components/common/LoadingSpinner'
import { EmptyState } from '@/components/common/EmptyState'
import { listAgents, deleteAgent } from '@/api/agents'
import type { AgentListItem } from '@/types/agent'
import { Plus, Trash2, Bot, Pencil } from 'lucide-react'
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
        <Link to="/agents/new">
          <Button><Plus className="mr-2 h-4 w-4" />创建 Agent</Button>
        </Link>
      </div>

      {agents.length === 0 ? (
        <EmptyState icon={Bot} title="暂无 Agent" description="创建一个 Agent 开始使用" />
      ) : (
        <div className="grid gap-3 md:grid-cols-2">
          {agents.map((a) => (
            <Card key={a.id} className="group">
              <CardContent className="flex items-center gap-4 p-4">
                <div className="flex h-12 w-12 items-center justify-center rounded-lg text-2xl" style={{ background: a.color + '20' }}>
                  {a.icon}
                </div>
                <div className="flex-1 min-w-0">
                  <p className="font-medium truncate">{a.name}</p>
                  <p className="text-xs text-muted-foreground truncate">{a.description || a.model || ''}</p>
                  <div className="flex gap-1 mt-1">
                    {(a.tools ?? []).slice(0, 3).map((t) => (
                      <span key={t} className="rounded bg-accent px-1.5 py-0.5 text-xs">{t}</span>
                    ))}
                  </div>
                </div>
                <div className="flex gap-1 opacity-0 group-hover:opacity-100">
                  <Link to={`/agents/${a.id}/edit`}>
                    <Button variant="ghost" size="icon"><Pencil className="h-4 w-4" /></Button>
                  </Link>
                  <Button variant="ghost" size="icon" onClick={() => handleDelete(a.id)}>
                    <Trash2 className="h-4 w-4 text-destructive" />
                  </Button>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </div>
  )
}
