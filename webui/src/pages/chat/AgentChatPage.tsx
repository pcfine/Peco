import { useEffect, useState, useCallback } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { ChatView, snapshotToMessages } from '@/components/chat/ChatView'
import { getSessionSnapshot, createConversation } from '@/api/conversations'
import { LoadingSpinner } from '@/components/common/LoadingSpinner'
import { Button } from '@/components/ui/button'
import { Archive, Plus } from 'lucide-react'
import { toast } from 'sonner'
import type { ChatMessage } from '@/components/chat/ChatView'

export function AgentChatPage() {
  const { agentId, conversationId } = useParams<{ agentId: string; conversationId?: string }>()
  const navigate = useNavigate()
  const [initialMessages, setInitialMessages] = useState<ChatMessage[]>([])
  const [loading, setLoading] = useState(true)
  const [convId, setConvId] = useState(conversationId)

  useEffect(() => {
    if (!agentId) return
    if (convId) {
      getSessionSnapshot(agentId, convId)
        .then((snap) => setInitialMessages(snapshotToMessages(snap.turns)))
        .catch(() => {})
        .finally(() => setLoading(false))
    } else {
      setInitialMessages([])
      setLoading(false)
    }
  }, [agentId, convId])

  const handleNewConversation = useCallback(async () => {
    if (!agentId) return
    try {
      const conv = await createConversation(agentId)
      navigate(`/chat/${agentId}/${conv.id}`, { replace: true })
      setConvId(conv.id)
      setInitialMessages([])
    } catch {
      toast.error('创建对话失败')
    }
  }, [agentId, navigate])

  const handleArchive = useCallback(async () => {
    if (!agentId || !convId) return
    try {
      const { updateConversation } = await import('@/api/conversations')
      await updateConversation(agentId, convId, { archive: true })
      toast.success('对话已归档')
    } catch {
      toast.error('归档失败')
    }
  }, [agentId, convId])

  if (loading) return <LoadingSpinner />

  const streamUrl = (message: string) =>
    `/api/chat/${agentId}/conversations/${convId}/stream?message=${encodeURIComponent(message)}`

  return (
    <ChatView
      key={`${agentId}-${convId}`}
      streamUrl={streamUrl}
      initialMessages={initialMessages}
      headerTitle={agentId ?? '对话'}
      headerActions={
        <div className="flex gap-2">
          {convId && (
            <Button variant="ghost" size="sm" onClick={handleArchive}>
              <Archive className="h-4 w-4 mr-1" />
              归档
            </Button>
          )}
          <Button variant="ghost" size="sm" onClick={handleNewConversation}>
            <Plus className="h-4 w-4 mr-1" />
            新对话
          </Button>
        </div>
      }
      welcomeMessage={
        <div>
          <p className="text-lg">👋 开始与 <strong>{agentId}</strong> 对话</p>
          <p className="text-sm mt-2">发送第一条消息开始新的对话。</p>
        </div>
      }
    />
  )
}
