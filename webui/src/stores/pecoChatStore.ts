import { create } from "zustand";
import { getPecoSession, clearPecoSession } from "@/api/peco";
import { snapshotToMessages } from "@/components/chat/ChatView";
import type { ChatMessage } from "@/components/chat/ChatView";

interface PecoChatState {
  loaded: boolean;
  loading: boolean;
  messages: ChatMessage[];
  sessionKey: number;

  load: () => Promise<void>;
  clear: () => Promise<void>;
}

export const usePecoChatStore = create<PecoChatState>()((set, get) => ({
  loaded: false,
  loading: false,
  messages: [],
  sessionKey: 0,

  load: async () => {
    // 已加载过则跳过，避免重复请求
    if (get().loaded) return;
    set({ loading: true });
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
    await clearPecoSession();
    set((s) => ({
      messages: [],
      loaded: false,
      sessionKey: s.sessionKey + 1,
    }));
  },
}));
