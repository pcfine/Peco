// ChatView — 共享聊天组件，被 PecoChatPage 和 AgentChatPage 共用

import { useEffect, useState, useRef, useCallback } from 'react'
import { Button } from '@/components/ui/button'
import { useAuthStore } from '@/stores/authStore'
import type { ChatSseEvent, MessageData, TurnData } from '@/types/chat'
import { Send, Square } from 'lucide-react'
import { toast } from 'sonner'
import { parseSSELines, toChatSseEvent } from '@/api/stream'
import { MarkdownRenderer } from '@/components/chat/MarkdownRenderer'

// ── Types ─────────────────────────────────────────────────────────────────

export interface ChatMessage {
  role: 'user' | 'assistant' | 'tool' | 'agent-call'
  content: string
  turnIndex: number
  toolCalls?: { id: string; name: string; arguments: string; result?: string }[]
  reasoning?: string
  agentName?: string
  agentTask?: string
  callId?: string
}

export interface ChatViewProps {
  /** 生成 SSE 流式 URL 的函数，接收用户输入消息，返回完整 URL */
  streamUrl: (message: string) => string
  /** 初始消息列表（从快照恢复） */
  initialMessages?: ChatMessage[]
  /** 头部右侧操作区（如清除对话按钮、归档按钮） */
  headerActions?: React.ReactNode
  /** 头部标题 */
  headerTitle?: string
  /** 消息列表为空时显示的欢迎内容 */
  welcomeMessage?: React.ReactNode
  /** 提交反馈回调 */
  onFeedback?: (messageId: string, rating: 'up' | 'down') => Promise<void>
}

// ── Component ──────────────────────────────────────────────────────────────

export function ChatView({
  streamUrl,
  initialMessages = [],
  headerActions,
  headerTitle = '对话',
  welcomeMessage,
}: ChatViewProps) {
  const [messages, setMessages] = useState<ChatMessage[]>(initialMessages)
  const [input, setInput] = useState('')
  const [streaming, setStreaming] = useState(false)
  const abortRef = useRef<AbortController | null>(null)
  const messagesEndRef = useRef<HTMLDivElement>(null)
  const token = useAuthStore((s) => s.token)

  // Update messages when initialMessages change (e.g. navigation between conversations)
  useEffect(() => {
    setMessages(initialMessages)
  }, [initialMessages])

  // Refs for stable closure access
  const inputRef = useRef(input)
  inputRef.current = input
  const streamingRef = useRef(streaming)
  streamingRef.current = streaming

  // Auto-scroll
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [messages])

  const handleSend = useCallback(async () => {
    const currentInput = inputRef.current
    if (!currentInput.trim() || !token || streamingRef.current) return

    const userMsg: ChatMessage = { role: 'user', content: currentInput, turnIndex: 0 }
    setMessages((prev) => [...prev, userMsg])
    setInput('')
    setStreaming(true)

    const assistantMsg: ChatMessage = { role: 'assistant', content: '', turnIndex: 0 }
    setMessages((prev) => [...prev, assistantMsg])

    const controller = new AbortController()
    abortRef.current = controller

    try {
      const url = streamUrl(currentInput)
      const response = await fetch(url, {
        headers: { Authorization: `Bearer ${token}` },
        signal: controller.signal,
      })
      if (!response.ok) {
        const errText = await response.text().catch(() => '')
        throw new Error(errText || `HTTP ${response.status}`)
      }
      const reader = response.body!.getReader()
      const decoder = new TextDecoder()
      let buffer = ''

      while (true) {
        const { done, value } = await reader.read()
        if (done) break

        const chunk = decoder.decode(value, { stream: true })
        const { events, remaining } = parseSSELines(chunk, buffer)
        buffer = remaining

        for (const parsed of events) {
          const event = toChatSseEvent(parsed)
          if (event) handleSSEEvent(event, setMessages, setStreaming)
        }
      }
    } catch (err: unknown) {
      if (err instanceof Error && err.name === 'AbortError') return
      toast.error('连接中断')
    } finally {
      setStreaming(false)
      abortRef.current = null
    }
  }, [token, streamUrl])

  const handleStop = () => {
    abortRef.current?.abort()
    setStreaming(false)
  }

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      handleSend()
    }
  }

  return (
    <div className="flex flex-col h-[calc(100vh-7rem)] max-w-4xl mx-auto">
      {/* Header */}
      <div className="flex items-center gap-3 py-3 border-b mb-4">
        <h2 className="font-semibold flex-1">{headerTitle}</h2>
        {headerActions}
      </div>

      {/* Messages */}
      <div className="flex-1 overflow-y-auto space-y-4 pr-2">
        {messages.length === 0 && welcomeMessage && (
          <div className="text-center text-muted-foreground mt-20">{welcomeMessage}</div>
        )}
        {messages.map((msg, i) => (
          <ChatBubble key={i} message={msg} />
        ))}
        <div ref={messagesEndRef} />
      </div>

      {/* Input */}
      <div className="flex gap-2 pt-4 border-t mt-4">
        <input
          className="flex-1 rounded-md border px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-primary bg-background"
          placeholder="输入消息..."
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          disabled={streaming}
        />
        {streaming ? (
          <Button variant="destructive" onClick={handleStop}>
            <Square className="h-4 w-4" />
          </Button>
        ) : (
          <Button onClick={handleSend} disabled={!input.trim()}>
            <Send className="h-4 w-4" />
          </Button>
        )}
      </div>
    </div>
  )
}

