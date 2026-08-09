import { create } from "zustand";
import { getPecoSession, clearPecoSession, pecoStreamUrl } from "@/api/peco";
import { parseSSELines, toChatSseEvent } from "@/api/stream";
import {
  snapshotToMessages,
  reduceStreamEvent,
  isStreamTerminalEvent,
} from "@/components/chat/ChatView";
import type { ChatMessage } from "@/components/chat/ChatView";
import type { ChatSseEvent } from "@/types/chat";

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

  load: () => Promise<void>;
  clear: () => Promise<void>;

  /** Start an SSE streaming request. The async fetch runs inside the store
   *  and is NOT tied to any React component lifecycle. */
  sendMessage: (text: string, token: string) => Promise<void>;

  /** Abort the currently running SSE stream. */
  abortStream: () => void;

  /** Clear the error state (call after displaying to user). */
  clearError: () => void;
}

export const usePecoChatStore = create<PecoChatState>()((set, get) => ({
  loaded: false,
  loading: false,
  messages: [],
  sessionKey: 0,
  isStreaming: false,
  error: null,

  load: async () => {
    if (get().loaded) return;
    set({ loading: true, error: null });
    try {
      const snap = await getPecoSession();
      set({
        messages: snapshotToMessages(snap.turns),
        loaded: true,
        loading: false,
      });
    } catch {
      set({ loading: false });
    }
  },

  clear: async () => {
    // Abort any in-flight stream before clearing.
    currentAbort?.abort();
    currentAbort = null;
    await clearPecoSession();
    set((s) => ({
      messages: [],
      loaded: false,
      isStreaming: false,
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

    const controller = new AbortController();
    currentAbort = controller;

    try {
      const url = pecoStreamUrl(text);
      const response = await fetch(url, {
        headers: { Authorization: `Bearer ${token}` },
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
            applyStreamEvent(event, set, get);
          }
        }
      }
    } catch (err: unknown) {
      if (err instanceof Error && err.name === "AbortError") return;
      // Surface network / server errors so the UI can display a toast.
      const message =
        err instanceof Error ? err.message : "连接中断，请重试";
      set({ error: message });
    } finally {
      set({ isStreaming: false });
      currentAbort = null;
    }
  },

  // ── abortStream ──────────────────────────────────────────────────────

  abortStream: () => {
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

  // Eagerly clear isStreaming on terminal events so the UI flips from
  // stop→send button immediately.  The finally block in sendMessage is
  // the safety net — it guarantees cleanup even if these events never arrive.
  if (isStreamTerminalEvent(event)) {
    set({ messages: newMessages, isStreaming: false });
  } else {
    set({ messages: newMessages });
  }
}
