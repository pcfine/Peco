import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { QueryClientProvider } from "@tanstack/react-query";
import { Toaster } from "sonner";
import { App } from "@/App";
import { queryClient } from "@/lib/query";
import { applyTheme, useThemeStore } from "@/stores/themeStore";
import "./index.css";

// 首帧脚本已套用 .dark；这里只补「跟随系统」下的系统切换。
window
  .matchMedia("(prefers-color-scheme: dark)")
  .addEventListener("change", () => {
    const theme = useThemeStore.getState().theme;
    if (theme === "system") applyTheme(theme);
  });

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <App />
      </BrowserRouter>
      <Toaster position="bottom-right" theme="dark" />
    </QueryClientProvider>
  </StrictMode>,
);
