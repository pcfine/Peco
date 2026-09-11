import { create } from "zustand";
import {
  getPecoSession,
  clearPecoSession,
  cancelPecoStream,
  pecoStreamUrl,
  pecoAttachUrl,
} from "@/api/peco";
import { parseSSELines, toChatSseEvent } from "@/api/stream";
import { useAuthStore } from "@/stores/authStore";
import {
  snapshotToMessages,
  reduceStreamEvent,
  isStreamTerminalEvent,
} from "@/components/chat/ChatView";
import type { ChatMessage } from "@/components/chat/ChatView";
import type { ChatSseEvent, UsageData } from "@/types/chat";

// Module-level AbortController so the SSE fetch survives component unmount.
let currentAbort: AbortController | null = null;

interface PecoChatState {
  loaded: boolean;
  loading: boolean;
  messages: ChatMessage[];
  sessionKey: number;

  // Streaming state — managed by the store so it survives route navigation.
  isStreaming: boolean;

  // Last error message from the streaming request (null when no error).
  error: string | null;

  // Current token usage for the context ring (null until the first stream event).
  usage: UsageData | null;

  load: () => Promise<void>;
  /** 获焦/切回标签页时追平会话：重拉快照 + 检测 is_running 附着。 */
  refresh: () => Promise<void>;
  clear: () => Promise<void>;

  /** Start an SSE streaming request. The async fetch runs inside the store
   *  and is NOT tied to any React component lifecycle. */
  sendMessage: (text: string, token: string) => Promise<void>;

  /** Abort the currently running SSE stream (server-side cancel + local). */
  abortStream: () => void;

  /** Clear the error state (call after displaying to user). */
  clearError: () => void;
}

/**
 * 消费一条 SSE 流（发起消息或重新附着共用）。
 *
 * 服务端任务与连接解耦：本地 fetch 断开不影响任务执行；
 * 页面刷新后 load() 检测 is_running 可重新接上。
 */
async function consumeSseStream(
  url: string,
  token: string,
  set: (partial: Partial<PecoChatState>) => void,
  get: () => PecoChatState,
): Promise<void> {
  const controller = new AbortController();
  currentAbort = controller;

  try {
    const response = await fetch(url, {
      headers: { Authorization: `Bearer ${token}` },
      signal: controller.signal,
    });

    if (!response.ok) {
      // Expired/invalid token — log out so ProtectedRoute redirects to
      // /login, rather than showing the raw JSON error in the chat box.
      if (response.status === 401) {
        useAuthStore.getState().logout();
        throw new Error("登录已过期，请重新登录");
      }

      // Surface a human-readable message from the API error body
      // ({ error, details }) instead of a raw JSON blob.
      let message = `HTTP ${response.status}`;
      try {
        const body = (await response.json()) as {
          details?: string;
          error?: string;
        };
        message = body.details || body.error || message;
      } catch {
        // Non-JSON body — fall back to the status code.
      }
      throw new Error(message);
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
          applyStreamEvent(event, set, get);
        }
      }
    }
  } catch (err: unknown) {
    if (err instanceof Error && err.name === "AbortError") return;
    // Surface network / server errors so the UI can display a toast.
    const message = err instanceof Error ? err.message : "连接中断，请重试";
    set({ error: message });
  } finally {
    set({ isStreaming: false });
    currentAbort = null;
  }
}

/**
 * 拉取会话快照并重建消息列表；服务端有进行中任务时追加占位并附着。
 *
 * load()（首载）与 refresh()（窗口获焦追平）共用。快照是唯一真相源，
 * 整表替换可自愈本窗口与其他窗口/服务端之间的任何漂移。
 * 快照不含进行中轮次，替换会丢占位里已流出的内容 — 调用方须保证
 * 本窗口不在流式中（isStreaming / currentAbort 均空闲）。
 */
async function refreshSession(
  set: (partial: Partial<PecoChatState>) => void,
  get: () => PecoChatState,
): Promise<void> {
  const snap = await getPecoSession();
  const restored = snapshotToMessages(snap.turns);
  // 有 pinned 摘要时在顶部渲染归档分隔线（hover 分隔条可看摘要正文）
  const messages: ChatMessage[] = snap.pinned_summary
    ? [
        {
          role: "assistant",
          content: "更早的对话已归档为摘要，仍在模型上下文中",
          turnIndex: 0,
          isNotice: true,
          summary: snap.pinned_summary,
        },
        ...restored,
      ]
    : restored;
  set({ messages, loaded: true });

  // 服务端有进行中的任务 → 追加 assistant 占位并重新附着。
  // 占位是必须的：reduceStreamEvent 的 delta 只在末条是 assistant 时应用，
  // 且避免新轮次文本误并入上一轮最后一条已完成的 assistant 消息。
  const token = useAuthStore.getState().token;
  if (snap.is_running && token && !get().isStreaming && !currentAbort) {
    set({
      messages: [
        ...get().messages,
        { role: "assistant", content: "", turnIndex: 0 },
      ],
      isStreaming: true,
    });
    void consumeSseStream(pecoAttachUrl(), token, set, get);
  }
}

