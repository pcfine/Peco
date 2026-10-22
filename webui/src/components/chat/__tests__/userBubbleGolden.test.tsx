// 用户消息气泡 DOM 快照 —— 纯文字形态必须逐字节不变，类串或层级漂移先在这里红

import { writeFileSync } from "node:fs";
import path from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ChatView, type ChatMessage } from "../ChatView";
import { TooltipProvider } from "@/components/ui/tooltip";
import { useAuthStore } from "@/stores/authStore";
import { PLAIN_TEXT, WITH_DOC } from "./fixtures/userBubbleGoldens";

// 快照里的时间标签是本地时间（`formatMessageTime` 走 getHours），不钉时区会在
// TZ=UTC 的机器上得到 00:00 而不是录制时的 08:00。Node ≥16 支持运行时改 TZ 生效。
process.env.TZ = "Asia/Shanghai";

(
  globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

const RECORD = process.env.GOLDEN_RECORD === "1";
// jsdom 环境下 import.meta.url 不是 file 协议，只能靠 vitest 的 cwd（= webui 根）
const FIXTURE_PATH = path.resolve(
  process.cwd(),
  "src/components/chat/__tests__/fixtures/userBubbleGoldens.ts",
);
const FIXED_NOW = Date.UTC(2026, 5, 1, 4, 0, 0);
const USER_TS = 1767225600000;

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  useAuthStore.setState({ token: "test-token" });
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
});

function userMessage(overrides: Partial<ChatMessage> = {}): ChatMessage {
  return {
    id: "u1",
    role: "user",
    content: "短消息",
    turnIndex: 0,
    timestampMs: USER_TS,
    ...overrides,
  };
}

async function bubbleHtmlFor(message: ChatMessage): Promise<string> {
  // 时间戳的 label 档位取决于「现在」，不钉住 Date.now 快照会随日历漂移
  vi.spyOn(Date, "now").mockReturnValue(FIXED_NOW);
  await act(async () => {
    root.render(
      <TooltipProvider>
        <ChatView
          mode="external"
          streamUrl={(m) => `/api/peco/stream?message=${encodeURIComponent(m)}`}
          initialMessages={[message]}
          onExternalSend={() => {}}
          onExternalStop={() => {}}
          externalIsStreaming={false}
        />
      </TooltipProvider>,
    );
  });
  const el = container.querySelector('[data-slot="user-bubble"]');
  if (!el) throw new Error('未找到 [data-slot="user-bubble"]');
  // 只截气泡本身会漏掉外层留白/对齐包装的漂移，连它一起录
  const wrapper = el.parentElement;
  if (!wrapper) throw new Error("气泡没有外层包装");
  return wrapper.outerHTML;
}

describe("用户消息气泡 DOM 快照", () => {
  if (RECORD) {
    it("录制两种形态的快照常量", async () => {
      const recorded: Record<string, string> = {
        PLAIN_TEXT: await bubbleHtmlFor(userMessage()),
        WITH_DOC: await bubbleHtmlFor(
          userMessage({ attachments: [{ name: "季度报告.pdf" }] }),
        ),
      };
      const source = [
        "// 由 `GOLDEN_RECORD=1 npx vitest run userBubbleGolden` 生成，勿手改",
        "",
        ...Object.entries(recorded).map(
          ([key, html]) => `export const ${key} = ${JSON.stringify(html)};`,
        ),
        "",
      ].join("\n");
      writeFileSync(FIXTURE_PATH, source);
      for (const html of Object.values(recorded)) {
        expect(html).toContain('data-slot="user-bubble"');
      }
    });
    return;
  }

  it("纯文字消息逐字节不变", async () => {
    expect(await bubbleHtmlFor(userMessage())).toBe(PLAIN_TEXT);
  });

  it("文字+文档附件消息逐字节不变", async () => {
    expect(
      await bubbleHtmlFor(userMessage({ attachments: [{ name: "季度报告.pdf" }] })),
    ).toBe(WITH_DOC);
  });
});
