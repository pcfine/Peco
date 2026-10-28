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
  ErrorData,
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
  ChevronRight,
} from "lucide-react";
import { toast } from "sonner";
import { parseSSELines, toChatSseEvent } from "@/api/stream";
import { MarkdownRenderer } from "@/components/chat/MarkdownRenderer";
import { TokenUsageRing } from "@/components/chat/TokenUsageRing";
import { ToolCallCard } from "@/components/chat/ToolCallCard";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { CHEVRON, FOLD_ROW } from "@/components/chat/foldRow";
import { MESSAGE_TIME_CLASS } from "@/components/chat/messageTime";
import { UserBubble } from "@/components/chat/UserBubble";
import { BrandMark } from "@/components/common/BrandMark";
import { DEFAULT_CONTEXT_WINDOW } from "@/lib/constants";
import { settlePendingToolCalls, type ToolCallView } from "@/lib/toolCall";
import { formatDuration, formatMessageTime } from "@/lib/time";

// ── Types ─────────────────────────────────────────────────────────────────

/**
 * 附件扩展名白名单 — 与服务端分类白名单对齐。驱动 input accept 与发送前的
 * 图片能力预筛；类型是否受支持始终由服务端权威判定（不匹配回 400 可读文案）。
 * docx/pptx/xlsx 仅在服务端启用 Docling 时可摄入，关闭时服务端返回 400。
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
  "docx",
  "pptx",
  "xlsx",
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

/** 上传进度回调：0–100（服务端未给出总长度时不回调）。 */
export type UploadProgress = (percent: number) => void;

/**
 * 按扩展名分类附件；未知类型返回 null。
 *
 * 与服务端白名单对齐，用于选中文件后立即提示 —— 不必等一次必然 400 的往返。
 * docx/pptx/xlsx 在服务端关闭 Docling 时仍会被拒，由服务端错误文案兜底。
 */
function classifyAttachment(file: File): "image" | "document" | null {
  const ext = extensionOf(file.name);
  if (IMAGE_FILE_EXTS.includes(ext)) return "image";
  if (DOC_FILE_EXTS.includes(ext)) return "document";
  // 剪贴板位图可能没有扩展名：图片端点按 MIME 识别，仍走图片通路
  if (ext === "" && file.type.startsWith("image/")) return "image";
  return null;
}

/** 不支持的类型提示：带扩展名，便于用户判断该换文件还是换格式。 */
function unsupportedAttachmentMessage(file: File): string {
  const ext = extensionOf(file.name);
  return ext
    ? `不支持的文件类型：.${ext}（支持图片与 PDF/Markdown/文本/代码等文档）`
    : `无法识别文件类型：${file.name}（缺少扩展名）`;
}

/** 在途上传：完成后转成待发送附件，失败则移除并 toast。 */
interface UploadingAttachment {
  key: string;
  name: string;
  kind: "image" | "document";
  percent: number;
}

/** 待发送附件（图片与文档统一入口）。图片有渲染 url，文档只带引用 id 与原名。 */
export interface ChatAttachment {
  id: string;
  kind: "image" | "document";
  name: string;
  url?: string;
}

/** 一轮的终态：正常完成 / 失败 / 用户取消。 */
export type TurnStatus = "success" | "failed" | "cancelled";

/** 最终结论的终态与总耗时。 */
export interface TurnResult {
  status: TurnStatus;
  /** 本轮墙钟耗时（毫秒）；缺起点时间戳时缺省。 */
  durationMs?: number;
}

