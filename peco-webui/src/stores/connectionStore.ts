// 两条流的连接状态放在壳级 store：dock 头部、成果页、空间台账都要显示同一枚点，
// 各自持有一份 EventSource 会让这三处对「连上了没有」给出不同答案。

import { create } from "zustand";
import type { SseState } from "@/lib/sseState";

interface ConnectionState {
  /** `/api/events` 台账事件流。 */
  events: SseState;
  /** chat stream：一轮一条，只有「生成中 / 空闲」两态。 */
  chat: SseState;
  /** 用户点「重试」时自增，驱动事件流重建连接并清零重试计数。 */
  retryToken: number;
  setEvents: (state: SseState) => void;
  setChat: (state: SseState) => void;
  retryEvents: () => void;
}

export const useConnectionStore = create<ConnectionState>((set) => ({
  events: { kind: "idle" },
  chat: { kind: "idle" },
  retryToken: 0,
  setEvents: (events) => set({ events }),
  setChat: (chat) => set({ chat }),
  retryEvents: () =>
    set((state) => ({
      events: { kind: "idle" },
      retryToken: state.retryToken + 1,
    })),
}));
