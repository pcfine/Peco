// 壳的折叠态：rail 宽度与 dock 开关都只是 UI 态，不进 query 缓存。

import { create } from "zustand";
import { persist } from "zustand/middleware";

interface UiState {
  railOpen: boolean;
  setRailOpen: (open: boolean) => void;
}

export const useUiStore = create<UiState>()(
  persist(
    (set) => ({
      // 默认按视口给初值：≥1100 展开，窄屏先折叠，用户手动切换后长期生效。
      railOpen: window.matchMedia("(min-width: 1100px)").matches,
      setRailOpen: (railOpen) => set({ railOpen }),
    }),
    { name: "peco-webui:rail" },
  ),
);
