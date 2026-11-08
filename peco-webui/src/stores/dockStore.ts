// Peco dock 的开关、tab 与「每个空间用哪个对话」。
// 对话 id 必须按空间分开存：切空间后再回来要接上原对话，而不是新开一条。

import { create } from "zustand";
import { persist } from "zustand/middleware";

export type DockTab = "chat" | "artifacts";

interface DockState {
  open: boolean;
  /** 视口 <900 时的覆盖式 Sheet：独立开关，不持久化 —— 缩窄窗口不该一上来就盖住整页。 */
  sheetOpen: boolean;
  tab: DockTab;
  /** ws_id → conversation id。 */
  conversationByWs: Record<string, string>;
  setOpen: (open: boolean) => void;
  setSheetOpen: (open: boolean) => void;
  toggle: () => void;
  setTab: (tab: DockTab) => void;
  setConversation: (wsId: string, convId: string) => void;
}

export const useDockStore = create<DockState>()(
  persist(
    (set, get) => ({
      // 首次进入：≥1440 才默认展开 —— 更窄时三列并列会把聊天列压到 200px 左右。
      open: window.matchMedia("(min-width: 1440px)").matches,
      sheetOpen: false,
      tab: "chat",
      conversationByWs: {},
      setOpen: (open) => set({ open }),
      setSheetOpen: (sheetOpen) => set({ sheetOpen }),
      toggle: () => set({ open: !get().open }),
      setTab: (tab) => set({ tab }),
      setConversation: (wsId, convId) =>
        set((state) => ({
          conversationByWs: { ...state.conversationByWs, [wsId]: convId },
        })),
    }),
    {
      name: "peco-webui:dock",
      partialize: (state) => ({
        open: state.open,
        tab: state.tab,
        conversationByWs: state.conversationByWs,
      }),
    },
  ),
);
