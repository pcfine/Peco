import { useEffect, useState } from 'react'
import { ChatView, snapshotToMessages } from '@/components/chat/ChatView'
import { getPecoSession, clearPecoSession, pecoStreamUrl } from '@/api/peco'
import { LoadingSpinner } from '@/components/common/LoadingSpinner'
import { Button } from '@/components/ui/button'
import { Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import type { ChatMessage } from '@/components/chat/ChatView'

export function PecoChatPage() {
  const [initialMessages, setInitialMessages] = useState<ChatMessage[]>([])
  const [loading, setLoading] = useState(true)
  const [sessionId, setSessionId] = useState(0) // increment to force ChatView remount

  useEffect(() => {
    getPecoSession()
      .then((snap) => setInitialMessages(snapshotToMessages(snap.turns)))
      .catch(() => {})
      .finally(() => setLoading(false))
  }, [sessionId])

  const handleClear = async () => {
    try {
      await clearPecoSession()
      setSessionId((s) => s + 1)
      toast.success('对话已清除')
    } catch {
      toast.error('清除失败')
    }
  }

  if (loading) return <LoadingSpinner />

  return (
    <ChatView
      key={sessionId}
      streamUrl={pecoStreamUrl}
      initialMessages={initialMessages}
      headerTitle="Peco"
      headerActions={
        <Button variant="ghost" size="sm" onClick={handleClear}>
          <Trash2 className="h-4 w-4 mr-1" />
          清除对话
        </Button>
      }
      welcomeMessage={
        <>
          <p className="text-lg">👋 你好！我是 Peco，你的个人 AI 助理。</p>
          <p className="text-sm mt-2">由 @assistant 驱动 · 我可以执行命令、管理记忆、搜索知识库。</p>
        </>
      }
    />
  )
}
