import { useEffect, useState } from 'react'
import { useParams, Link } from 'react-router-dom'
import { LoadingSpinner } from '@/components/common/LoadingSpinner'
import { getTaskLogs } from '@/api/tasks'
import type { TaskLog } from '@/types/task'
import { ArrowLeft, CheckCircle, XCircle, Loader2 } from 'lucide-react'
import { toast } from 'sonner'

const STATUS_ICONS: Record<string, React.ReactNode> = {
  running: <Loader2 className="h-4 w-4 animate-spin text-blue-500" />,
  success: <CheckCircle className="h-4 w-4 text-green-500" />,
  error: <XCircle className="h-4 w-4 text-red-500" />,
}

export function TaskLogsPage() {
  const { taskId } = useParams<{ taskId: string }>()
  const [logs, setLogs] = useState<TaskLog[]>([])
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    if (!taskId) return
    getTaskLogs(taskId).then(setLogs).catch(() => toast.error('加载失败')).finally(() => setLoading(false))
  }, [taskId])

  if (loading) return <LoadingSpinner />

  return (
    <div className="max-w-4xl mx-auto space-y-4">
      <div className="flex items-center gap-3">
        <Link to="/tasks" className="text-muted-foreground hover:text-foreground">
          <ArrowLeft className="h-5 w-5" />
        </Link>
        <h2 className="text-2xl font-bold">执行日志</h2>
      </div>

      {logs.length === 0 ? (
        <p className="text-muted-foreground">暂无执行日志</p>
      ) : (
        <div className="space-y-3">
          {logs.map((log) => (
            <div key={log.id} className="rounded-md border p-4">
              <div className="flex items-center gap-2 mb-2">
                {STATUS_ICONS[log.status] ?? null}
                <span className="font-medium text-sm">{log.status}</span>
                <span className="text-xs text-muted-foreground">
                  {new Date(log.started_at).toLocaleString('zh-CN')}
                  {log.finished_at && ` · 耗时 ${((new Date(log.finished_at).getTime() - new Date(log.started_at).getTime()) / 1000).toFixed(1)}s`}
                </span>
              </div>
              {log.output && (
                <details><summary className="text-sm cursor-pointer">输出</summary>
                  <pre className="text-xs mt-1 whitespace-pre-wrap bg-muted p-2 rounded max-h-48 overflow-y-auto">{log.output}</pre>
                </details>
              )}
              {log.error && <p className="text-sm text-destructive mt-1">{log.error}</p>}
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
