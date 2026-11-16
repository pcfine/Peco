// Peco dock 的开关与 tab。

import { create } from "zustand";
import { persist } from "zustand/middleware";

export type DockTab = "chat" | "artifacts";

interface DockState {
  open: boolean;
  tab: DockTab;
  setOpen: (open: boolean) => void;
  toggle: () => void;
  setTab: (tab: DockTab) => void;
}

export const useDockStore = create<DockState>()(
  persist(
    (set, get) => ({
      // 抽屉是覆盖层：默认打开会盖住刚进来的页面。
      open: false,
      tab: "chat",
      setOpen: (open) => set({ open }),
      toggle: () => set({ open: !get().open }),
      setTab: (tab) => set({ tab }),
    }),
    {
      name: "peco-webui:dock",
      partialize: (state) => ({ open: state.open, tab: state.tab }),
    },
  ),
);
