import { defineConfig } from "@playwright/test";

// 浏览器验收台：`headless: false` 必须在 X 下跑（run.sh 用 xvfb-run 提供）。
// 单 worker 串行：断言是「同一时刻的几何/颜色」，并行会互相抢视口与终端输出。
export default defineConfig({
  testDir: ".",
  // 产物落在 e2e/ 内（e2e/.gitignore 已忽略），仓库根不留 playwright 输出目录。
  outputDir: "./.report",
  testMatch: /\.spec\.ts$/,
  timeout: 90_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: process.env.E2E_BASE_URL ?? "http://127.0.0.1:5174",
    headless: false,
    viewport: { width: 1920, height: 1080 },
    trace: "off",
    video: "off",
    screenshot: "off",
  },
});