// ── ChatBubble ─────────────────────────────────────────────────────────────

function ChatBubble({ message }: { message: ChatMessage }) {
  if (message.role === 'user') {
    return (
      <div className="flex justify-end">
        <div className="bg-primary text-primary-foreground rounded-lg px-4 py-2 max-w-[80%] text-sm">
          {message.content}
        </div>
      </div>
    )
  }

  if (message.role === 'agent-call') {
    return (
      <div className="flex gap-2 items-start">
        <div className="bg-accent rounded-lg px-4 py-2 text-sm border border-accent-foreground/10 max-w-[80%]">
          <p className="font-medium text-xs text-muted-foreground">
            🤖 子 Agent: {message.agentName}
          </p>
          <p className="text-xs text-muted-foreground">任务: {message.agentTask}</p>
          {message.content && <MarkdownRenderer content={message.content} />}
        </div>
      </div>
    )
  }

  if (message.role === 'tool') {
    return (
      <div className="flex gap-2 items-start">
        <div className="bg-muted/50 rounded-lg px-3 py-1.5 text-xs text-muted-foreground max-w-[85%] font-mono whitespace-pre-wrap break-all">
          {message.content}
        </div>
      </div>
    )
  }

  // Assistant message
  return (
    <div className="flex gap-2 items-start">
      <div className="bg-muted rounded-lg px-4 py-2 text-sm max-w-[80%]">
        {message.reasoning && (
          <details className="mb-2">
            <summary className="text-xs text-muted-foreground cursor-pointer">推理过程</summary>
            <p className="text-xs text-muted-foreground mt-1 whitespace-pre-wrap">
              {message.reasoning}
            </p>
          </details>
        )}
        {message.toolCalls?.map((tc) => (
          <details key={tc.id} className="mb-2">
            <summary className="text-xs font-medium cursor-pointer">
              🔧 {tc.name} {tc.result ? '✓' : '...'}
            </summary>
            <pre className="text-xs text-muted-foreground mt-1 whitespace-pre-wrap max-h-32 overflow-y-auto">
              {tc.result || tc.arguments}
            </pre>
          </details>
        ))}
        {message.content && <MarkdownRenderer content={message.content} />}
      </div>
    </div>
  )
}

// ── SSE Event Handler ──────────────────────────────────────────────────────

function handleSSEEvent(
  event: ChatSseEvent,
  setMessages: React.Dispatch<React.SetStateAction<ChatMessage[]>>,
  setStreaming: React.Dispatch<React.SetStateAction<boolean>>,
) {
  switch (event.event) {
    case 'text_delta':
      setMessages((prev) => {
        const last = prev[prev.length - 1]
        if (last?.role === 'assistant') {
          return [...prev.slice(0, -1), { ...last, content: last.content + event.data.content }]
        }
        return prev
      })
      break
    case 'reasoning_delta':
      setMessages((prev) => {
        const last = prev[prev.length - 1]
        if (last?.role === 'assistant') {
          return [
            ...prev.slice(0, -1),
            { ...last, reasoning: (last.reasoning ?? '') + event.data.content },
          ]
        }
        return prev
      })
      break
    case 'tool_call_start':
      setMessages((prev) => {
        const last = prev[prev.length - 1]
        if (last?.role === 'assistant') {
          const tc = {
            id: event.data.id,
            name: event.data.name,
            arguments: event.data.arguments,
          }
          return [...prev.slice(0, -1), { ...last, toolCalls: [...(last.toolCalls ?? []), tc] }]
        }
        return prev
      })
      break
    case 'tool_result':
      setMessages((prev) => {
        const last = prev[prev.length - 1]
        if (last?.role === 'assistant' && last.toolCalls) {
          const updated = last.toolCalls.map((tc) =>
            tc.id === event.data.id ? { ...tc, result: event.data.result } : tc,
          )
          return [...prev.slice(0, -1), { ...last, toolCalls: updated }]
        }
        return prev
      })
      break
    case 'agent_call_start':
      setMessages((prev) => [
        ...prev,
        {
          role: 'agent-call' as const,
          content: '',
          turnIndex: 0,
          agentName: event.data.agent_name,
          agentTask: event.data.task,
          callId: event.data.call_id,
        },
      ])
      break
    case 'agent_call_end':
      setMessages((prev) =>
        prev.map((m) =>
          m.callId === event.data.call_id ? { ...m, content: event.data.result || '(完成)' } : m,
        ),
      )
      break
    case 'turn_complete':
    case 'done':
      setStreaming(false)
      break
    case 'error':
      setStreaming(false)
      break
  }
}

// ── Snapshot to Messages ───────────────────────────────────────────────────

export function snapshotToMessages(turns: TurnData[]): ChatMessage[] {
  return turns.flatMap((turn) =>
    turn.messages.map((md: MessageData): ChatMessage => {
      if (md.role === 'user') {
        return { role: 'user', content: md.content ?? '', turnIndex: turn.turn_index }
      }
      if (md.role === 'tool') {
        return { role: 'tool', content: md.content ?? '', turnIndex: turn.turn_index }
      }
      return {
        role: 'assistant',
        content: md.content ?? '',
        turnIndex: turn.turn_index,
        toolCalls: md.tool_calls?.map((tc) => ({ ...tc, result: undefined })),
        reasoning: md.reasoning_content,
      }
    }),
  )
}
