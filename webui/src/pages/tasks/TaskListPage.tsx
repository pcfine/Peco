import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Switch } from '@/components/ui/switch'
import { LoadingSpinner } from '@/components/common/LoadingSpinner'
import { EmptyState } from '@/components/common/EmptyState'
import { listTasks, deleteTask, toggleTask } from '@/api/tasks'
import type { Task } from '@/types/task'
import { Plus, Trash2, Clock, FileText } from 'lucide-react'
import { toast } from 'sonner'
import cronstrue from 'cronstrue'

export function TaskListPage() {
  const [tasks, setTasks] = useState<Task[]>([])
  const [loading, setLoading] = useState(true)

  const load = () => {
    listTasks().then(setTasks).catch(() => toast.error('加载失败')).finally(() => setLoading(false))
  }
  useEffect(() => { load() }, [])

  const handleDelete = async (id: string) => {
    try {
      await deleteTask(id)
      setTasks((p) => p.filter((t) => t.id !== id))
      toast.success('已删除')
    } catch { toast.error('删除失败') }
  }

  const handleToggle = async (id: string) => {
    try {
      const res = await toggleTask(id)
      setTasks((p) => p.map((t) => (t.id === id ? { ...t, enabled: res.enabled } : t)))
      toast.success(res.enabled ? '已启用' : '已禁用')
    } catch { toast.error('操作失败') }
  }

  if (loading) return <LoadingSpinner />

  return (
    <div className="max-w-4xl mx-auto space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-bold">定时任务</h2>
        <Link to="/tasks/new">
          <Button><Plus className="mr-2 h-4 w-4" />创建任务</Button>
        </Link>
      </div>

      {tasks.length === 0 ? (
        <EmptyState icon={Clock} title="暂无定时任务" />
      ) : (
        <div className="space-y-3">
          {tasks.map((t) => {
            let desc = t.cron_expr
            try { desc = cronstrue.toString(t.cron_expr, { locale: 'zh_CN' }) } catch { /* ignore */ }
            return (
              <Card key={t.id} className="group">
                <CardContent className="flex items-center gap-4 p-4">
                  <Clock className="h-8 w-8 text-muted-foreground shrink-0" />
                  <div className="flex-1 min-w-0">
                    <p className="font-medium">{t.name}</p>
                    <p className="text-xs text-muted-foreground">{t.agent_name ?? '未知 Agent'} · {desc}</p>
                    <p className="text-xs text-muted-foreground">prompt: {t.prompt.slice(0, 80)}</p>
                    {t.last_run_at && <p className="text-xs text-muted-foreground">上次运行: {new Date(t.last_run_at).toLocaleString('zh-CN')}</p>}
                  </div>
                  <div className="flex items-center gap-2 shrink-0">
                    <Switch checked={t.enabled} onCheckedChange={() => handleToggle(t.id)} />
                    <Link to={`/tasks/${t.id}/logs`}>
                      <Button variant="ghost" size="icon"><FileText className="h-4 w-4" /></Button>
                    </Link>
                    <Button variant="ghost" size="icon" className="opacity-0 group-hover:opacity-100"
                      onClick={() => handleDelete(t.id)}>
                      <Trash2 className="h-4 w-4 text-destructive" />
                    </Button>
                  </div>
                </CardContent>
              </Card>
            )
          })}
        </div>
      )}
    </div>
  )
}