// 焦点刷新的在途去重：visibilitychange 与 focus 可能同刻触发两次
let refreshInFlight = false;

export const usePecoChatStore = create<PecoChatState>()((set, get) => ({
  loaded: false,
  loading: false,
  messages: [],
  sessionKey: 0,
  isStreaming: false,
  error: null,
  usage: null,

  load: async () => {
    if (get().loaded) return;
    set({ loading: true, error: null });
    try {
      await refreshSession(set, get);
    } catch {
      // 快照拉取失败：静默 — 首载无历史可显示，用户发消息即触发服务端构建
    } finally {
      set({ loading: false });
    }
  },

  refresh: async () => {
    // 本窗口正在流式 = 已实时，无需追平；快照重建还会丢进行中占位内容
    if (refreshInFlight || get().isStreaming || currentAbort) return;
    const token = useAuthStore.getState().token;
    if (!token) return;
    refreshInFlight = true;
    try {
      await refreshSession(set, get);
    } catch {
      // 追平失败不打断使用 — 保留现有消息，下次获焦重试
    } finally {
      refreshInFlight = false;
    }
  },

  clear: async () => {
    // Abort any in-flight stream before clearing, and cancel the server-side
    // run first (否则 runner 会在快照删除后仍按轮边界落盘).
    currentAbort?.abort();
    currentAbort = null;
    try {
      await cancelPecoStream();
    } catch {
      // 无活跃任务时 404 — 常态，忽略
    }
    await clearPecoSession();
    set((s) => ({
      messages: [],
      loaded: false,
      isStreaming: false,
      usage: null,
      sessionKey: s.sessionKey + 1,
    }));
  },

  // ── sendMessage ──────────────────────────────────────────────────────

  sendMessage: async (text: string, token: string) => {
    const state = get();
    if (state.isStreaming || !text.trim() || !token) return;

    // Append user message + empty assistant placeholder.
    const userMsg: ChatMessage = {
      role: "user",
      content: text,
      turnIndex: 0,
    };
    const assistantMsg: ChatMessage = {
      role: "assistant",
      content: "",
      turnIndex: 0,
    };
    set({
      messages: [...state.messages, userMsg, assistantMsg],
      isStreaming: true,
      error: null,
    });

    await consumeSseStream(pecoStreamUrl(text), token, set, get);
  },

  // ── abortStream ──────────────────────────────────────────────────────

  abortStream: () => {
    // 服务端取消（fire-and-forget）：任务在途时 looper 于下个检查点收尾；
    // 本地 abort 只是断开观察连接，不再承担取消职责。
    void cancelPecoStream().catch(() => {});
    currentAbort?.abort();
    currentAbort = null;
    set({ isStreaming: false });
  },

  clearError: () => {
    set({ error: null });
  },
}));

// ── SSE Event Handler (store version) ────────────────────────────────────
//
// Thin wrapper around the shared reduceStreamEvent / isStreamTerminalEvent
// from ChatView.tsx.  Operates on Zustand's get() / set() instead of React's
// setMessages / setStreaming.

function applyStreamEvent(
  event: ChatSseEvent,
  set: (partial: Partial<PecoChatState>) => void,
  get: () => PecoChatState,
): void {
  const newMessages = reduceStreamEvent(event, get().messages);

  // 捕获 ModelUsage 事件驱动用量圆环。仅 `usage` 事件携带「当前上下文
  // 窗口用量」（input_tokens）；done/turn_complete 的 usage 是会话累计量，
  // 不适合作为圆环分母。
  let usage: UsageData | null | undefined;
  if (event.event === "usage") {
    usage = {
      input_tokens: event.data.input_tokens,
      output_tokens: event.data.output_tokens,
    };
  }

  // Eagerly clear isStreaming on terminal events so the UI flips from
  // stop→send button immediately.  The finally block in the stream consumer
  // is the safety net — it guarantees cleanup even if these events never arrive.
  if (isStreamTerminalEvent(event)) {
    set({
      messages: newMessages,
      isStreaming: false,
      ...(usage ? { usage } : {}),
    });
  } else {
    set({ messages: newMessages, ...(usage ? { usage } : {}) });
  }
}