export interface ChatMessage {
  role: "user" | "assistant" | "agent-call";
  content: string;
  turnIndex: number;
  /**
   * 稳定标识：快照消息恒有、实时消息恒无 —— store 据此区分来源（否则实时
   * 残留会被当成已落盘的历史，刷新后与快照重复渲染）。
   * 分页前置插入时也据此保持 DOM 节点不重挂。
   */
  id?: string;
  toolCalls?: ToolCallView[];
  reasoning?: string;
  agentName?: string;
  agentTask?: string;
  callId?: string;
  /** 用户消息携带的图片（data URI）。发送当轮来自 pendingAttachments，刷新后经快照 DTO 恢复 */
  images?: string[];
  /** 用户消息携带的文档附件名（附件正文不进展示层），来自快照 DTO attachments */
  attachments?: { name: string }[];
  /** steering 并入当前轮的用户消息：本轮耗时起点须越过它，向前取真正的轮起点。 */
  steered?: boolean;
  /** 本轮终态与耗时（仅结论消息携带）。 */
  turnResult?: TurnResult;
  /** 错误提示消息（来自 SSE `error` 事件），以警示样式渲染 */
  isError?: boolean;
  /** 系统通知消息（如 SSE `context_compacted`），以居中分隔样式渲染 */
  isNotice?: boolean;
  /** 分隔条图标（notice 消息可选），缺省为归档语义的 📦 */
  noticeIcon?: string;
  /** 归档摘要正文（notice 消息可选）— hover 分隔条时展示 */
  summary?: string;
  /**
   * 气泡时间（毫秒 epoch）：快照取自 `MessageData.timestamp_ms`，实时消息取
   * 构造时刻。孤儿工具输出与中断横幅没有正文气泡，故留空；缺失即不渲染。
   */
  timestampMs?: number;
}

/** [mode="external"] 在途轮期间排队、等待下一次模型请求送达的用户输入。 */
export interface QueuedChatMessage {
  id: string;
  text: string;
}

export interface ChatViewProps {
  /** 生成 SSE 流式 URL 的函数，接收用户输入消息与附件引用 id，返回完整 URL */
  streamUrl: (message: string, fileIds: string[]) => string;
  /** 是否支持图片输入。false 时图片文件被拒（附件入口本身由 uploadFile 决定） */
  supportsImages?: boolean;
  /** 上传图片回调（既有图片通路），返回引用 id 与渲染 URL。supportsImages 时必需 */
  uploadImage?: (
    file: File,
    onProgress?: UploadProgress,
  ) => Promise<{ id: string; url: string }>;
  /** 上传附件回调（图片与文档统一端点）。提供时附件入口可用，与 supportsImages 解耦 */
  uploadFile?: (
    file: File,
    onProgress?: UploadProgress,
  ) => Promise<ChatAttachment>;
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
  /** 空态建议问题，点击即发送。 */
  suggestions?: string[];
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
  /** [mode="external"] 在途轮期间已排队、等待下一次模型请求送达的用户输入。
   *  在输入框上方以「已排队」区展示；送达后由 store 落成正式消息气泡。 */
  externalQueued?: QueuedChatMessage[];
  /**
   * [可选] 暂停当前运行中的 looper。与 `onResume` **同时提供**时才出现入口，
   * 且只在流式进行中展示（`ExternalIsStreaming ?? streaming`）。
   *
   * 暂停在边界检查点落地：请求发出前，或工具批执行前。请求在途时须等本次模型调用
   * 收尾；工具批执行中不打断，批跑完才停；无工具调用的轮次暂停被吸收。按钮状态
   * 以 SSE `looper_state` 事件为准。
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
  suggestions,
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
  externalQueued = [],
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
  /** 在途上传（带进度）；同时用于禁用附件入口与发送 */
  const [uploads, setUploads] = useState<UploadingAttachment[]>([]);
  const uploadSeqRef = useRef(0);
  const uploading = uploads.length > 0;
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
  const uploadingRef = useRef(uploading);
  uploadingRef.current = uploading;

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

