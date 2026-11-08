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

/**
 * rail 的渲染宽度：`viewport−rail` 是 shell 宽度，dock 与 main 都按它分档。
 * 900–1099 强制图标轨，但只压渲染层 —— 存档里的偏好原样留着，视口放宽即恢复。
 */
export function railWidth(viewport: number, railOpen: boolean): 0 | 64 | 208 {
  if (viewport < 900) return 0;
  if (viewport < 1100) return 64;
  return railOpen ? 208 : 64;
}
