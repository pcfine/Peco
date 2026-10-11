// ChatView — 共享聊天组件，被 PecoChatPage 和 AgentChatPage 共用

import {
  useEffect,
  useState,
  useRef,
  useCallback,
  useLayoutEffect,
} from "react";
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
import {
  Send,
  Square,
  Paperclip,
  X,
  FileText,
  Pause,
  Play,
  Loader2,
} from "lucide-react";
import { toast } from "sonner";
import { parseSSELines, toChatSseEvent } from "@/api/stream";
import { MarkdownRenderer } from "@/components/chat/MarkdownRenderer";
import { TokenUsageRing } from "@/components/chat/TokenUsageRing";
import { DEFAULT_CONTEXT_WINDOW } from "@/lib/constants";

// ── Types ─────────────────────────────────────────────────────────────────

/**
 * 附件扩展名白名单 — 与服务端分类白名单对齐。驱动 input accept 与发送前的
 * 图片能力预筛；类型是否受支持始终由服务端权威判定（不匹配回 400 可读文案）。
 */
const IMAGE_FILE_EXTS = ["png", "jpg", "jpeg", "gif", "webp"];
const DOC_FILE_EXTS = [
  "pdf",
  "md",
  "markdown",
  "txt",
  "text",
  "log",
  "html",
  "htm",
  "csv",
  "json",
  "yaml",
  "yml",
  "toml",
  "xml",
  "rs",
  "py",
  "js",
  "jsx",
  "ts",
  "tsx",
  "go",
  "java",
];

/** 取文件名小写扩展名；无扩展名返回空串。 */
function extensionOf(filename: string): string {
  const i = filename.lastIndexOf(".");
  return i >= 0 ? filename.slice(i + 1).toLowerCase() : "";
}

/** 从 axios 错误里取服务端可读文案（{ error, details }），取不到回退通用文案。 */
function uploadErrorMessage(err: unknown, name: string): string {
  const data = (
    err as { response?: { data?: { details?: string; error?: string } } }
  )?.response?.data;
  return data?.details || data?.error || `附件上传失败：${name}`;
}

/** 待发送附件（图片与文档统一入口）。图片有渲染 url，文档只带引用 id 与原名。 */
export interface ChatAttachment {
  id: string;
  kind: "image" | "document";
  name: string;
  url?: string;
}

export interface ChatMessage {
  role: "user" | "assistant" | "tool" | "agent-call";
  content: string;
  turnIndex: number;
  /** 稳定标识（快照消息恒有）；分页前置插入时据此保持 DOM 节点不重挂。 */
  id?: string;
  toolCalls?: {
    id: string;
    name: string;
    arguments: string;
    result?: string;
    /** 工具输出图片（data URI），来自 SSE `tool_result` 事件 */
    images?: string[];
  }[];
  reasoning?: string;
  agentName?: string;
  agentTask?: string;
  callId?: string;
  /** 用户消息携带的图片（data URI）。发送当轮来自 pendingAttachments，刷新后经快照 DTO 恢复 */
  images?: string[];
  /** 用户消息携带的文档附件名（附件正文不进展示层），来自快照 DTO attachments */
  attachments?: { name: string }[];
  /** 错误提示消息（来自 SSE `error` 事件），以警示样式渲染 */
  isError?: boolean;
  /** 系统通知消息（如 SSE `context_compacted`），以居中分隔样式渲染 */
  isNotice?: boolean;
  /** 分隔条图标（notice 消息可选），缺省为归档语义的 📦 */
  noticeIcon?: string;
  /** 归档摘要正文（notice 消息可选）— hover 分隔条时展示 */
  summary?: string;
}