    const sentAt = Date.now();
    const userMsg: ChatMessage = {
      role: "user",
      content: text,
      turnIndex: 0,
      timestampMs: sentAt,
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
      timestampMs: sentAt,
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
    // external 模式允许在途发送：消息进入排队区，由 store 经控制通道投递，
    // 在下一次模型请求或下一轮生效。internal 模式仍要求轮到 Idle 才能发。
    if (
      (!currentInput.trim() && attachments.length === 0) ||
      !tok ||
      (streaming && !isExternalMode) ||
      uploadingRef.current
    )
      return;
    setInput("");
    if (onExternalSendRef.current) {
      setPendingAttachments([]);
      onExternalSendRef.current(currentInput, attachments);
    } else {
      await sendMessage(currentInput);
    }
  }, [sendMessage, isExternalMode]);

  // ── initialQuery auto-send ──────────────────────────────────────────────

  const submitText = useCallback(
    (text: string) => {
      if (onExternalSendRef.current) {
        onExternalSendRef.current(text, []);
      } else {
        sendMessage(text);
      }
    },
    [sendMessage],
  );

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
      // 连接已断，收不到服务端终态事件
      setMessages((prev) => markTurnResult(prev, "cancelled"));
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

      // 单个文件的上传生命周期：入列 → 进度回调 → 转附件 / 失败 toast → 出列
      const uploadOne = async (
        file: File,
        kind: "image" | "document",
        run: (
          onProgress: UploadProgress,
        ) => Promise<{ id: string; url?: string }>,
        errorText: (err: unknown) => string,
      ) => {
        const key = `upload-${++uploadSeqRef.current}`;
        setUploads((prev) => [
          ...prev,
          { key, name: file.name, kind, percent: 0 },
        ]);
        try {
          const uploaded = await run((percent) =>
            setUploads((prev) =>
              prev.map((u) => (u.key === key ? { ...u, percent } : u)),
            ),
          );
          setPendingAttachments((prev) => [
            ...prev,
            { ...uploaded, kind, name: file.name },
          ]);
        } catch (err) {
          toast.error(errorText(err));
        } finally {
          setUploads((prev) => prev.filter((u) => u.key !== key));
        }
      };

      for (const file of files) {
        const kind = classifyAttachment(file);
        if (!kind) {
          // 选文件时即可判定 —— 不发请求、不留空白，直接提示原因
          toast.error(unsupportedAttachmentMessage(file));
          continue;
        }

        // 无附件端点（agent 会话页）：仅图片通路，行为与接入附件端点前一致
        if (!uploadFile) {
          if (!supportsImages || !uploadImage) return;
          if (kind !== "image") {
            toast.error(`当前对话仅支持图片附件：${file.name}`);
            continue;
          }
          await uploadOne(
            file,
            "image",
            (onProgress) => uploadImage(file, onProgress),
            () => `图片上传失败：${file.name}`,
          );
          continue;
        }

        if (kind === "image") {
          if (!supportsImages) {
            toast.error("当前模型不支持图片输入");
            continue;
          }
          // 既有图片通路（/images + uploadImage）优先，行为不变
          if (uploadImage) {
            await uploadOne(
              file,
              "image",
              (onProgress) => uploadImage(file, onProgress),
              () => `图片上传失败：${file.name}`,
            );
            continue;
          }
        }

        await uploadOne(
          file,
          "document",
          (onProgress) => uploadFile(file, onProgress),
          (err) => uploadErrorMessage(err, file.name),
        );
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
  // external 模式在途也允许输入（消息排队，下一次请求/下一轮送达）；
  // internal 模式仍要求轮到 Idle。
  const isInputDisabled = (isStreaming && !isExternalMode) || !visible;
  const queuedCount = externalQueued.length;

  // 暂态的两个解除点：服务端确认挂起，或本轮结束（按钮随门控消失）
  useEffect(() => {
    if (effectivePaused || !isStreaming) setPausePending(false);
  }, [effectivePaused, isStreaming]);

  // 暂态期间再点一次 = 撤回尚未落地的暂停；撤回后清本地暂态，
  // 否则该轮内按钮会一直停在暂态、无法再次发起暂停
  const handlePause = useCallback(() => {
    if (pausePending || effectivePaused) {
      setPausePending(false);
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
        data-slot="message-scroller"
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
          <WelcomeHero
            message={welcomeMessage}
            suggestions={suggestions}
            onSuggestion={submitText}
          />
        )}
        {displayMessages.map((msg, i) => (
          <ChatBubble key={msg.id ?? i} message={msg} />
        ))}
        <div ref={messagesEndRef} />
      </div>

      {/* Input */}
      <div className="pt-4 mt-4 border-t border-border">
        <div className="flex flex-col rounded-2xl border bg-card shadow-xs focus-within:border-primary focus-within:ring-[3px] focus-within:ring-primary/25">
          {/* 在途轮已排队、尚未被模型请求消费的输入。送达后由 store 落成正式
              气泡（`user_steer` / 轮次收尾兜底），这里只承担「没有石沉大海」的
              即时回显。 */}
          {queuedCount > 0 && (
            <div
              data-slot="queued-inputs"
              className="flex flex-col gap-1 px-3 pt-2 text-xs text-muted-foreground"
            >
              {externalQueued.map((item) => (
                <div key={item.id} className="flex items-start gap-1.5">
                  <span className="shrink-0 text-muted-foreground/70">
                    已排队
                  </span>
                  <span className="line-clamp-1 break-all">{item.text}</span>
                </div>
              ))}
              <div className="text-muted-foreground/60">
                将在下一次模型请求时送达
              </div>
            </div>
          )}

          <Textarea
            className="min-h-10 max-h-40 resize-none border-0 px-3 pt-2.5 pb-0 shadow-none focus-visible:ring-0"
            placeholder={
              isInputDisabled
                ? "加载中…"
                : isExternalMode && isStreaming
                  ? "输入消息，将在下一次模型请求时送达…"
                  : "输入消息..."
            }
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            onPaste={handlePaste}
            disabled={isInputDisabled}
            rows={1}
          />

          {/* 待发送附件预览（hover 出移除按钮）：图片缩略图 + 文档名 chip；
              在途上传以带百分比与进度条的 chip 并排展示，完成后替换为其一 */}
          {(pendingAttachments.length > 0 || uploads.length > 0) && (
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
              {uploads.map((u) => (
                <div
                  key={u.key}
                  data-slot="upload-progress"
                  title={`正在上传：${u.name}`}
                  className="relative flex items-center gap-1.5 overflow-hidden rounded-md border border-dashed bg-muted/40 py-1.5 pr-2 pl-2 text-xs"
                >
                  <Loader2
                    className="h-3.5 w-3.5 shrink-0 animate-spin"
                    aria-hidden="true"
                  />
                  <span className="max-w-[160px] truncate">{u.name}</span>
                  <span className="tabular-nums text-muted-foreground">
                    {u.percent > 0 ? `${u.percent}%` : "上传中"}
                  </span>
                  <span
                    aria-hidden="true"
                    className="absolute bottom-0 left-0 h-0.5 bg-primary transition-[width]"
                    style={{ width: `${u.percent}%` }}
                  />
                </div>
              ))}
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
                          ? "取消暂停"
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
              {isStreaming && (
                <Button
                  variant="destructive"
                  size="sm"
                  aria-label="停止"
                  onClick={handleStop}
                >
                  <Square className="h-4 w-4" />
                </Button>
              )}
              {/* external 模式在途也保留发送入口（消息排队）；internal 模式
                  流式中仍只给停止，避免新 SSE 顶替在途 run。 */}
              {(!isStreaming || isExternalMode) && (
                <Button
                  size="sm"
                  onClick={handleSend}
                  disabled={
                    (!input.trim() && pendingAttachments.length === 0) ||
                    uploading ||
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

function WelcomeHero({
  message,
  suggestions,
  onSuggestion,
}: {
  message: React.ReactNode;
  suggestions?: string[];
  onSuggestion: (text: string) => void;
}) {
  return (
    <div className="flex flex-col items-center justify-center gap-6 py-16 text-center">
      <BrandMark size={56} className="rounded-2xl" />
      <div className="max-w-md space-y-1.5 text-muted-foreground">
        {message}
      </div>
      {suggestions && suggestions.length > 0 && (
        <div className="flex max-w-lg flex-wrap justify-center gap-2">
          {suggestions.map((s) => (
            <button
              key={s}
              type="button"
              onClick={() => onSuggestion(s)}
              className="rounded-full border bg-card px-3.5 py-1.5 text-xs text-muted-foreground shadow-xs transition-colors hover:border-ring/40 hover:bg-accent hover:text-accent-foreground"
            >
              {s}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * 推理块：过程层，比工具行再淡一档 —— 无状态槽、无描边体、只有一个标签。
 *
 * 非受控折叠加模块级组件：折叠态存在 Radix 内部，组件在渲染函数里内联定义会
 * 随每次流式事件重建而丢状态（展开的推理块会在下一个 token 时自己合上）。
 */
function ReasoningFold({ reasoning }: { reasoning: string }) {
  return (
    <Collapsible data-slot="reasoning-fold">
      <CollapsibleTrigger
        data-fold-row="reasoning"
        className={FOLD_ROW}
        aria-label="推理过程"
      >
        <ChevronRight
          data-slot="tool-chevron"
          className={CHEVRON}
          aria-hidden="true"
        />
        <span
          data-fold-content
          className="flex min-w-0 flex-1 items-center gap-2"
        >
          <span
            data-slot="reasoning-label"
            className="shrink-0 truncate text-muted-foreground"
          >
            推理过程
          </span>
        </span>
      </CollapsibleTrigger>
      <CollapsibleContent
        data-kind="reasoning"
        className="mt-1 pl-[26px] pr-2 max-h-[240px] overflow-auto overscroll-contain whitespace-pre-wrap break-words text-xs text-tool-args-fg"
      >
        {reasoning}
      </CollapsibleContent>
    </Collapsible>
  );
}

const TURN_STATUS_META: Record<
  TurnStatus,
  { label: string; dotClass: string }
> = {
  success: { label: "成功", dotClass: "bg-emerald-500" },
  failed: { label: "失败", dotClass: "bg-destructive" },
  cancelled: { label: "取消", dotClass: "bg-amber-500" },
};

/** 结论气泡上方的状态行：圆点 + 状态 + 耗时。 */
function TurnStatusRow({ result }: { result: TurnResult }) {
  const meta = TURN_STATUS_META[result.status];
  const duration = formatDuration(result.durationMs);
  return (
    <div
      data-slot="turn-status"
      data-status={result.status}
      className="flex items-center gap-1.5 px-1 text-xs text-muted-foreground"
    >
      <span
        className={`size-1.5 shrink-0 rounded-full ${meta.dotClass}`}
        aria-hidden="true"
      />
      <span>{meta.label}</span>
      {duration && (
        <span className="text-muted-foreground/80">· 耗时 {duration}</span>
      )}
    </div>
  );
}

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
    return <UserBubble message={message} />;
  }

  if (message.role === "agent-call") {
    return (
      <div className="flex gap-2 items-start">
        <div className="bg-accent rounded-2xl rounded-bl-md px-4 py-2 text-sm border border-accent-foreground/10 max-w-[80%]">
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

  // 错误气泡早返回：它不受「整列全空则不渲染」的判据约束，否则空文案的
  // 错误会被整条吞掉。保留 destructive 配色以区别于正文气泡。
  if (message.isError) {
    return (
      <div className="flex gap-2 items-start">
        <div className="flex w-full min-w-0 flex-col gap-2">
          {message.turnResult && <TurnStatusRow result={message.turnResult} />}
          <div
            data-slot="assistant-bubble"
            className="bg-destructive/10 border border-destructive/30 text-destructive rounded-2xl rounded-bl-md px-4 py-2 text-sm max-w-[80%]"
          >
            <MarkdownRenderer content={message.content} />
          </div>
        </div>
      </div>
    );
  }

  // 正文走气泡，过程（推理与工具行）保持裸行。
  const toolCalls = message.toolCalls ?? [];
  // 空白正文不算内容：流式首个 token 常是 "\n"，真值判断会留下一个空外壳。
  // 终态行独立于正文 —— 切分出的占位气泡可能只有终态、没有任何内容。
  if (
    !message.reasoning &&
    toolCalls.length === 0 &&
    message.content.trim() === "" &&
    !message.turnResult
  ) {
    return null;
  }

  const hasProcess = Boolean(message.reasoning) || toolCalls.length > 0;
  const hasResult = message.content.trim() !== "";
  const resultTime = formatMessageTime(message.timestampMs);

  return (
    <div
      data-slot="assistant-turn"
      data-turn-index={message.turnIndex}
      className="flex gap-2 items-start"
    >
      <div
        data-slot="assistant-column"
        className="flex flex-col gap-2 w-full min-w-0"
      >
        {hasProcess && (
          <div
            data-slot="assistant-process"
            className="flex flex-col gap-1.5 min-w-0"
          >
            {message.reasoning && (
              <ReasoningFold reasoning={message.reasoning} />
            )}
            {toolCalls.length > 0 && (
              <ol data-slot="tool-call-group" className="flex flex-col gap-1.5">
                {toolCalls.map((tc, i) => (
                  // provider 可能复用 call id，下标参与 key 以避免重复键
                  <ToolCallCard key={`${tc.id}-${i}`} toolCall={tc} />
                ))}
              </ol>
            )}
          </div>
        )}
        {/* 状态行在结论气泡之上：圆点标终态，旁边是本轮总耗时。不依赖正文 ——
            切分出的占位气泡只有终态，正文由前一个气泡承载。 */}
        {message.turnResult && <TurnStatusRow result={message.turnResult} />}
        {hasResult && (
          <div
            data-slot="assistant-result"
            data-has-process={hasProcess ? "true" : undefined}
            className="group relative w-full rounded-2xl rounded-bl-md bg-bubble border border-border px-4 py-2 text-sm"
          >
            <MarkdownRenderer content={message.content} />
            {resultTime && (
              <time
                dateTime={resultTime.iso}
                data-slot="message-time"
                className={`${MESSAGE_TIME_CLASS} left-0`}
              >
                {resultTime.label}
              </time>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

// ── Shared SSE Event Reducer ─────────────────────────────────────────────────
//
// Pure functions: take current messages + an SSE event, return new messages.
// Exported so pecoChatStore can reuse them — keep the two copies in sync by
// making this the single source of truth for message transformation logic.

/** 最后一个 user 消息的下标；没有则 -1（此时扫描范围就是整表）。 */
function lastUserIndex(messages: ChatMessage[]): number {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    if (messages[i].role === "user") return i;
  }
  return -1;
}

/** 本轮起点：末尾向前第一条非 steering 的 user 消息的时间戳。 */
function turnStartMs(messages: ChatMessage[]): number | undefined {
  // steering 消息的时间戳是排队时刻（用户敲下那句话时本轮已在进行），
  // 拿它当起点会把耗时算短 —— 必须越过它继续向前找。
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const m = messages[i];
    if (m.role === "user" && !m.steered) return m.timestampMs;
  }
  return undefined;
}

/**
 * 按文案识别「服务端以取消收尾」—— 仅作旧服务端 / 非 SSE 路径的兜底。
 * 新服务端在 `error` 事件里带 `cancelled` 字段，那才是主判据。
 */
export function isCancellationText(message: string): boolean {
  return /^(对话已被取消|cancelled by user)/i.test(message);
}

/** 本轮是否被用户取消：优先服务端字段，缺省时回退到文案识别。 */
export function isCancelledError(data: ErrorData): boolean {
  return data.cancelled ?? isCancellationText(data.message);
}

/** 冻结原因含 cancel 即为用户取消。 */
function isCancellationReason(reason: string | undefined): boolean {
  return reason !== undefined && /cancel/i.test(reason);
}

/** 以本轮起点计算终态对象。 */
function turnResultAt(
  messages: ChatMessage[],
  status: TurnStatus,
  now: number,
): TurnResult {
  const start = turnStartMs(messages);
  return {
    status,
    durationMs: start !== undefined && now >= start ? now - start : undefined,
  };
}

/** 给本轮最后一条 assistant 宿主挂上终态。 */
export function markTurnResult(
  messages: ChatMessage[],
  status: TurnStatus,
  now: number = Date.now(),
): ChatMessage[] {
  const host = lastHostIndex(messages);
  if (host < 0) return messages;
  const turnResult = turnResultAt(messages, status, now);
  return messages.map((m, i) => (i === host ? { ...m, turnResult } : m));
}

/**
 * 向前找最近的、可作为正文/工具宿主的消息：role 为 assistant 且不是
 * notice/error。没有则 -1。
 *
 * 不能只看末条：委派类工具先发 `agent_call_start` 再发 `tool_call_start`，
 * 末条此时是 agent-call，卡片与后续增量会一起落空。
 */
function lastHostIndex(messages: ChatMessage[]): number {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const m = messages[i];
    if (m.role === "assistant" && !m.isNotice && !m.isError) return i;
  }
  return -1;
}

/**
 * Apply a single SSE event to a messages array (pure — no side effects).
 * `now` 仅在终态事件上用于计算本轮耗时。
 */
export function reduceStreamEvent(
  event: ChatSseEvent,
  messages: ChatMessage[],
  now: number = Date.now(),
): ChatMessage[] {
  switch (event.event) {
    case "text_delta": {
      const host = lastHostIndex(messages);
      if (host < 0) return messages;
      return messages.map((m, i) =>
        i === host ? { ...m, content: m.content + event.data.content } : m,
      );
    }
    case "reasoning_delta": {
      const host = lastHostIndex(messages);
      if (host < 0) return messages;
      return messages.map((m, i) =>
        i === host
          ? { ...m, reasoning: (m.reasoning ?? "") + event.data.content }
          : m,
      );
    }
    case "tool_call_start": {
      const host = lastHostIndex(messages);
      if (host < 0) return messages;
      const tc: ToolCallView = {
        id: event.data.id,
        name: event.data.name,
        arguments: event.data.arguments,
        status: "pending",
      };
      return messages.map((m, i) =>
        i === host ? { ...m, toolCalls: [...(m.toolCalls ?? []), tc] } : m,
      );
    }
    case "tool_result": {
      // 扫描范围限定在最后一个 user 之后的 assistant：provider 复用 call id 时，
      // 扫全部消息会把历史轮的同名卡片一并改写。
      const scopeStart = lastUserIndex(messages) + 1;
      let hit = false;
      const updated = messages.map((m, i) => {
        if (hit || i < scopeStart || m.role !== "assistant" || !m.toolCalls) {
          return m;
        }
        const target = m.toolCalls.findIndex((tc) => tc.id === event.data.id);
        if (target < 0) return m;
        hit = true;
        return {
          ...m,
          toolCalls: m.toolCalls.map((tc, j) =>
            j === target
              ? {
                  ...tc,
                  result: event.data.result,
                  images: event.data.images,
                  status: "done" as const,
                }
              : tc,
          ),
        };
      });
      if (hit) return updated;

      // 未命中 = 本轮的输出找不到对应调用（调用事件丢失或被截断）。孤儿输出
      // 不伪造归属，单列成一条合成宿主，避免改写别人的卡片。
      return [
        ...messages,
        {
          role: "assistant" as const,
          content: "",
          turnIndex: 0,
          toolCalls: [
            {
              id: event.data.id,
              name: event.data.name,
              arguments: "",
              result: event.data.result,
              status: "done" as const,
              orphan: true,
              images: event.data.images,
            },
          ],
        },
      ];
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
      return markTurnResult(settlePendingToolCalls(messages), "success", now);
    case "done":
      return settlePendingToolCalls(messages);
    case "usage":
    case "looper_state":
    // steering 的气泡切分由 store 负责（需要同时消费「已排队」状态）；
    // 纯 reducer 视角下消息列表不变。internal 模式收不到该事件。
    case "user_steer":
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
      // 横幅落在宿主之前，是排版取舍：残句在上、重试说明在下，读起来是「先
      // 看到作废的残句，再看到解释」；追加到末尾在阅读顺序上恰好相反。
      //
      // reason 区分两种重发：truncated（截断抬预算）与 transient
      //（限流/网络/5xx 退避重发）；缺省按截断展示（兼容旧后端）。
      const host = lastHostIndex(messages);
      if (host < 0) return messages;
      const isTransient = event.data.reason === "transient";
      return [
        ...messages.slice(0, host),
        {
          role: "assistant" as const,
          content: isTransient
            ? `连接中断，正在重试（${event.data.attempt}/${event.data.limit}）`
            : `输出被截断，正在以更大的输出预算重试（${event.data.attempt}/${event.data.limit}）`,
          turnIndex: 0,
          isNotice: true,
          noticeIcon: "🔁",
        },
        ...messages.slice(host),
      ];
    }
    case "error":
      // 失败轮以警示气泡展示；取消还原为「取消」态
      return settlePendingToolCalls([
        ...messages,
        {
          role: "assistant" as const,
          content: event.data.message,
          turnIndex: 0,
          isError: true,
          turnResult: turnResultAt(
            messages,
            isCancelledError(event.data) ? "cancelled" : "failed",
            now,
          ),
        },
      ]);
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

  // 暂停态只认服务端事件：POST /pause 的返回不代表已暂停
  //（流式在途时模型调用收尾才生效）
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

// ── Message Folding ────────────────────────────────────────────────────────
//
// 历史快照与实时流都产出同构的 ChatMessage：工具调用一律折叠进宿主的
// `toolCalls`，`role:"tool"` 不再进入渲染层。

interface ToolOutput {
  callId: string;
  content: string;
  consumed: boolean;
}

/**
 * 把一轮 `MessageData[]` 折叠成渲染就绪的消息列表。
 *
 * `tool` 消息本身不产出：它的内容被配对成对应调用的 `result`。配对按
 * `tool_call_id` 全等且未消费，与位置无关，因此乱序输出也安全。
 */
export function messagesFromTurn(turn: TurnData): ChatMessage[] {
  const messages: ChatMessage[] = [];
  const outputs: ToolOutput[] = [];

  turn.messages.forEach((md: MessageData, idx: number) => {
    // id 用原始下标：折叠不重排，分页前置插入时 DOM 节点才不重挂
    const id = `turn-${turn.turn_index}-${idx}`;
    if (md.role === "user") {
      messages.push({
        id,
        role: "user",
        content: md.content ?? "",
        turnIndex: turn.turn_index,
        images: md.images,
        attachments: md.attachments,
        timestampMs: md.timestamp_ms,
      });
      return;
    }
    if (md.role === "tool") {
      outputs.push({
        callId: md.tool_call_id ?? "",
        content: md.content ?? "",
        consumed: false,
      });
      return;
    }
    messages.push({
      id,
      role: "assistant",
      content: md.content ?? "",
      turnIndex: turn.turn_index,
      timestampMs: md.timestamp_ms,
      toolCalls: md.tool_calls?.length
        ? md.tool_calls.map((tc) => ({
            id: tc.id,
            name: tc.name,
            arguments: tc.arguments,
            status: "missing" as const,
          }))
        : undefined,
      reasoning: md.reasoning_content,
    });
  });

  const paired = messages.map((m) => {
    if (!m.toolCalls) return m;
    return {
      ...m,
      toolCalls: m.toolCalls.map((tc) => {
        const output = outputs.find((o) => !o.consumed && o.callId === tc.id);
        if (!output) return tc;
        output.consumed = true;
        return { ...tc, result: output.content, status: "done" as const };
      }),
    };
  });

  const orphans = outputs.filter((o) => !o.consumed);
  if (orphans.length === 0) return paired;

  // 孤儿输出原本挂在已被删除的 tool 消息上，落点就是那条消息本身 —— 折叠后
  // 中间位置无法复原。本轮末尾是唯一无歧义、且不伪造归属的位置。工具名无从
  // 得知（输出里没有），故统一记为「工具输出」。
  return [
    ...paired,
    {
      id: `turn-${turn.turn_index}-tool-orphans`,
      role: "assistant",
      content: "",
      turnIndex: turn.turn_index,
      toolCalls: orphans.map((o) => ({
        id: o.callId,
        name: "工具输出",
        arguments: "",
        result: o.content,
        status: "done" as const,
        orphan: true,
      })),
    },
  ];
}

/** 快照轮终态：未冻结为成功，cancel 为取消，其余失败。 */
function turnStatusOf(turn: TurnData): TurnStatus {
  if (!turn.interrupted) return "success";
  return isCancellationReason(turn.interrupted_reason) ? "cancelled" : "failed";
}

/** 快照轮耗时：本轮首末消息时间戳之差。 */
function snapshotTurnDuration(turn: TurnData): number | undefined {
  const times = turn.messages
    .map((m) => m.timestamp_ms)
    .filter((t) => Number.isFinite(t) && t > 0);
  if (times.length < 2) return undefined;
  const start = times[0];
  const end = Math.max(...times);
  return end > start ? end - start : undefined;
}

/** 给本轮最后一条带正文的 assistant 消息挂上终态。 */
function attachTurnResult(
  messages: ChatMessage[],
  status: TurnStatus,
  durationMs: number | undefined,
): ChatMessage[] {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const m = messages[i];
    if (m.role !== "assistant" || m.isNotice || m.isError) continue;
    if (m.content.trim() === "") continue;
    return messages.map((x, j) =>
      j === i ? { ...x, turnResult: { status, durationMs } } : x,
    );
  }
  return messages;
}

export function snapshotToMessages(turns: TurnData[]): ChatMessage[] {
  return turns.flatMap((turn) => {
    const restored = attachTurnResult(
      messagesFromTurn(turn),
      turnStatusOf(turn),
      snapshotTurnDuration(turn),
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
