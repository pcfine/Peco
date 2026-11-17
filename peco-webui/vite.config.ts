import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

// daemon 默认监听 6971（见 crates/peco-daemon/src/config.rs 的 DEFAULT_BIND_ADDR）。
// 生产走同源，只有 `vite dev` 需要知道后端地址。
const daemonUrl = process.env.PECO_DAEMON_URL ?? "http://127.0.0.1:6971";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: {
    // 旧工程占 9233，两个前端并排跑时不能撞端口。
    port: 5174,
    proxy: {
      "/api": { target: daemonUrl, changeOrigin: true },
      "/uploads": { target: daemonUrl, changeOrigin: true },
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