export interface ChatViewProps {
  /** 生成 SSE 流式 URL 的函数，接收用户输入消息与附件引用 id，返回完整 URL */
  streamUrl: (message: string, fileIds: string[]) => string;
  /** 是否支持图片输入。false 时图片文件被拒（附件入口本身由 uploadFile 决定） */
  supportsImages?: boolean;
  /** 上传图片回调（既有图片通路），返回引用 id 与渲染 URL。supportsImages 时必需 */
  uploadImage?: (file: File) => Promise<{ id: string; url: string }>;
  /** 上传附件回调（图片与文档统一端点）。提供时附件入口可用，与 supportsImages 解耦 */
  uploadFile?: (file: File) => Promise<ChatAttachment>;
  /** 文件选择框 accept；缺省按内置图片/文档白名单拼接（仅 uploadFile 存在时为扩展名列表） */
  accept?: string;
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
  /** [mode="external" 必需] 外部发送回调；attachments 为待发送附件（无附件时为空数组）。 */
  onExternalSend?: (text: string, attachments: ChatAttachment[]) => void;
  /** [mode="external" 必需] 外部停止回调 */
  onExternalStop?: () => void;
  /** [mode="external" 必需] 外部流式状态 */
  externalIsStreaming?: boolean;
  /** [mode="external"] 外部提供的当前 token 用量（驱动底部用量圆环）。 */
  externalUsage?: UsageData | null;
  /** [mode="external"] 外部提供的暂停态 —— external 模式下 SSE 由 store 管理，
   *  本地循环收不到 `looper_state`，故以本 prop 为权威值。缺省按 false 处理；
   *  internal 模式忽略本 prop。 */
  externalPaused?: boolean;
  /**
   * [可选] 暂停当前运行中的 looper。与 `onResume` **同时提供**时才出现入口，
   * 且只在流式进行中展示（`ExternalIsStreaming ?? streaming`）。
   *
   * 暂停在下一个步进边界才生效，按钮状态以 SSE `looper_state` 事件为准。
   */
  onPause?: () => void;
  /** [可选] 恢复已暂停的 looper（与 `onPause` 成对提供）。 */
  onResume?: () => void;
  /** [mode="external" 可选] 拉取更早一页对话的回调（顶部按钮触发）。 */
  onLoadEarlier?: () => void;
  /** [mode="external" 可选] 是否还有更早的轮可加载（决定按钮显隐）。 */
  hasMore?: boolean;
  /** [mode="external" 可选] 「加载更早」请求进行中（按钮置灰）。 */
  loadingEarlier?: boolean;
}

// ── Component ──────────────────────────────────────────────────────────────

