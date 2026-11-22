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
import type {
  CancelTurnOutcome,
  ChatSseEvent,
  ErrorData,
  MessageData,
  TurnData,
  TurnSnapshotData,
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
import { frameSeq, parseSSELines, toChatSseEvent } from "@/api/stream";
import {
  REJECTED_TITLE,
  chatFailureToast,
  fromError,
  fromResponse,
  userFacingDetail,
  type ChatFailure,
} from "@/lib/chatErrors";
import { MarkdownRenderer } from "@/components/chat/MarkdownRenderer";
import { TokenUsageRing } from "@/components/chat/TokenUsageRing";
import { ToolCallCard } from "@/components/chat/ToolCallCard";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { CHEVRON, FOLD_ROW } from "@/components/chat/foldRow";
import { CHAT_MEASURE } from "@/components/chat/measure";
import { MessageMeta } from "@/components/chat/MessageMeta";
import { UserBubble } from "@/components/chat/UserBubble";
import { WakeNotice, isWakeNotice } from "@/components/chat/WakeNotice";
import { BrandMark } from "@/components/common/BrandMark";
import { DEFAULT_CONTEXT_WINDOW } from "@/lib/constants";
import { settlePendingToolCalls, type ToolCallView } from "@/lib/toolCall";
import { formatDuration, formatMessageTime } from "@/lib/time";
import { MAX_RETRIES, retryDelay } from "@/lib/sseState";

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

/**
 * 点「停止」后等 `done` 的硬兜底时长。
 *
 * 取消命令 200 只表示已投递，收尾在步进边界；10s 还没等到 `done` 说明连接已经死
 * 了，才退化成断开本地连接（服务端那一轮仍在跑，并会自己落库）。
 */
export const CANCEL_ABORT_MS = 10_000;

/**
 * 空闲时重新探测在途轮的间隔。
 *
 * 一轮也可能由定时任务、唤醒投递或另一个标签页发起；不重探的话页面会停在
 * idle，发送却撞 409。3s 与任务实时列表同一量级。
 */
export const ATTACH_IDLE_POLL_MS = 3_000;

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

/**
 * 一轮非取消的失败：气泡留在本地，等历史回来时按锚点插回去。
 *
 * 库里没有它的位置（服务端只在有正文时落库），所以锚点只能取「本轮是第几个
 * user」—— 这是失败气泡与历史消息之间唯一稳定的关系。
 */
export interface RetainedFailure {
  /** 失败轮的 user 在历史基准里的序号（0 起） */
  anchorUserIndex: number;
  /** `reduceStreamEvent` 生成的那条 isError 气泡，原样复用 */
  message: ChatMessage;
}

/** 保留上限：只涨不跌的登记表，就是一条只涨不跌的内存。 */
export const MAX_RETAINED_FAILURES = 20;

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
  /** 隐藏内置头部：页面已经有自己的页头时，聊天列不再重复一条标题栏。 */
  hideHeader?: boolean;
  /** composer 上方的插槽（如冲突提示）：只占位不进入消息流，也不影响输入。 */
  composerNotice?: React.ReactNode;
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
  /** 流式开关变化回调：dock 头部据此显示「对话流」的连接状态。 */
  onStreamingChange?: (streaming: boolean) => void;
  /**
   * [可选] 重新订阅在途轮的流地址（`GET .../stream/live`；服务端无在途轮回 204）。
   *
   * 一轮的执行归 daemon，连接只是它的出口之一：切走再回来不是重发，而是接上。
   * 不传即不启用 attach —— 既有用法（external 模式、纯本地流）行为不变。
   */
  liveStreamUrl?: () => string;
  /**
   * [可选] 历史请求是否已落定（壳里的 `history.isFetched`）。
   *
   * attach 的启动条件之一：历史请求必须先于订阅请求发出，否则快照补进来的内容
   * 与历史快照谁先落地就变成运气。缺省 false（不 attach）。
   */
  historySettled?: boolean;
  /**
   * [可选] 请求服务端取消在途轮（`POST .../cancel`）。
   *
   * 提供时「停止」不再掐本地连接 —— detach 之后掐连接不会停住服务端那一轮。
   * 返回 `"no_running_task"` 表示轮已结束（409），只补历史、不打「已取消」标。
   */
  cancelTurn?: () => Promise<CancelTurnOutcome>;
  /**
   * [可选] 在途轮注入：把输入排进当前轮的 pending 队列。提供时在途仍可输入发送。
   *
   * `"accepted"` = 已投递（下一次模型请求前并入，不是立即生效）；
   * `"no_running_task"` = 轮刚好收尾（409），这条输入改走普通发送。
   */
  steerTurn?: (
    text: string,
    imageIds: string[],
  ) => Promise<"accepted" | "no_running_task">;
  /**
   * [可选] 首帧 `turn_snapshot` 落定后回调一次。
   *
   * 用于补取历史：user 行可能晚于历史快照落库（帧序规则第 2 条）。
   */
  onAttachSettled?: () => void;
  /**
   * [可选] 一轮真正结束（收到 `done`）后回调 —— 壳在此 `void history.refetch()`。
   *
   * 时机见 `settleTurn`。
   */
  onTurnSettled?: () => void;
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
  hideHeader = false,
  composerNotice,
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
  onStreamingChange,
  liveStreamUrl,
  historySettled,
  cancelTurn,
  steerTurn,
  onAttachSettled,
  onTurnSettled,
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
  if (liveStreamUrl && historySettled === undefined && import.meta.env.DEV) {
    // 静默不 attach 是最难查的一类问题：attach 依赖「历史已落定」这个外部事实，
    // 漏传就表现为「回来以后什么都没接上」。
    console.warn("ChatView liveStreamUrl: historySettled is required");
  }
  const [messages, setMessages] = useState<ChatMessage[]>(initialMessages);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  /** internal 模式的暂停态（由本地 SSE `looper_state` 事件驱动） */
  const [paused, setPaused] = useState(false);
  /** 已点暂停、服务端尚未挂起（流式在途）时的暂态提示 */
  const [pausePending, setPausePending] = useState(false);
  /** 已投递取消命令、服务端尚未收尾：气泡标 `aria-busy`，「停止」按钮禁用 */
  const [cancelling, setCancelling] = useState(false);
  /** attach 的重连次数用尽：不再静默重试，摆一个可点的重试入口 */
  const [attachFailed, setAttachFailed] = useState(false);
  /** attach 的重跑开关（重试按钮 / 重新可见都靠它推一拍） */
  const [attachEpoch, setAttachEpoch] = useState(0);
  /** 收尾计数器：`done` 之后每次 +1，effect 里再回调 onTurnSettled（见 settleTurn） */
  const [settleSeq, setSettleSeq] = useState(0);
  /** internal 模式在途排队的输入（已投进该轮 pending，尚未并入模型请求） */
  const [internalQueued, setInternalQueuedState] = useState<
    QueuedChatMessage[]
  >([]);
  /** 在途轮是后台唤醒（没有人守着）：不给暂停入口，没人能看着它恢复 */
  const [headlessTurn, setHeadlessTurn] = useState(false);
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
  /** attach（订阅在途轮）的连接：与「本地发出去的那条流」生命周期不同，单独持有 */
  const attachAbortRef = useRef<AbortController | null>(null);
  /** 取消命令的 10s 兜底定时器 */
  const cancelTimerRef = useRef<ReturnType<typeof setTimeout> | undefined>(
    undefined,
  );
  /** 本轮收到过 `error{cancelled:true}` —— 服务端权威判据，`done` 时据此打标 */
  const cancelledRef = useRef(false);
  /**
   * 仍在本地保留的未落库失败。失败轮在库里只有 user 行，历史一回来就会把错误
   * 气泡一起抹掉；这里记着「它属于第几个 user」，等历史落地时插回原位。
   */
  const retainedFailuresRef = useRef<RetainedFailure[]>([]);
  /**
   * 本地消息列表里的 user 条数：本轮 user 的锚点是它 − 1。
   *
   * 不拿渲染后的 `messages` 现算 —— 一轮的头几帧可能早于乐观气泡的渲染就到了，
   * 那样算出来的锚点会落到上一轮。
   */
  const userCountRef = useRef(countUserMessages(initialMessages));
  /**
   * `internalQueued` 的同步副本 —— 读流循环里要在同一拍内摘掉并读回队列，
   * 而 setState 的下一拍才到。
   */
  const internalQueuedRef = useRef<QueuedChatMessage[]>([]);
  /** 排队条目的单调 id（仅用于渲染 key） */
  const queuedSeqRef = useRef(0);
  /**
   * 轮已收尾（409）而没投递成功的那条输入：等流式态真正落下来再当普通消息发。
   *
   * 此刻 `streamingRef` 仍为 true，当场 `sendMessage` 会被自己挡掉。
   */
  const steerFallbackRef = useRef<{ text: string; images: ChatAttachment[] }[]>(
    [],
  );
  const fileInputRef = useRef<HTMLInputElement>(null);
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
  // 登记保留气泡时要算本轮耗时（起点是本轮 user 的时间戳），而读流循环拿不到
  // 最新 state —— 只能读上一次提交的列表。
  const messagesRef = useRef(messages);
  messagesRef.current = messages;

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

  // attach / 取消相关回调同样走 ref：壳里传的是内联箭头，每渲染都是新引用，
  // 直接进 effect 依赖会让订阅随每次渲染重建。
  const liveStreamUrlRef = useRef(liveStreamUrl);
  liveStreamUrlRef.current = liveStreamUrl;
  const cancelTurnRef = useRef(cancelTurn);
  cancelTurnRef.current = cancelTurn;
  const steerTurnRef = useRef(steerTurn);
  steerTurnRef.current = steerTurn;
  const onAttachSettledRef = useRef(onAttachSettled);
  onAttachSettledRef.current = onAttachSettled;
  const onTurnSettledRef = useRef(onTurnSettled);
  onTurnSettledRef.current = onTurnSettled;
  const cancellingRef = useRef(cancelling);
  cancellingRef.current = cancelling;

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

  /** 在途排队队列的唯一写入口：ref 与 state 必须同拍（读流循环里同步读回）。 */
  const commitInternalQueued = useCallback((next: QueuedChatMessage[]) => {
    internalQueuedRef.current = next;
    setInternalQueuedState(next);
  }, []);

  /**
   * `user_steer`：把「已排队」的那几条落成正式气泡，并在其后开一个新宿主。
   *
   * 切分是展示需要 —— 服务端把这些输入并进了同一条 user 消息，之后的增量该落在
   * 新宿主上，旧气泡保留已产出的内容。气泡文本优先取本地原文：服务端下发的是
   * 归一后的合并口径，只在本地没有对应条目时（别的客户端注入、attach 接上的轮）
   * 才用它兜底。
   */
  const applyUserSteer = useCallback(
    (count: number, serverText: string) => {
      const queued = internalQueuedRef.current;
      const texts =
        queued.length >= count
          ? queued.slice(0, count).map((item) => item.text)
          : [serverText];
      commitInternalQueued(queued.slice(count));
      const at = Date.now();
      setMessages((prev) => [
        ...prev,
        ...texts.map((text): ChatMessage => ({
          role: "user",
          content: text,
          turnIndex: 0,
          steered: true,
          timestampMs: at,
        })),
        { role: "assistant", content: "", turnIndex: 0, timestampMs: at },
      ]);
      userCountRef.current += texts.length;
    },
    [commitInternalQueued],
  );

  /**
   * 一轮收尾：**先**关流式态，下一次提交再回调 `onTurnSettled`。
   *
   * 顺序是验收项而非实现细节 —— 壳里 refetch 回来的 `initialMessages` 依赖既有
   * 同步效应（要求 `streamingRef.current === false`）才能替换本地在途气泡；把
   * 「关流式」排在「让 refetch 落地」之后，用户看到的就是一行永远不被替换的脏气泡。
   * 用计数器 + effect 而不是直接调回调，是为了让这次提交先把「已不再是流式」渲染
   * 出去（流式开关的观察者也按同一提交的顺序收到 false）。
   */
  const settleTurn = useCallback(() => {
    setMessages((prev) => {
      const settled = settlePendingToolCalls(prev);
      // 「已取消」只在服务端说了算（`error{cancelled:true}`）时才打标，
      // 且延到 `done` 这一拍 —— 提前打会把还在收尾的轮说成已经停下。
      return cancelledRef.current
        ? markTurnResult(settled, "cancelled")
        : settled;
    });
    setStreaming(false);
    setPaused(false);
    setCancelling(false);
    setHeadlessTurn(false);
    // 队列在这里清空：这些输入要么已经由 `user_steer` 变成气泡，要么已被服务端
    // 当下一轮输入消费掉（`done` 之后的历史 refetch 会补回来）。重发就是重复。
    commitInternalQueued([]);
    if (cancelTimerRef.current) {
      clearTimeout(cancelTimerRef.current);
      cancelTimerRef.current = undefined;
    }
    setSettleSeq((n) => n + 1);
  }, [commitInternalQueued]);

  /** 登记一条未落库的失败：气泡本体沿用 `reduceStreamEvent` 的那一份。 */
  const retainFailure = useCallback((data: ErrorData) => {
    retainedFailuresRef.current = retainFailureIn(retainedFailuresRef.current, {
      anchorUserIndex: userCountRef.current - 1,
      message: errorBubble(data, messagesRef.current, Date.now()),
    });
  }, []);

  // ── Core send logic — stable identity (empty deps, reads everything from refs) ─

  const sendMessage = useCallback(
    async (text: string) => {
      const attachments = pendingAttachmentsRef.current;
      if ((!text.trim() && attachments.length === 0) || streamingRef.current)
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
      userCountRef.current += 1;
      setStreaming(true);
      setPaused(false);
      setCancelling(false);
      cancelledRef.current = false;
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
      // 本地流接手出口：停掉空闲重探/在途订阅，避免两条流抢同一轮。
      attachAbortRef.current?.abort();
      attachAbortRef.current = null;

      /**
       * 撤掉刚乐观插进去的两条气泡，输入与附件原样还给用户。
       *
       * 只在「服务端那一轮根本没跑起来」时调用：帧一旦进来，撤气泡会让人以为
       * 消息没发出去，而它其实已经跑了。
       */
      const restoreOptimistic = () => {
        setMessages((prev) =>
          prev.filter((m) => m !== userMsg && m !== assistantMsg),
        );
        // 乐观插入的那条 user 撤了，锚点跟着退回去，否则下一轮会算错位置
        userCountRef.current -= 1;
        setInput(text);
        setPendingAttachments(attachments);
      };

      const reportFailure = (failure: ChatFailure) => {
        const line = chatFailureToast(failure);
        if (!line) return;
        if (line.description) {
          toast.error(line.title, { description: line.description });
        } else {
          toast.error(line.title);
        }
      };

      /** `fetch` 是否已 resolve：false 才可能确实是「连不上」。 */
      let responded = false;
      /** 本轮已应用的帧数：0 = 服务端那轮没跑起来，回滚才是对的。 */
      let framesApplied = 0;

      try {
        const url = streamUrlRef.current(text, fileIds);
        const response = await fetch(url, { signal: controller.signal });
        responded = true;
        if (!response.ok) {
          const errText = await response.text().catch(() => "");
          // 观测点即分类点：回了响应就一定不是「连不上」，别再落进 catch。
          const failure = fromResponse(response.status, errText);
          restoreOptimistic();
          // 409 conversation_busy：服务端有一轮在跑而本页没接上（定时/唤醒/
          // 另一个标签页起的轮都不通知本页）。改为 steering 投递，而不是让用户先停止。
          if (
            failure.kind === "server" &&
            failure.code === "conversation_busy" &&
            steerTurnRef.current
          ) {
            const images = attachments.filter((a) => a.kind === "image");
            try {
              const outcome = await steerTurnRef.current(
                text,
                images.map((a) => a.id),
              );
              if (outcome === "accepted") {
                setInput("");
                setPendingAttachments(
                  attachments.filter((a) => a.kind !== "image"),
                );
                queuedSeqRef.current += 1;
                commitInternalQueued([
                  ...internalQueuedRef.current,
                  { id: `steer-${queuedSeqRef.current}`, text },
                ]);
                return;
              }
            } catch {
              // 投递失败退回普通失败提示；输入已由 restoreOptimistic 还给用户。
            }
          }
          reportFailure(failure);
          return;
        }
        const reader = response.body!.getReader();
        const decoder = new TextDecoder();
        let buffer = "";
        let pendingEvent = "";
        let pendingId = "";
        /** 帧序水位：首帧 `turn_snapshot` 覆盖到的最后一帧；`id <= 水位` 的帧丢弃 */
        let watermark = -1;

        while (true) {
          const { done, value } = await reader.read();
          if (done) break;

          const chunk = decoder.decode(value, { stream: true });
          const parsedChunk = parseSSELines(
            chunk,
            buffer,
            pendingEvent,
            pendingId,
          );
          buffer = parsedChunk.remaining;
          pendingEvent = parsedChunk.pendingEvent;
          pendingId = parsedChunk.pendingId;

          for (const parsed of parsedChunk.events) {
            const event = toChatSseEvent(parsed);
            if (!event) continue;

            if (event.event === "turn_snapshot") {
              // 先记水位再应用（帧序规则第 2 条）：快照之后的重叠帧一律丢弃
              watermark = event.data.seq;
              setUsage(event.data.usage);
              setHeadlessTurn(event.data.audience === "headless");
              // 快照早于下面的水位过滤，单独计一帧
              framesApplied += 1;
            } else {
              const seq = frameSeq(parsed.id);
              if (seq !== null && seq <= watermark) continue;
              framesApplied += 1;
            }

            if (event.event === "usage") {
              setUsage({
                input_tokens: event.data.input_tokens,
                output_tokens: event.data.output_tokens,
              });
            }
            if (event.event === "error") {
              if (isCancelledError(event.data)) {
                cancelledRef.current = true;
              } else {
                // 这一轮多半不会落库：先把气泡留一份，历史回来时按锚点插回
                retainFailure(event.data);
              }
            }
            if (event.event === "done") {
              // `done` 是唯一的「流结束」信号：关流式态 → 下一拍 refetch 以库里为准
              settleTurn();
              continue;
            }
            if (event.event === "user_steer") {
              applyUserSteer(event.data.count, event.data.text);
              continue;
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
      } catch (err: unknown) {
        const failure = fromError(err, responded);
        // 用户自己按的停止：界面上的这一轮由停止流程收尾，不提示也不回滚
        if (failure.kind === "aborted") return;
        // 帧已进来 ⇒ 服务端那一轮可能已经跑了，撤气泡会让人以为没发出去
        if (failure.kind === "stream" && framesApplied > 0) {
          reportFailure(failure);
          return;
        }
        restoreOptimistic();
        if (failure.kind === "stream") {
          // 有响应却一帧未出：服务端那轮没跑起来，与「拒绝」同档
          const detail = userFacingDetail(failure.detail);
          if (detail) toast.error(REJECTED_TITLE, { description: detail });
          else toast.error(REJECTED_TITLE);
          return;
        }
        reportFailure(failure);
      } finally {
        // 兜底：若这一轮没等到 `done`（连接断在半路），流式态仍要关掉 —— 服务端
        // 那一轮有没有跑完由下一次 attach（204/快照）交代，本地只负责不再假装在生成。
        setStreaming(false);
        setPaused(false);
        setCancelling(false);
        abortRef.current = null;
        // 本地收尾后重订阅：下一轮（唤醒/定时）必须能被这个页面看见。
        setAttachEpoch((n) => n + 1);
      }
    },
    [settleTurn, retainFailure, applyUserSteer, commitInternalQueued],
  );

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
      const merged = mergeRetainedFailures(
        initialMessages,
        retainedFailuresRef.current,
      );
      // 回写剪枝后的登记表：真实回复已落库的那些失败不该拖到下一次 refetch
      retainedFailuresRef.current = merged.retained;
      userCountRef.current = countUserMessages(initialMessages);
      setMessages(merged.messages);
    }
  }, [initialMessages, isExternalMode]);

  // ── 在途轮注入（steering）────────────────────────────────────────────
  //
  // external 模式那一半的对称：轮在跑也能说话，输入进该轮的 pending 队列，
  // 下一次模型请求前并入，而不是另开一条流抢在途 run。

  const steerInFlight = useCallback(
    async (text: string) => {
      const steer = steerTurnRef.current;
      if (!steer) return;
      const attachments = pendingAttachmentsRef.current;
      const images = attachments.filter((att) => att.kind === "image");
      setInput("");
      // 图片随这条一起进 pending；文档没有对应的注入通路，留在输入区等下一次
      // 普通发送 —— 静默丢掉用户的附件比留在原地更难解释。
      setPendingAttachments(attachments.filter((att) => att.kind !== "image"));
      try {
        const outcome = await steer(
          text,
          images.map((att) => att.id),
        );
        if (outcome === "accepted") {
          queuedSeqRef.current += 1;
          commitInternalQueued([
            ...internalQueuedRef.current,
            { id: `steer-${queuedSeqRef.current}`, text },
          ]);
          return;
        }
        // 轮刚好收尾：这条还没投递，先存着（此刻 streamingRef 仍为 true，
        // 当场发会被自己挡掉），等流式态落下来再当普通消息发。
        steerFallbackRef.current = [
          ...steerFallbackRef.current,
          { text, images },
        ];
      } catch {
        setInput(text);
        setPendingAttachments(attachments);
        toast.error("发送失败，请重试");
      }
    },
    [commitInternalQueued],
  );

  // ── handleSend: reads from input → clears input → delegates to sendMessage ─

  const handleSend = useCallback(async () => {
    const currentInput = inputRef.current;
    const streaming = externalIsStreamingRef.current ?? streamingRef.current;
    const attachments = pendingAttachmentsRef.current;
    // external 模式允许在途发送：消息进入排队区，由 store 经控制通道投递，
    // 在下一次模型请求或下一轮生效。internal 模式只有接上了 steerTurn 才有
    // 同样的排队通路，否则仍要求轮到 Idle 才能发。
    const steering = !isExternalMode && streaming && !!steerTurnRef.current;
    if (
      (!currentInput.trim() && attachments.length === 0) ||
      (streaming && !isExternalMode && !steering) ||
      uploadingRef.current
    )
      return;
    if (steering) {
      await steerInFlight(currentInput);
      return;
    }
    setInput("");
    if (onExternalSendRef.current) {
      setPendingAttachments([]);
      onExternalSendRef.current(currentInput, attachments);
    } else {
      await sendMessage(currentInput);
    }
  }, [sendMessage, isExternalMode, steerInFlight]);

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
    if (initialQuery && !hasSentInitialRef.current) {
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
      // 取消的 10s 兜底定时器不能活过组件：到点再 setState 已无处安放。
      if (cancelTimerRef.current) clearTimeout(cancelTimerRef.current);
      // In external mode the SSE lifecycle is managed by the store —
      // do NOT abort on unmount so streaming continues in the background.
      if (isExternalMode) return;
      unmountTimerRef.current = setTimeout(() => {
        abortRef.current?.abort();
      }, 0);
    };
  }, [isExternalMode]);

  // ── 重新订阅在途轮（attach）────────────────────────────────────────────
  //
  // 一轮的执行归 daemon，SSE 连接只是它的出口之一，所以「切走再回来」不是重发
  // 而是重新订阅：无在途轮 → 204（历史即权威，什么都不做）；有在途轮 → 首帧
  // `turn_snapshot` 把已产出的内容原样交回，我们按 `seq` 记水位、按 `id` 丢弃重叠
  // 帧后继续跟增量。断开只是关掉出口，服务端那一轮不受影响。
  const canAttachTurn =
    !isExternalMode && liveStreamUrl !== undefined && historySettled === true;

  useEffect(() => {
    // 不可见时不握连接：切回可见时本 effect 会重跑，届时重新订阅并补齐
    if (!canAttachTurn || !visible) return;
    // 本地正在发的那条流本身就是出口，不要再叠一条订阅
    if (abortRef.current) return;
    const buildUrl = liveStreamUrlRef.current;
    if (!buildUrl) return;

    // 地址只解析一次：重试要重走的是**同一条订阅**，不是重新拼一串参数。
    // 拼不出来（接线漏了）就别开订阅，更不要拿它当连接失败去重试。
    let url: string;
    try {
      url = buildUrl();
    } catch (err) {
      if (import.meta.env.DEV) {
        console.warn("ChatView liveStreamUrl: 取流地址失败，跳过订阅", err);
      }
      return;
    }

    const controller = new AbortController();
    attachAbortRef.current = controller;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    /** 退避重连（复用 sseState 的 1s→…→15s，MAX_RETRIES 次） */
    const retry = (attempt: number) => {
      if (disposed) return;
      if (attempt >= MAX_RETRIES) {
        setAttachFailed(true);
        return;
      }
      timer = setTimeout(
        () => {
          void run(attempt + 1);
        },
        retryDelay(attempt + 1),
      );
    };

    /** 本页拿到的 204 只说明「那一刻空闲」，不是「以后都空闲」。 */
    const scheduleIdlePoll = (attempt: number) => {
      if (disposed || abortRef.current) return;
      timer = setTimeout(() => {
        void run(attempt);
      }, ATTACH_IDLE_POLL_MS);
    };

    const run = async (attempt: number): Promise<void> => {
      // 本地发送的流就是出口：让路，别再叠一条订阅。
      if (abortRef.current) return;
      let watermark = -1;
      let sawDone = false;
      try {
        const response = await fetch(url, { signal: controller.signal });
        if (disposed) return;
        if (response.status === 204) {
          // 无在途轮：历史即权威。顺手把可能残留的流式态收干净（隐藏期间这一轮
          // 跑完时我们不在，没人替我们关它）。
          if (abortRef.current) return;
          setStreaming(false);
          setCancelling(false);
          setAttachFailed(false);
          scheduleIdlePoll(attempt);
          return;
        }
        if (!response.ok || !response.body) {
          throw new Error(`HTTP ${response.status}`);
        }

        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        let buffer = "";
        let pendingEvent = "";
        let pendingId = "";

        while (true) {
          const { done, value } = await reader.read();
          if (done) break;

          const chunk = decoder.decode(value, { stream: true });
          const parsedChunk = parseSSELines(
            chunk,
            buffer,
            pendingEvent,
            pendingId,
          );
          buffer = parsedChunk.remaining;
          pendingEvent = parsedChunk.pendingEvent;
          pendingId = parsedChunk.pendingId;

          for (const parsed of parsedChunk.events) {
            const event = toChatSseEvent(parsed);
            if (!event) continue;

            if (event.event === "turn_snapshot") {
              // 规则 2：先记水位，再**替换**在途气泡（替换即气泡级去重）；
              // 服务端因 Lagged 补发新快照时也走这里，按同一口径重置。
              watermark = event.data.seq;
              setMessages((prev) => applyTurnSnapshot(prev, event.data));
              setUsage(event.data.usage);
              setStreaming(true);
              setPaused(event.data.phase === "paused");
              setHeadlessTurn(event.data.audience === "headless");
              setCancelling(event.data.phase === "cancelling");
              // 规则 2 末句：快照落定后补取一次历史，补齐可能晚于历史快照
              // 落库的 user 行（窗口 W4）
              onAttachSettledRef.current?.();
              continue;
            }

            // 规则 3：`id <= 水位` 的帧已被快照覆盖，丢弃
            const seq = frameSeq(parsed.id);
            if (seq !== null && seq <= watermark) continue;

            if (event.event === "error") {
              if (isCancelledError(event.data)) {
                cancelledRef.current = true;
              } else {
                // 水位已拦掉重叠帧：同一帧不会登记第二次
                retainFailure(event.data);
              }
            }
            if (event.event === "done") {
              sawDone = true;
              settleTurn();
              continue;
            }
            // 接上的这一轮也可能被注入：排队的输入要在这里落成气泡，否则只有
            // 等 `done` 之后的历史 refetch 才看得见它。
            if (event.event === "user_steer") {
              applyUserSteer(event.data.count, event.data.text);
              continue;
            }
            handleSSEEvent(event, setMessages, setStreaming, setPaused);
          }
        }

        // 规则 7：没有 `done` 而流结束 = 连接失败，不得自行判定轮已结束
        if (!sawDone) {
          retry(attempt);
          return;
        }
        // 正常收尾后继续空闲重探，等下一次服务端侧的轮。
        scheduleIdlePoll(attempt);
      } catch {
        if (disposed || controller.signal.aborted) return;
        retry(attempt);
      }
    };

    setAttachFailed(false);
    void run(0);

    return () => {
      disposed = true;
      if (timer) clearTimeout(timer);
      // 只是关掉出口：服务端那一轮由 daemon 持有，回来时重新订阅即可
      controller.abort();
      if (attachAbortRef.current === controller) attachAbortRef.current = null;
    };
  }, [
    canAttachTurn,
    visible,
    attachEpoch,
    settleTurn,
    retainFailure,
    applyUserSteer,
  ]);

  // ── Handlers ────────────────────────────────────────────────────────────

  /**
   * 停止 = 请服务端取消这一轮，而不是掐掉自己的连接。
   *
   * detach 之后连接不再是所有权：掐它停不住服务端那一轮，用户看到的只是「按了
   * 没反应」。200 只表示命令已投递（收尾在步进边界），所以守着连接等
   * `error{cancelled:true}` + `done`；10s 还没等到才退化为本地 abort —— 硬兜底，
   * 只在连接已经死掉时触发，服务端那一轮仍会自己跑完并落库。
   */
  const handleStop = () => {
    if (onExternalStopRef.current) {
      onExternalStopRef.current();
      return;
    }
    const requestCancel = cancelTurnRef.current;
    if (!requestCancel) {
      // 没有取消能力（旧后端 / 未接线）：保持旧行为 —— 断流 + 本地打标
      abortRef.current?.abort();
      // 连接已断，收不到服务端终态事件
      setMessages((prev) => markTurnResult(prev, "cancelled"));
      setStreaming(false);
      return;
    }
    if (cancellingRef.current) return;
    setCancelling(true);
    void requestCancel().then(
      (outcome) => {
        if (outcome === "no_running_task") {
          // 轮已经结束：历史即权威，补一次历史就够了，不打「已取消」标
          setCancelling(false);
          setSettleSeq((n) => n + 1);
          return;
        }
        // 命令已投递：连接保持打开，等 error{cancelled:true} + done 收尾
        cancelTimerRef.current = setTimeout(() => {
          cancelTimerRef.current = undefined;
          abortRef.current?.abort();
          attachAbortRef.current?.abort();
          setStreaming(false);
          setCancelling(false);
          setSettleSeq((n) => n + 1);
        }, CANCEL_ABORT_MS);
      },
      () => {
        setCancelling(false);
        toast.error("取消失败，请重试");
      },
    );
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
  // 只在开关变化时回调，不每帧：父级是壳级 store，多写一次就多一次全站重渲染。
  const onStreamingChangeRef = useRef(onStreamingChange);
  onStreamingChangeRef.current = onStreamingChange;
  useEffect(() => {
    onStreamingChangeRef.current?.(isStreaming);
  }, [isStreaming]);

  // 一轮收尾的回调必须落在「流式已关」之后的那次提交里 —— 本 effect 因此**排在上
  // 面那个之后**（同一提交内 effect 按声明顺序执行）。
  useEffect(() => {
    if (settleSeq === 0) return;
    onTurnSettledRef.current?.();
  }, [settleSeq]);

  // 被 409 挡回来的输入等这一轮真正落下来再发。一次只放一条：发出去的那条自己
  // 会把流式态重新拉起来，本 effect 因此不会连着放完。
  useEffect(() => {
    if (isStreaming) return;
    const next = steerFallbackRef.current.shift();
    if (!next) return;
    // `sendMessage` 从 ref 读附件：先把这条的图片摆回去，flush 出来的才是原样
    pendingAttachmentsRef.current = next.images;
    setPendingAttachments(next.images);
    void sendMessage(next.text);
  }, [isStreaming, sendMessage]);

  // external 模式的 SSE 由 store 管理，本地循环收不到 `looper_state`，
  // 故以父组件传入的 externalPaused 为权威；internal 模式仍认本地事件。
  const effectivePaused = isExternalMode ? externalPaused : paused;
  // 在途仍可输入的两条通路：external（store 投递）与 internal 接了 steerTurn
  //（本组件自己把输入注进该轮队列）。都没有时仍要求轮到 Idle。
  const isInputDisabled =
    (isStreaming && !isExternalMode && !steerTurn) || !visible;
  const queued = isExternalMode ? externalQueued : internalQueued;

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
      className={`${CHAT_MEASURE} flex min-h-0 flex-col ${className || "h-full"}`}
    >
      {/* Header */}
      {!hideHeader && (
        <div className="flex items-center gap-3 py-3 border-b mb-4">
          <h2 className="font-semibold flex-1">{headerTitle}</h2>
          {headerActions}
        </div>
      )}

      {/* Messages */}
      <div
        ref={scrollContainerRef}
        onScroll={handleScroll}
        data-slot="message-scroller"
        // 取消已投递、收尾尚未发生：在途气泡仍在被服务端改写
        aria-busy={cancelling ? "true" : undefined}
        className="flex flex-1 flex-col overflow-y-auto pr-2"
      >
        {/* my-auto 而非 justify-center：flex 列溢出时自动外边距归零，首条仍能滚到顶 */}
        <div data-slot="message-list" className="my-auto w-full space-y-4">
          {onLoadEarlier && hasMore && (
            <div className="flex justify-center">
              <Button
                variant="ghost"
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
        </div>
      </div>

      {/* 订阅在途轮的重试入口：重连次数用尽后停在这里，不后台空转 */}
      {attachFailed && (
        <div
          data-slot="attach-retry"
          role="status"
          className="mt-2 flex items-center justify-between gap-2 rounded-md border border-warn/40 bg-warn/10 px-3 py-1.5 text-caption text-ink-muted"
        >
          <span>没能接上这一轮，连接已断开。</span>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => {
              setAttachFailed(false);
              setAttachEpoch((n) => n + 1);
            }}
          >
            重试
          </Button>
        </div>
      )}

      {/* Input */}
      <div className="pt-4 mt-4">
        {composerNotice}
        <div className="flex flex-col rounded-2xl border bg-card focus-within:border-primary focus-within:ring-[3px] focus-within:ring-primary/25">
          {/* 在途轮已排队、尚未被模型请求消费的输入。送达后落成正式气泡
              （`user_steer` / 轮次收尾兜底），这里只承担「没有石沉大海」的
              即时回显。external 的队列归 store，internal 的归本组件。 */}
          {queued.length > 0 && (
            <div
              data-slot="queued-inputs"
              className="flex flex-col gap-1 px-3 pt-2 text-xs text-muted-foreground"
            >
              {queued.map((item) => (
                <div key={item.id} className="flex items-start gap-1.5">
                  <span className="shrink-0 text-muted-foreground/70">
                    已排队
                  </span>
                  <span className="line-clamp-1 min-w-0 break-all">
                    {item.text}
                  </span>
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
                : isStreaming && (isExternalMode || !!steerTurn)
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
              {/* 唤醒轮没有人守着：摆一个暂停入口，停住的后台轮就没人来恢复。 */}
              {isStreaming && onPause && onResume && !headlessTurn && (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button
                      variant="ghost"
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
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button
                      variant="destructive"
                      size="sm"
                      aria-label={cancelling ? "正在取消" : "停止"}
                      aria-busy={cancelling ? "true" : undefined}
                      disabled={cancelling}
                      onClick={handleStop}
                    >
                      <Square className="h-4 w-4" />
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent>
                    {cancelling ? "正在取消，等这一轮收尾…" : "停止"}
                  </TooltipContent>
                </Tooltip>
              )}
              {/* 在途也保留发送入口的两条通路：external（store 排队）与 internal
                  接了 steerTurn（注入当前轮队列）。都没有时流式中只给停止，
                  避免新 SSE 顶替在途 run。 */}
              {(!isStreaming || isExternalMode || !!steerTurn) && (
                <Button
                  size="sm"
                  aria-label="发送"
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
              className="rounded-full bg-line/60 px-3.5 py-1.5 text-xs text-muted-foreground transition-colors hover:bg-line hover:text-ink"
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

  // 唤醒投递也走 user 角色，但它不是用户说的话：折叠成一行通知，不占气泡。
  if (message.role === "user" && isWakeNotice(message.content)) {
    return (
      <WakeNotice content={message.content} timestampMs={message.timestampMs} />
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
            <MessageMeta
              align="start"
              time={resultTime}
              copyText={message.content}
            />
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

/**
 * `error` 帧的气泡本体。
 *
 * 未落库的失败要跨一次 refetch 留着，所以构造口径必须只有一处 —— 读流时现成
 * 生成一份、保留时再造一份，两边就会长成两种样子。
 */
function errorBubble(
  data: ErrorData,
  messages: ChatMessage[],
  now: number,
): ChatMessage {
  return {
    role: "assistant",
    content: data.message,
    turnIndex: 0,
    isError: true,
    turnResult: turnResultAt(
      messages,
      isCancelledError(data) ? "cancelled" : "failed",
      now,
    ),
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
 * 在途气泡的下标：本轮 SSE 造出来的 assistant 宿主。
 *
 * 与 `lastHostIndex` 的区别是「不带 id」—— 历史消息（快照/分页）恒有 id，实时
 * 消息恒无（见 `ChatMessage.id` 的注释）。attach 时若拿历史里的回答当宿主，快照
 * 就会把上一轮的回答覆盖成本轮内容。
 */
function lastLiveHostIndex(messages: ChatMessage[]): number {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const m = messages[i];
    if (
      m.role === "assistant" &&
      m.id === undefined &&
      !m.isNotice &&
      !m.isError
    )
      return i;
  }
  return -1;
}

/**
 * 首帧 `turn_snapshot` → 在途气泡（帧序规则第 2 条：**替换**而非追加）。
 *
 * 快照是在途内容的显示权威（DB 只在 `done` 之后接手）：正文/推理/工具卡一律以
 * 它为准，本轮已产出的子 Agent 卡按快照重建。没有在途气泡时新建一个 —— 这是
 * 「切走再回来」与「刷新后接上」的共同入口。
 */
export function applyTurnSnapshot(
  messages: ChatMessage[],
  snapshot: TurnSnapshotData,
): ChatMessage[] {
  // 子 Agent 卡整体由快照重建：本地那些无 id 的同轮卡片一并作废
  const base = messages.filter(
    (m) => m.role !== "agent-call" || m.id !== undefined,
  );
  const toolCalls: ToolCallView[] | undefined = snapshot.tool_calls.length
    ? snapshot.tool_calls.map((tc) => ({
        id: tc.id,
        name: tc.name,
        arguments: tc.arguments,
        result: tc.result,
        status: tc.status === "done" ? ("done" as const) : ("pending" as const),
      }))
    : undefined;
  const reasoning = snapshot.reasoning === "" ? undefined : snapshot.reasoning;
  const agents: ChatMessage[] = snapshot.agent_calls.map((call) => ({
    role: "agent-call" as const,
    content: call.status === "done" ? call.result || "(完成)" : "",
    turnIndex: 0,
    agentName: call.agent_name,
    // 快照不带子 Agent 的任务描述（阶段一按 id/name/status 重建卡片即可）
    agentTask: "",
    callId: call.call_id,
  }));

  const host = lastLiveHostIndex(base);
  if (host < 0) {
    return [
      ...base,
      {
        role: "assistant" as const,
        content: snapshot.text,
        turnIndex: 0,
        reasoning,
        toolCalls,
        timestampMs: Date.now(),
      },
      ...agents,
    ];
  }
  return [
    ...base.slice(0, host),
    { ...base[host], content: snapshot.text, reasoning, toolCalls },
    ...agents,
    ...base.slice(host + 1),
  ];
}

/** 列表里 user 消息的条数 —— 也就是下一个 user 的锚点。 */
function countUserMessages(messages: ChatMessage[]): number {
  let count = 0;
  for (const m of messages) {
    if (m.role === "user") count += 1;
  }
  return count;
}

/** 登记一条失败；超出上限丢最旧的（新的失败比旧的一直更有信息量）。 */
export function retainFailureIn(
  retained: RetainedFailure[],
  entry: RetainedFailure,
): RetainedFailure[] {
  return [...retained, entry].slice(-MAX_RETAINED_FAILURES);
}

/**
 * 把未落库的失败气泡合进历史基准（纯函数）。
 *
 * 失败轮在库里只剩 user 行，直接拿历史整表覆盖会把错误抹掉（用户只看到红框闪
 * 一下）。合并而不是追加：每次都从基准重建，多次 refetch 不会叠出第二条同样的
 * 气泡。这一轮已经有 assistant 落库时丢弃登记 —— 真实回复才是最终结论，错误只是
 * 失败尝试的回声。
 */
export function mergeRetainedFailures(
  base: ChatMessage[],
  retained: RetainedFailure[],
): { messages: ChatMessage[]; retained: RetainedFailure[] } {
  if (retained.length === 0) return { messages: base, retained };

  const userIndices: number[] = [];
  base.forEach((m, i) => {
    if (m.role === "user") userIndices.push(i);
  });

  /** 插入位置（基准下标，插在该下标之前）→ 该位置要插入的气泡 */
  const insertions = new Map<number, ChatMessage[]>();
  const kept: RetainedFailure[] = [];

  for (const item of retained) {
    const blockStart = userIndices[item.anchorUserIndex];
    // 一轮块的末尾 = 下一个 user 之前；没有下一个 user 就是列表末尾
    const blockEnd =
      blockStart === undefined
        ? base.length
        : (userIndices[item.anchorUserIndex + 1] ?? base.length);
    if (blockStart !== undefined) {
      let replied = false;
      for (let i = blockStart + 1; i < blockEnd; i += 1) {
        if (base[i].role === "assistant") {
          replied = true;
          break;
        }
      }
      if (replied) continue;
    }
    // 锚点越界 = 基准还没含这一轮（历史尚未追上）：错误仍要说出来，先追加到
    // 末尾；下一次 refetch 若追上，锚点会重新把它归位。
    kept.push(item);
    const at = insertions.get(blockEnd) ?? [];
    at.push(item.message);
    insertions.set(blockEnd, at);
  }

  if (kept.length === 0) return { messages: base, retained: kept };

  const messages: ChatMessage[] = [];
  base.forEach((m, i) => {
    const at = insertions.get(i);
    if (at) messages.push(...at);
    messages.push(m);
  });
  const tail = insertions.get(base.length);
  if (tail) messages.push(...tail);
  return { messages, retained: kept };
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
    case "turn_snapshot":
      // 在途轮快照：水位与流式态由读循环负责，这里只管气泡内容（替换口径）
      return applyTurnSnapshot(messages, event.data);
    case "usage":
    case "looper_state":
    // steering 的气泡切分由消费方负责（store / 读流循环）：它要同时消费
    // 「已排队」状态并另起一个宿主。纯 reducer 视角下消息列表不变。
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
        errorBubble(event.data, messages, now),
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
