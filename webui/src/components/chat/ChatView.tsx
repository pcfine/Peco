// ChatView — 共享聊天组件，被 PecoChatPage 和 AgentChatPage 共用

import { useEffect, useState, useRef, useCallback } from "react";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { useAuthStore } from "@/stores/authStore";
import type {
  ChatSseEvent,
  MessageData,
  TurnData,
  UsageData,
} from "@/types/chat";
import { Send, Square, Paperclip } from "lucide-react";
import { toast } from "sonner";
import { parseSSELines, toChatSseEvent } from "@/api/stream";
import { MarkdownRenderer } from "@/components/chat/MarkdownRenderer";
import { TokenUsageRing } from "@/components/chat/TokenUsageRing";
import { DEFAULT_CONTEXT_WINDOW } from "@/lib/constants";

// ── Types ─────────────────────────────────────────────────────────────────

export interface ChatMessage {
  role: "user" | "assistant" | "tool" | "agent-call";
  content: string;
  turnIndex: number;
  toolCalls?: {
    id: string;
    name: string;
    arguments: string;
    result?: string;
  }[];
  reasoning?: string;
  agentName?: string;
  agentTask?: string;
  callId?: string;
  /** 错误提示消息（来自 SSE `error` 事件），以警示样式渲染 */
  isError?: boolean;
  /** 系统通知消息（如 SSE `context_compacted`），以居中分隔样式渲染 */
  isNotice?: boolean;
  /** 归档摘要正文（notice 消息可选）— hover 分隔条时展示 */
  summary?: string;
}

export interface ChatViewProps {
  /** 生成 SSE 流式 URL 的函数，接收用户输入消息，返回完整 URL */
  streamUrl: (message: string) => string;
  /** 初始消息列表（从快照恢复） */
  initialMessages?: ChatMessage[];
  /** 头部右侧操作区（如清除对话按钮、归档按钮） */
  headerActions?: React.ReactNode;
  /** 头部标题 */
  headerTitle?: React.ReactNode;
  /** 消息列表为空时显示的欢迎内容 */
  welcomeMessage?: React.ReactNode;
  /** 提交反馈回调 */
  onFeedback?: (messageId: string, rating: "up" | "down") => Promise<void>;
  /** 当前是否为可见对话。false 时隐藏 DOM 但保持挂载和 SSE */
  visible?: boolean;
  /** 挂载后自动发送的首条消息（仅在 snapshot 就绪后传入） */
  initialQuery?: string;
  /** 不可见时收到新消息的回调，用于 ConversationList 未读标记 */
  onUnread?: (unreadCount: number) => void;
  /** initialQuery 被发送后回调，用于父组件清理状态，防止重新挂载时重复发送 */
  onInitialQuerySent?: () => void;
  /** 附加到根元素的 CSS class（用于覆盖高度等布局属性） */
  className?: string;
  /** 上下文窗口总量（token），用于底部用量圆环的分母。默认 1M。 */
  contextWindowTokens?: number;
  /** 运行模式。`"external"` 时 ChatView 不管理自己的 SSE 连接 —
   * 消息从 initialMessages prop 读取，发送/停止委托给外部。
   * @default "internal" */
  mode?: "internal" | "external";
  /** [mode="external" 必需] 外部发送回调 */
  onExternalSend?: (text: string) => void;
  /** [mode="external" 必需] 外部停止回调 */
  onExternalStop?: () => void;
  /** [mode="external" 必需] 外部流式状态 */
  externalIsStreaming?: boolean;
  /** [mode="external"] 外部提供的当前 token 用量（驱动底部用量圆环）。 */
  externalUsage?: UsageData | null;
}

// ── Component ──────────────────────────────────────────────────────────────

