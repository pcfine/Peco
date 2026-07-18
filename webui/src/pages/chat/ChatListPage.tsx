import { useEffect, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { LoadingSpinner } from '@/components/common/LoadingSpinner'
import { EmptyState } from '@/components/common/EmptyState'
import { listConversations, createConversation, deleteConversation } from '@/api/conversations'
import type { Conversation } from '@/types/chat'
import { Plus, Trash2, MessageSquare } from 'lucide-react'
import { toast } from 'sonner'

export function ChatListPage() {
  const [conversations, setConversations] = useState<Conversation[]>([])
  const [loading, setLoading] = useState(true)
  const navigate = useNavigate()

  useEffect(() => {
    listConversations()
      .then(setConversations)
      .catch(() => toast.error('加载对话列表失败'))
      .finally(() => setLoading(false))
  }, [])

  const handleCreate = async () => {
    try {
      const conv = await createConversation()
      navigate(`/chat/${conv.id}`)
    } catch {
      toast.error('创建对话失败')
    }
  }

  const handleDelete = async (id: string) => {
    try {
      await deleteConversation(id)
      setConversations((prev) => prev.filter((c) => c.id !== id))
      toast.success('对话已删除')
    } catch {
      toast.error('删除对话失败')
    }
  }

  if (loading) return <LoadingSpinner />

  return (
    <div className="max-w-4xl mx-auto space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-bold">对话</h2>
        <Button onClick={handleCreate}>
          <Plus className="mr-2 h-4 w-4" /> 新建对话
        </Button>
      </div>

      {conversations.length === 0 ? (
        <EmptyState
          icon={MessageSquare}
          title="暂无对话"
          description="点击上方按钮开始一个新的对话"
          action={<Button onClick={handleCreate}>开始对话</Button>}
        />
      ) : (
        <div className="grid gap-3">
          {conversations.map((conv) => (
            <Card key={conv.id} className="group hover:bg-accent/50 transition-colors">
              <CardHeader className="flex flex-row items-center justify-between p-4">
                <Link to={`/chat/${conv.id}`} className="flex-1 min-w-0">
                  <CardTitle className="text-base truncate">{conv.title}</CardTitle>
                  <p className="text-xs text-muted-foreground mt-1">
                    {conv.agent_name ? `Agent: ${conv.agent_name}` : '默认助手'}
                    {' · '}
                    {new Date(conv.updated_at).toLocaleDateString('zh-CN')}
                  </p>
                </Link>
                <Button
                  variant="ghost"
                  size="icon"
                  className="opacity-0 group-hover:opacity-100 shrink-0"
                  onClick={() => handleDelete(conv.id)}
                >
                  <Trash2 className="h-4 w-4 text-destructive" />
                </Button>
              </CardHeader>
            </Card>
          ))}
        </div>
      )}
    </div>
  )
}
