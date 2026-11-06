// 主题只有两态落盘（dark / light）与一个「跟随系统」：首帧脚本读同一个 key。

import { create } from "zustand";
import { persist } from "zustand/middleware";

export type ThemeChoice = "dark" | "light" | "system";

interface ThemeState {
  theme: ThemeChoice;
  setTheme: (theme: ThemeChoice) => void;
}

function prefersDark(): boolean {
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/** 应用 `.dark` 类：token 反相靠这一个类，没有第二套命名。 */
export function applyTheme(theme: ThemeChoice): void {
  const dark = theme === "dark" || (theme === "system" && prefersDark());
  document.documentElement.classList.toggle("dark", dark);
}

export const useThemeStore = create<ThemeState>()(
  persist(
    (set) => ({
      theme: "dark",
      setTheme: (theme) => {
        applyTheme(theme);
        set({ theme });
      },
    }),
    {
      name: "peco-webui:theme",
      onRehydrateStorage: () => (state) => {
        applyTheme(state?.theme ?? "dark");
      },
    },
  ),
);
