import { useEffect } from 'react'
import { ChatView } from '@/components/chat/ChatView'
import { pecoStreamUrl } from '@/api/peco'
import { usePecoChatStore } from '@/stores/pecoChatStore'
import { LoadingSpinner } from '@/components/common/LoadingSpinner'
import { Button } from '@/components/ui/button'
import { Trash2 } from 'lucide-react'
import { toast } from 'sonner'

export function PecoChatPage() {
  const messages = usePecoChatStore((s) => s.messages)
  const loading = usePecoChatStore((s) => s.loading)
  const loaded = usePecoChatStore((s) => s.loaded)
  const sessionKey = usePecoChatStore((s) => s.sessionKey)
  const load = usePecoChatStore((s) => s.load)
  const clear = usePecoChatStore((s) => s.clear)

  useEffect(() => {
    load()
  }, [load])

  const handleClear = async () => {
    try {
      await clear()
      toast.success('对话已清除')
    } catch {
      toast.error('清除失败')
    }
  }

  // 首次加载且无缓存数据时显示 loading
  if (loading && !loaded) return <LoadingSpinner />

  return (
    <ChatView
      key={sessionKey}
      streamUrl={pecoStreamUrl}
      initialMessages={messages}
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