export function ChatView({
  streamUrl,
  initialMessages = [],
  headerActions,
  headerTitle = "对话",
  welcomeMessage,
  visible = true,
  initialQuery,
  onUnread,
  onInitialQuerySent,
  className,
  contextWindowTokens = DEFAULT_CONTEXT_WINDOW,
  mode = "internal",
  onExternalSend,
  onExternalStop,
  externalIsStreaming,
  externalUsage = null,
}: ChatViewProps) {
  const isExternalMode = mode === "external";
  if (isExternalMode && import.meta.env.DEV) {
    if (!onExternalSend)
      console.warn("ChatView mode=external: onExternalSend is required");
    if (!onExternalStop)
      console.warn("ChatView mode=external: onExternalStop is required");
    if (externalIsStreaming === undefined)
      console.warn("ChatView mode=external: externalIsStreaming is required");
  }
  const [messages, setMessages] = useState<ChatMessage[]>(initialMessages);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const [usage, setUsage] = useState<UsageData | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const token = useAuthStore((s) => s.token);
  const unreadCountRef = useRef(0);
  const unreadDebounceRef = useRef<ReturnType<typeof setTimeout>>();

  // Refs for stable closure access — avoids effect re-trigger from prop/state changes
  const inputRef = useRef(input);
  inputRef.current = input;
  const streamingRef = useRef(streaming);
  streamingRef.current = streaming;
  const visibleRef = useRef(visible);
  visibleRef.current = visible;

  // StrictMode-safe unmount timer: setTimeout in cleanup is cleared on remount,
  // so abort only fires for genuine unmounts, not StrictMode double-invocation.
  const unmountTimerRef = useRef<ReturnType<typeof setTimeout>>();

  // Refs for callback props — eliminate dependency churn
  const streamUrlRef = useRef(streamUrl);
  streamUrlRef.current = streamUrl;
  const onUnreadRef = useRef(onUnread);
  onUnreadRef.current = onUnread;
  const onInitialQuerySentRef = useRef(onInitialQuerySent);
  onInitialQuerySentRef.current = onInitialQuerySent;
  // token from Zustand is stable after login, but capture in ref for safety
  const tokenRef = useRef(token);
  tokenRef.current = token;

  // External-mode refs — stable across renders for use in callbacks / effects.
  const onExternalSendRef = useRef(onExternalSend);
  onExternalSendRef.current = onExternalSend;
  const onExternalStopRef = useRef(onExternalStop);
  onExternalStopRef.current = onExternalStop;
  const externalIsStreamingRef = useRef(externalIsStreaming);
  externalIsStreamingRef.current = externalIsStreaming;

  // In external mode messages are driven by the store via initialMessages;
  // in local mode they are managed by our own useState + SSE handlers.
  const displayMessages = isExternalMode ? initialMessages : messages;

  // Token usage for the context ring: internal mode reads from our own SSE
  // `usage` state; external mode reads from the parent-provided prop.
  const effectiveUsage = isExternalMode ? externalUsage : usage;

  // ── Core send logic — stable identity (empty deps, reads everything from refs) ─

  const sendMessage = useCallback(async (text: string) => {
    const tok = tokenRef.current;
    if (!text.trim() || !tok || streamingRef.current) return;

    const userMsg: ChatMessage = {
      role: "user",
      content: text,
      turnIndex: 0,
    };
    setMessages((prev) => [...prev, userMsg]);
    setStreaming(true);

    const assistantMsg: ChatMessage = {
      role: "assistant",
      content: "",
      turnIndex: 0,
    };
    setMessages((prev) => [...prev, assistantMsg]);

    const controller = new AbortController();
    abortRef.current = controller;

    try {
      const url = streamUrlRef.current(text);
      const response = await fetch(url, {
        headers: { Authorization: `Bearer ${tok}` },
        signal: controller.signal,
      });
      if (!response.ok) {
        const errText = await response.text().catch(() => "");
        throw new Error(errText || `HTTP ${response.status}`);
      }
      const reader = response.body!.getReader();
      const decoder = new TextDecoder();
      let buffer = "";

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        const chunk = decoder.decode(value, { stream: true });
        const { events, remaining } = parseSSELines(chunk, buffer);
        buffer = remaining;

        for (const parsed of events) {
          const event = toChatSseEvent(parsed);
          if (event) {
            if (event.event === "usage") {
              setUsage({
                input_tokens: event.data.input_tokens,
                output_tokens: event.data.output_tokens,
              });
            }
            handleSSEEvent(event, setMessages, setStreaming, () => {
              if (!visibleRef.current && onUnreadRef.current) {
                unreadCountRef.current += 1;
                // Debounce: each text_delta fires this callback, which can be
                // hundreds of times per message. Throttle to one
                // notification per 150ms to avoid flooding the parent with
                // Map rebuilds and re-renders.
                if (unreadDebounceRef.current) {
                  clearTimeout(unreadDebounceRef.current);
                }
                unreadDebounceRef.current = setTimeout(() => {
                  onUnreadRef.current?.(unreadCountRef.current);
                }, 150);
              }
            });
          }
        }
      }
    } catch (err: unknown) {
      if (err instanceof Error && err.name === "AbortError") return;
      toast.error("连接中断");
    } finally {
      setStreaming(false);
      abortRef.current = null;
    }
  }, []);

  // ── Sync messages when initialMessages changes (pool keep-alive) ─────────

  const prevInitialMessagesRef = useRef(initialMessages);
  useEffect(() => {
    // In external mode messages are rendered directly from initialMessages
    // so there is no need to sync to local state.
    if (isExternalMode) return;
    // Only sync if the reference actually changed (not just re-render).
    // Skip reset while an SSE stream is active — parent re-renders (e.g. from
    // onInitialQuerySent) can cause the `?? []` fallback to produce a new
    // reference, which would otherwise wipe out in-flight messages.
    if (
      prevInitialMessagesRef.current !== initialMessages &&
      !streamingRef.current
    ) {
      prevInitialMessagesRef.current = initialMessages;
      setMessages(initialMessages);
    }
  }, [initialMessages, isExternalMode]);

  // ── handleSend: reads from input → clears input → delegates to sendMessage ─

  const handleSend = useCallback(async () => {
    const currentInput = inputRef.current;
    const tok = tokenRef.current;
    const streaming = externalIsStreamingRef.current ?? streamingRef.current;
    if (!currentInput.trim() || !tok || streaming) return;
    setInput("");
    if (onExternalSendRef.current) {
      onExternalSendRef.current(currentInput);
    } else {
      await sendMessage(currentInput);
    }
  }, [sendMessage]);

  // ── initialQuery auto-send ──────────────────────────────────────────────

  const hasSentInitialRef = useRef(false);
  useEffect(() => {
    const tok = tokenRef.current;
    if (initialQuery && !hasSentInitialRef.current && tok) {
      hasSentInitialRef.current = true;
      if (onExternalSendRef.current) {
        onExternalSendRef.current(initialQuery);
      } else {
        sendMessage(initialQuery);
      }
      // Defer onInitialQuerySent to the next microtask so that sendMessage's
      // synchronous state updates (setMessages, setStreaming) are committed
      // before the parent clears initialQuery.  This prevents a parent
      // re-render in the same batch from resetting in-flight messages.
      Promise.resolve().then(() => {
        onInitialQuerySentRef.current?.();
      });
    }
  }, [initialQuery]);

  // ── Reset unread when becoming visible ──────────────────────────────────

  useEffect(() => {
    if (visible) {
      unreadCountRef.current = 0;
      // Notify parent so it clears unreadCounts map for this convId.
      // Without this, switching away from a conversation that previously
      // had unread messages causes the blue dot to incorrectly reappear.
      onUnreadRef.current?.(0);
    }
  }, [visible]);

  // ── Auto-scroll (paused when hidden) ────────────────────────────────────

  useEffect(() => {
    if (visible) {
      messagesEndRef.current?.scrollIntoView({ behavior: "instant" });
    }
  }, [displayMessages, visible]);

  // ── Cleanup on unmount ─────────────────────────────────────────────────

  // When the component unmounts, abort the SSE stream so the browser releases
  // the TCP connection and the server-side AgentLooper is cancelled.
  //
  // We delay abort via setTimeout(0) so React StrictMode double-invocation
  // (mount → unmount → remount, synchronous in dev) can cancel it: the remount
  // effect clears the timer before the macrotask fires. For a genuine unmount
  // there is no follow-up remount, so the timer fires and the stream is killed.
  useEffect(() => {
    // Mount: cancel any pending abort from a StrictMode unmount cycle
    if (unmountTimerRef.current) {
      clearTimeout(unmountTimerRef.current);
      unmountTimerRef.current = undefined;
    }
    return () => {
      // In external mode the SSE lifecycle is managed by the store —
      // do NOT abort on unmount so streaming continues in the background.
      if (isExternalMode) return;
      unmountTimerRef.current = setTimeout(() => {
        abortRef.current?.abort();
      }, 0);
    };
  }, [isExternalMode]);

  // ── Handlers ────────────────────────────────────────────────────────────

  const handleStop = () => {
    if (onExternalStopRef.current) {
      onExternalStopRef.current();
    } else {
      abortRef.current?.abort();
      setStreaming(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const isInputDisabled = (externalIsStreaming ?? streaming) || !visible;

  // ── Render ──────────────────────────────────────────────────────────────

  return (
    <div
      className={`flex flex-col max-w-4xl mx-auto ${className || "h-[calc(100vh-7rem)]"}`}
    >
      {/* Header */}
      <div className="flex items-center gap-3 py-3 border-b mb-4">
        <h2 className="font-semibold flex-1">{headerTitle}</h2>
        {headerActions}
      </div>

      {/* Messages */}
      <div className="flex-1 overflow-y-auto space-y-4 pr-2">
        {displayMessages.length === 0 && welcomeMessage && (
          <div className="text-center text-muted-foreground mt-20">
            {welcomeMessage}
          </div>
        )}
        {displayMessages.map((msg, i) => (
          <ChatBubble key={i} message={msg} />
        ))}
        <div ref={messagesEndRef} />
      </div>

      {/* Input */}
      <div className="pt-4 mt-4 border-t border-muted-foreground/15">
        <div className="flex flex-col rounded-xl border bg-background focus-within:border-primary focus-within:ring-2 focus-within:ring-primary/50">
          <Textarea
            className="min-h-10 max-h-40 resize-none border-0 px-3 pt-2.5 pb-0 shadow-none focus-visible:ring-0"
            placeholder={isInputDisabled ? "加载中…" : "输入消息..."}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            disabled={isInputDisabled}
            rows={1}
          />

          {/* 底部工具行：左侧预留扩展按钮，右侧用量圆环 + 发送/停止 */}
          <div className="flex items-center justify-between gap-2 px-1.5 py-1">
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  disabled
                  aria-label="文件上传（即将推出）"
                >
                  <Paperclip className="h-4 w-4" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>文件上传（即将推出）</TooltipContent>
            </Tooltip>

            <div className="flex items-center gap-1">
              {/* 用量圆环：external 模式（Peco）的 usage 由父组件传入。 */}
              <TokenUsageRing
                inputTokens={effectiveUsage?.input_tokens ?? 0}
                outputTokens={effectiveUsage?.output_tokens}
                contextWindow={contextWindowTokens}
              />
              {(externalIsStreaming ?? streaming) ? (
                <Button variant="destructive" size="sm" onClick={handleStop}>
                  <Square className="h-4 w-4" />
                </Button>
              ) : (
                <Button
                  size="sm"
                  onClick={handleSend}
                  disabled={!input.trim() || !visible}
                >
                  <Send className="h-4 w-4" />
                </Button>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

// ── ChatBubble ─────────────────────────────────────────────────────────────

function ChatBubble({ message }: { message: ChatMessage }) {
  if (message.isNotice) {
    return (
      <div className="group relative flex items-center gap-3 py-1 text-xs text-muted-foreground">
        <div className="bg-border h-px flex-1" />
        <span className="whitespace-nowrap">📦 {message.content}</span>
        <div className="bg-border h-px flex-1" />
        {message.summary && (
          // hover 归档分隔条展示摘要正文
          <div
            className="bg-popover text-popover-foreground border absolute top-full left-1/2 z-10 mt-2 hidden max-h-64 max-w-md -translate-x-1/2 overflow-auto rounded-md p-3 text-xs whitespace-pre-wrap shadow-md group-hover:block"
            role="tooltip"
          >
            <p className="text-muted-foreground mb-2 font-medium">归档摘要</p>
            {message.summary}
          </div>
        )}
      </div>
    );
  }

  if (message.role === "user") {
    return (
      <div className="flex justify-end">
        <div className="bg-primary text-primary-foreground rounded-lg px-4 py-2 max-w-[80%] text-sm">
          {message.content}
        </div>
      </div>
    );
  }

  if (message.role === "agent-call") {
    return (
      <div className="flex gap-2 items-start">
        <div className="bg-accent rounded-lg px-4 py-2 text-sm border border-accent-foreground/10 max-w-[80%]">
          <p className="font-medium text-xs text-muted-foreground">
            🤖 子 Agent: {message.agentName}
          </p>
          <p className="text-xs text-muted-foreground">
            任务: {message.agentTask}
          </p>
          {message.content && <MarkdownRenderer content={message.content} />}
        </div>
      </div>
    );
  }

  if (message.role === "tool") {
    return (
      <div className="flex gap-2 items-start">
        <div className="bg-muted/50 rounded-lg px-3 py-1.5 text-xs text-muted-foreground max-w-[85%] font-mono whitespace-pre-wrap break-all">
          {message.content}
        </div>
      </div>
    );
  }

  // Assistant message
  return (
    <div className="flex gap-2 items-start">
      <div
        className={
          message.isError
            ? "bg-destructive/10 border border-destructive/30 text-destructive rounded-lg px-4 py-2 text-sm max-w-[80%]"
            : "bg-muted rounded-lg px-4 py-2 text-sm max-w-[80%]"
        }
      >
        {message.reasoning && (
          <details className="mb-2">
            <summary className="text-xs text-muted-foreground cursor-pointer">
              推理过程
            </summary>
            <p className="text-xs text-muted-foreground mt-1 whitespace-pre-wrap">
              {message.reasoning}
            </p>
          </details>
        )}
        {message.toolCalls?.map((tc) => (
          <details key={tc.id} className="mb-2">
            <summary className="text-xs font-medium cursor-pointer">
              🔧 {tc.name} {tc.result ? "✓" : "..."}
            </summary>
            <pre className="text-xs text-muted-foreground mt-1 whitespace-pre-wrap max-h-32 overflow-y-auto">
              {tc.result || tc.arguments}
            </pre>
          </details>
        ))}
        {message.content && <MarkdownRenderer content={message.content} />}
      </div>
    </div>
  );
}

// ── Shared SSE Event Reducer ─────────────────────────────────────────────────
//
// Pure functions: take current messages + an SSE event, return new messages.
// Exported so pecoChatStore can reuse them — keep the two copies in sync by
// making this the single source of truth for message transformation logic.

/** Apply a single SSE event to a messages array (pure — no side effects). */
export function reduceStreamEvent(
  event: ChatSseEvent,
  messages: ChatMessage[],
): ChatMessage[] {
  switch (event.event) {
    case "text_delta": {
      const last = messages[messages.length - 1];
      if (last?.role === "assistant") {
        return [
          ...messages.slice(0, -1),
          { ...last, content: last.content + event.data.content },
        ];
      }
      return messages;
    }
    case "reasoning_delta": {
      const last = messages[messages.length - 1];
      if (last?.role === "assistant") {
        return [
          ...messages.slice(0, -1),
          { ...last, reasoning: (last.reasoning ?? "") + event.data.content },
        ];
      }
      return messages;
    }
    case "tool_call_start": {
      const last = messages[messages.length - 1];
      if (last?.role === "assistant") {
        const tc = {
          id: event.data.id,
          name: event.data.name,
          arguments: event.data.arguments,
        };
        return [
          ...messages.slice(0, -1),
          { ...last, toolCalls: [...(last.toolCalls ?? []), tc] },
        ];
      }
      return messages;
    }
    case "tool_result": {
      const last = messages[messages.length - 1];
      if (last?.role === "assistant" && last.toolCalls) {
        const updated = last.toolCalls.map((tc) =>
          tc.id === event.data.id ? { ...tc, result: event.data.result } : tc,
        );
        return [...messages.slice(0, -1), { ...last, toolCalls: updated }];
      }
      return messages;
    }
    case "agent_call_start":
      return [
        ...messages,
        {
          role: "agent-call" as const,
          content: "",
          turnIndex: 0,
          agentName: event.data.agent_name,
          agentTask: event.data.task,
          callId: event.data.call_id,
        },
      ];
    case "agent_call_end":
      return messages.map((m) =>
        m.callId === event.data.call_id
          ? { ...m, content: event.data.result || "(完成)" }
          : m,
      );
    case "turn_complete":
    case "done":
    case "usage":
      return messages;
    case "context_compacted":
      // 更早的对话已被归档为结构化摘要 — 以居中分隔线提示（hover 可看摘要正文）
      return [
        ...messages,
        {
          role: "assistant" as const,
          content: `更早的 ${event.data.evicted_turns} 轮对话已归档为摘要，仍在模型上下文中`,
          turnIndex: 0,
          isNotice: true,
          summary: event.data.summary,
        },
      ];
    case "error":
      // 后端失败轮次（模型错误、超时、取消等）以警示气泡展示错误信息
      return [
        ...messages,
        {
          role: "assistant" as const,
          content: event.data.message,
          turnIndex: 0,
          isError: true,
        },
      ];
  }
}

/** Whether this SSE event signals the end of streaming (UI should show stop→send). */
export function isStreamTerminalEvent(event: ChatSseEvent): boolean {
  switch (event.event) {
    case "turn_complete":
    case "done":
    case "error":
      return true;
    default:
      return false;
  }
}

// ── Internal SSE handler (wires reduceStreamEvent into React state) ──────────

function handleSSEEvent(
  event: ChatSseEvent,
  setMessages: React.Dispatch<React.SetStateAction<ChatMessage[]>>,
  setStreaming: React.Dispatch<React.SetStateAction<boolean>>,
  onTextDelta?: () => void,
) {
  if (event.event === "text_delta") {
    onTextDelta?.();
  }

  setMessages((prev) => reduceStreamEvent(event, prev));

  // Eagerly update streaming flag for responsive UI.
  // The AbortController / finally block in sendMessage is the safety net
  // that guarantees streaming is cleaned up even if these events never arrive.
  if (isStreamTerminalEvent(event)) {
    setStreaming(false);
  }
}

// ── Snapshot to Messages ───────────────────────────────────────────────────

export function snapshotToMessages(turns: TurnData[]): ChatMessage[] {
  return turns.flatMap((turn) =>
    turn.messages.map((md: MessageData): ChatMessage => {
      if (md.role === "user") {
        return {
          role: "user",
          content: md.content ?? "",
          turnIndex: turn.turn_index,
        };
      }
      if (md.role === "tool") {
        return {
          role: "tool",
          content: md.content ?? "",
          turnIndex: turn.turn_index,
        };
      }
      return {
        role: "assistant",
        content: md.content ?? "",
        turnIndex: turn.turn_index,
        toolCalls: md.tool_calls?.map((tc) => ({ ...tc, result: undefined })),
        reasoning: md.reasoning_content,
      };
    }),
  );
}