export function ChatView({
  streamUrl,
  supportsImages = false,
  uploadImage,
  uploadFile,
  accept,
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
  externalPaused = false,
  onPause,
  onResume,
  onLoadEarlier,
  hasMore = false,
  loadingEarlier = false,
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
  /** internal 模式的暂停态（由本地 SSE `looper_state` 事件驱动） */
  const [paused, setPaused] = useState(false);
  /** 已点暂停、服务端尚未挂起（流式在途）时的暂态提示 */
  const [pausePending, setPausePending] = useState(false);
  const [usage, setUsage] = useState<UsageData | null>(null);
  /** 已上传待发送的附件（图片 + 文档） */
  const [pendingAttachments, setPendingAttachments] = useState<
    ChatAttachment[]
  >([]);
  const [uploading, setUploading] = useState(false);
  const abortRef = useRef<AbortController | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const token = useAuthStore((s) => s.token);
  const unreadCountRef = useRef(0);
  const unreadDebounceRef = useRef<ReturnType<typeof setTimeout> | undefined>(
    undefined,
  );

  // Refs for stable closure access — avoids effect re-trigger from prop/state changes
  const inputRef = useRef(input);
  inputRef.current = input;
  const streamingRef = useRef(streaming);
  streamingRef.current = streaming;
  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  const pendingAttachmentsRef = useRef(pendingAttachments);
  pendingAttachmentsRef.current = pendingAttachments;

  // StrictMode-safe unmount timer: setTimeout in cleanup is cleared on remount,
  // so abort only fires for genuine unmounts, not StrictMode double-invocation.
  const unmountTimerRef = useRef<ReturnType<typeof setTimeout> | undefined>(
    undefined,
  );

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
    const attachments = pendingAttachmentsRef.current;
    if (
      (!text.trim() && attachments.length === 0) ||
      !tok ||
      streamingRef.current
    )
      return;

    // 上传中的附件不允许随消息发送（upload 完成前禁用发送即可达，这里兜底跳过）
    const fileIds = attachments.map((att) => att.id);

    const userMsg: ChatMessage = {
      role: "user",
      content: text,
      turnIndex: 0,
      images: attachments
        .filter((att) => att.kind === "image" && att.url)
        .map((att) => att.url as string),
      attachments: attachments
        .filter((att) => att.kind === "document")
        .map((att) => ({ name: att.name })),
    };
    setMessages((prev) => [...prev, userMsg]);
    setStreaming(true);
    setPaused(false);
    setPendingAttachments([]);

    const assistantMsg: ChatMessage = {
      role: "assistant",
      content: "",
      turnIndex: 0,
    };
    setMessages((prev) => [...prev, assistantMsg]);

    const controller = new AbortController();
    abortRef.current = controller;

    try {
      const url = streamUrlRef.current(text, fileIds);
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
      let pendingEvent = "";

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        const chunk = decoder.decode(value, { stream: true });
        const {
          events,
          remaining,
          pendingEvent: next,
        } = parseSSELines(chunk, buffer, pendingEvent);
        buffer = remaining;
        pendingEvent = next;

        for (const parsed of events) {
          const event = toChatSseEvent(parsed);
          if (event) {
            if (event.event === "usage") {
              setUsage({
                input_tokens: event.data.input_tokens,
                output_tokens: event.data.output_tokens,
              });
            }
            handleSSEEvent(event, setMessages, setStreaming, setPaused, () => {
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
      setPaused(false);
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
    const attachments = pendingAttachmentsRef.current;
    if ((!currentInput.trim() && attachments.length === 0) || !tok || streaming)
      return;
    setInput("");
    if (onExternalSendRef.current) {
      setPendingAttachments([]);
      onExternalSendRef.current(currentInput, attachments);
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
        onExternalSendRef.current(initialQuery, []);
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

  // ── 滚动锚定（分页前置插入不拽底）────────────────────────────────────
  //
  // 记录滚前 `gap = scrollHeight - scrollTop`，渲染后按增量回补
  // `scrollTop = scrollHeight - gap`：前置插入旧轮时 gap 不变 → 视口不动；
  // 仅当用户本就贴底（distanceToBottom 很小）时继续贴底（流式/尾部追加）。
  const scrollContainerRef = useRef<HTMLDivElement>(null);
  const stickToBottomRef = useRef(true);
  const prevGeometryRef = useRef({ scrollHeight: 0, scrollTop: 0 });

  const handleScroll = useCallback(() => {
    const el = scrollContainerRef.current;
    if (!el) return;
    const distanceToBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    stickToBottomRef.current = distanceToBottom < 80;
    prevGeometryRef.current = {
      scrollHeight: el.scrollHeight,
      scrollTop: el.scrollTop,
    };
  }, []);

  useLayoutEffect(() => {
    const el = scrollContainerRef.current;
    if (!el || !visible) return;
    if (stickToBottomRef.current) {
      el.scrollTop = el.scrollHeight;
    } else {
      const gap =
        prevGeometryRef.current.scrollHeight -
        prevGeometryRef.current.scrollTop;
      el.scrollTop = el.scrollHeight - gap;
    }
    prevGeometryRef.current = {
      scrollHeight: el.scrollHeight,
      scrollTop: el.scrollTop,
    };
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

  // ── 附件上传：附件按钮 + 粘贴 ──────────────────────────────────────────
  //
  // 附件入口由 uploadFile（统一附件端点）决定，与 supportsImages 解耦；
  // supportsImages 只拦图片文件 —— 文档不依赖模型视觉能力。

  const canAttach = !!uploadFile || (supportsImages && !!uploadImage);

  const addAttachments = useCallback(
    async (files: File[]) => {
      if (files.length === 0) return;

      // 无附件端点（agent 会话页）：仅图片通路，行为与接入附件端点前一致
      if (!uploadFile) {
        if (!supportsImages || !uploadImage) return;
        setUploading(true);
        try {
          for (const file of files) {
            try {
              const uploaded = await uploadImage(file);
              setPendingAttachments((prev) => [
                ...prev,
                { ...uploaded, kind: "image" as const, name: file.name },
              ]);
            } catch {
              toast.error(`图片上传失败：${file.name}`);
            }
          }
        } finally {
          setUploading(false);
        }
        return;
      }

      setUploading(true);
      try {
        for (const file of files) {
          const ext = extensionOf(file.name);
          const isImage =
            IMAGE_FILE_EXTS.includes(ext) ||
            (ext === "" && file.type.startsWith("image/"));

          if (isImage) {
            if (!supportsImages) {
              toast.error("当前模型不支持图片输入");
              continue;
            }
            // 既有图片通路（/images + uploadImage）优先，行为不变
            if (uploadImage) {
              try {
                const uploaded = await uploadImage(file);
                setPendingAttachments((prev) => [
                  ...prev,
                  { ...uploaded, kind: "image", name: file.name },
                ]);
              } catch {
                toast.error(`图片上传失败：${file.name}`);
              }
              continue;
            }
          }

          try {
            const uploaded = await uploadFile(file);
            setPendingAttachments((prev) => [...prev, uploaded]);
          } catch (err) {
            toast.error(uploadErrorMessage(err, file.name));
          }
        }
      } finally {
        setUploading(false);
      }
    },
    [uploadImage, uploadFile, supportsImages],
  );

  const handleFileInputChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files ?? []);
    e.target.value = ""; // 允许重复选择同一文件
    void addAttachments(files);
  };

  const handlePaste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    if (!canAttach) return;
    const files = Array.from(e.clipboardData.files).filter((f) => {
      if (uploadFile) return true; // 统一端点：图片 + 文档都收（能力预筛在 addAttachments）
      // 仅图片通路（agent 会话页）：按 MIME 过滤，行为与接入附件端点前一致
      return f.type.startsWith("image/");
    });
    if (files.length > 0) {
      e.preventDefault();
      void addAttachments(files);
    }
  };

  const acceptAttr =
    accept ??
    (uploadFile
      ? [...IMAGE_FILE_EXTS, ...DOC_FILE_EXTS].map((e) => `.${e}`).join(",")
      : "image/png,image/jpeg,image/gif,image/webp");

  const isStreaming = externalIsStreaming ?? streaming;
  // external 模式的 SSE 由 store 管理，本地循环收不到 `looper_state`，
  // 故以父组件传入的 externalPaused 为权威；internal 模式仍认本地事件。
  const effectivePaused = isExternalMode ? externalPaused : paused;
  const isInputDisabled = isStreaming || !visible;

  // 暂态的两个解除点：服务端确认挂起，或本轮结束（按钮随门控消失）
  useEffect(() => {
    if (effectivePaused || !isStreaming) setPausePending(false);
  }, [effectivePaused, isStreaming]);

  // 暂态期间再点一次 = 撤回尚未落地的暂停
  const handlePause = useCallback(() => {
    if (pausePending || effectivePaused) {
      onResume?.();
    } else {
      setPausePending(true);
      onPause?.();
    }
  }, [pausePending, effectivePaused, onPause, onResume]);

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
      <div
        ref={scrollContainerRef}
        onScroll={handleScroll}
        className="flex-1 overflow-y-auto space-y-4 pr-2"
      >
        {onLoadEarlier && hasMore && (
          <div className="flex justify-center">
            <Button
              variant="outline"
              size="sm"
              onClick={onLoadEarlier}
              disabled={loadingEarlier || (externalIsStreaming ?? streaming)}
            >
              {loadingEarlier ? "加载中…" : "加载更早的对话"}
            </Button>
          </div>
        )}
        {displayMessages.length === 0 && welcomeMessage && (
          <div className="text-center text-muted-foreground mt-20">
            {welcomeMessage}
          </div>
        )}
        {displayMessages.map((msg, i) => (
          <ChatBubble key={msg.id ?? i} message={msg} />
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
            onPaste={handlePaste}
            disabled={isInputDisabled}
            rows={1}
          />

          {/* 待发送附件预览（hover 出移除按钮）：图片缩略图 + 文档名 chip */}
          {pendingAttachments.length > 0 && (
            <div className="flex flex-wrap gap-2 px-3 pt-2">
              {pendingAttachments.map((att) =>
                att.kind === "image" && att.url ? (
                  <div key={att.id} className="group relative">
                    <img
                      src={att.url}
                      alt="待发送图片"
                      className="h-16 w-16 rounded-md border object-cover"
                    />
                    <button
                      type="button"
                      aria-label="移除图片"
                      onClick={() =>
                        setPendingAttachments((prev) =>
                          prev.filter((p) => p.id !== att.id),
                        )
                      }
                      className="absolute -top-1.5 -right-1.5 hidden h-5 w-5 items-center justify-center rounded-full bg-destructive text-destructive-foreground group-hover:flex"
                    >
                      <X className="h-3 w-3" />
                    </button>
                  </div>
                ) : (
                  <div
                    key={att.id}
                    className="group relative flex items-center gap-1.5 rounded-md border bg-muted/40 py-1.5 pr-7 pl-2 text-xs"
                  >
                    <FileText className="h-3.5 w-3.5 shrink-0" />
                    <span className="max-w-[160px] truncate">{att.name}</span>
                    <button
                      type="button"
                      aria-label="移除附件"
                      onClick={() =>
                        setPendingAttachments((prev) =>
                          prev.filter((p) => p.id !== att.id),
                        )
                      }
                      className="absolute top-1/2 right-1.5 -translate-y-1/2 hidden h-5 w-5 items-center justify-center rounded-full bg-destructive text-destructive-foreground group-hover:flex"
                    >
                      <X className="h-3 w-3" />
                    </button>
                  </div>
                ),
              )}
            </div>
          )}

          {/* 底部工具行：左侧附件按钮，右侧用量圆环 + 发送/停止 */}
          <div className="flex items-center justify-between gap-2 px-1.5 py-1">
            {canAttach ? (
              <>
                <input
                  ref={fileInputRef}
                  type="file"
                  accept={acceptAttr}
                  multiple
                  hidden
                  onChange={handleFileInputChange}
                />
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      disabled={isInputDisabled || uploading}
                      aria-label={uploadFile ? "添加附件" : "添加图片"}
                      onClick={() => fileInputRef.current?.click()}
                    >
                      <Paperclip className="h-4 w-4" />
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent>
                    {uploadFile ? "添加附件" : "添加图片"}
                  </TooltipContent>
                </Tooltip>
              </>
            ) : (
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    disabled
                    aria-label="当前模型不支持图片输入"
                  >
                    <Paperclip className="h-4 w-4" />
                  </Button>
                </TooltipTrigger>
                <TooltipContent>当前模型不支持图片输入</TooltipContent>
              </Tooltip>
            )}

            <div className="flex items-center gap-1">
              {/* 用量圆环：external 模式（Peco）的 usage 由父组件传入。 */}
              <TokenUsageRing
                inputTokens={effectiveUsage?.input_tokens ?? 0}
                outputTokens={effectiveUsage?.output_tokens}
                contextWindow={contextWindowTokens}
              />
              {isStreaming && onPause && onResume && (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={handlePause}
                      aria-label={
                        pausePending
                          ? "暂停中…"
                          : effectivePaused
                            ? "恢复"
                            : "暂停"
                      }
                    >
                      {pausePending ? (
                        <Loader2 className="h-4 w-4 animate-spin" />
                      ) : effectivePaused ? (
                        <Play className="h-4 w-4" />
                      ) : (
                        <Pause className="h-4 w-4" />
                      )}
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent>
                    {pausePending
                      ? "暂停中…"
                      : effectivePaused
                        ? "恢复"
                        : "暂停"}
                  </TooltipContent>
                </Tooltip>
              )}
              {isStreaming ? (
                <Button
                  variant="destructive"
                  size="sm"
                  aria-label="停止"
                  onClick={handleStop}
                >
                  <Square className="h-4 w-4" />
                </Button>
              ) : (
                <Button
                  size="sm"
                  onClick={handleSend}
                  disabled={
                    (!input.trim() && pendingAttachments.length === 0) ||
                    !visible
                  }
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
        <span className="whitespace-nowrap">
          {message.noticeIcon ?? "📦"} {message.content}
        </span>
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
          {message.images && message.images.length > 0 && (
            <div className="mb-2 flex flex-wrap justify-end gap-2">
              {message.images.map((url, i) => (
                <img
                  key={i}
                  src={url}
                  alt="用户图片"
                  className="max-h-48 max-w-[240px] rounded-md border border-primary-foreground/20 object-contain"
                />
              ))}
            </div>
          )}
          {message.attachments && message.attachments.length > 0 && (
            <div className="mb-2 flex flex-wrap justify-end gap-2">
              {message.attachments.map((att, i) => (
                <span
                  key={`${att.name}-${i}`}
                  className="inline-flex max-w-[220px] items-center gap-1 rounded-md border border-primary-foreground/30 bg-primary-foreground/10 px-2 py-1 text-xs"
                >
                  <FileText className="h-3.5 w-3.5 shrink-0" />
                  <span className="truncate">{att.name}</span>
                </span>
              ))}
            </div>
          )}
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
            {tc.images && tc.images.length > 0 && (
              <div className="mt-1 flex flex-wrap gap-2">
                {tc.images.map((url, i) => (
                  <img
                    key={i}
                    src={url}
                    alt={`${tc.name} 输出图片`}
                    className="max-h-48 max-w-[240px] rounded-md border object-contain"
                  />
                ))}
              </div>
            )}
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
          tc.id === event.data.id
            ? {
                ...tc,
                result: event.data.result,
                images: event.data.images,
              }
            : tc,
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
    case "looper_state":
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
    case "truncation_retry": {
      // 本次尝试已作废并回退，实时视图里留下的残句由这条横幅解释。
      // 不删除已收到的增量 —— 权威内容由 Session 快照保证，重载即收敛。
      //
      // 横幅插在末条 assistant **之前**，不是追加到末尾：上面四个 delta
      // 分支都以「末条 assistant」为目标，追加会让重试的增量全落进横幅里。
      //
      // reason 区分两种重发：truncated（截断抬预算）与 transient
      //（限流/网络/5xx 退避重发）；缺省按截断展示（兼容旧后端）。
      const last = messages[messages.length - 1];
      if (last?.role !== "assistant") return messages;
      const isTransient = event.data.reason === "transient";
      return [
        ...messages.slice(0, -1),
        {
          role: "assistant" as const,
          content: isTransient
            ? `连接中断，正在重试（${event.data.attempt}/${event.data.limit}）`
            : `输出被截断，正在以更大的输出预算重试（${event.data.attempt}/${event.data.limit}）`,
          turnIndex: 0,
          isNotice: true,
          noticeIcon: "🔁",
        },
        last,
      ];
    }
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
  setPaused?: React.Dispatch<React.SetStateAction<boolean>>,
  onTextDelta?: () => void,
) {
  if (event.event === "text_delta") {
    onTextDelta?.();
  }

  // 暂停态只认服务端事件：POST /pause 的返回不代表已暂停（步进边界才生效）
  if (event.event === "looper_state") {
    setPaused?.(event.data.paused);
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
  return turns.flatMap((turn) => {
    const restored = turn.messages.map(
      (md: MessageData, idx: number): ChatMessage => {
        // 稳定 key：turn 位置号 + 轮内序号。分页前置插入时据此不重挂 DOM。
        const id = `turn-${turn.turn_index}-${idx}`;
        if (md.role === "user") {
          return {
            id,
            role: "user",
            content: md.content ?? "",
            turnIndex: turn.turn_index,
            images: md.images,
            attachments: md.attachments,
          };
        }
        if (md.role === "tool") {
          return {
            id,
            role: "tool",
            content: md.content ?? "",
            turnIndex: turn.turn_index,
          };
        }
        return {
          id,
          role: "assistant",
          content: md.content ?? "",
          turnIndex: turn.turn_index,
          toolCalls: md.tool_calls?.map((tc) => ({ ...tc, result: undefined })),
          reasoning: md.reasoning_content,
        };
      },
    );

    // 中断轮（取消/超时/失败后冻结入史）在两轮之间补一条居中横幅，
    // 在该轮消息之前 —— 与 compaction 归档分隔条同一渲染路径。
    if (!turn.interrupted) return restored;
    return [
      {
        id: `turn-${turn.turn_index}-notice`,
        role: "assistant" as const,
        content: `本轮被中断（${turn.interrupted_reason ?? "原因未知"}）`,
        turnIndex: turn.turn_index,
        isNotice: true,
        noticeIcon: "⏹",
      },
      ...restored,
    ];
  });
}
