// ChatView 在途发送入口 —— external 模式允许流式中继续输入/发送（消息排队，
// 由 store 经控制通道投递）；internal 模式仍要求轮到 Idle，避免新 SSE 顶替在途 run。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ChatView } from "../ChatView";
import { useAuthStore } from "@/stores/authStore";
import {
  usePecoChatStore,
  __resetPecoStreamForTests,
} from "@/stores/pecoChatStore";
import { TooltipProvider } from "@/components/ui/tooltip";

vi.mock("@/api/peco", () => ({
  pecoStreamUrl: (m: string) =>
    `/api/peco/stream?message=${encodeURIComponent(m)}`,
  uploadPecoImage: vi.fn(),
  pecoAttachUrl: () => "/api/peco/stream",
  queryPecoStream: vi.fn(async () => undefined),
  cancelPecoStream: vi.fn(async () => ({ success: true })),
  pausePecoStream: vi.fn(async () => ({ success: true })),
  resumePecoStream: vi.fn(async () => ({ success: true })),
  clearPecoSession: vi.fn(async () => ({ success: true })),
  getPecoSession: vi.fn(),
}));

(
  globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.clearAllMocks();
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

async function render(props: Partial<ComponentProps<typeof ChatView>> = {}) {
  await act(async () => {
    root.render(
      <TooltipProvider>
        <ChatView
          streamUrl={(m) => `/api/chat/a/conversations/c/stream?message=${m}`}
          initialMessages={[]}
          {...props}
        />
      </TooltipProvider>,
    );
  });
}

const textarea = () =>
  container.querySelector("textarea") as HTMLTextAreaElement;
const stopBtn = () => container.querySelector('button[aria-label="停止"]');
const sendBtn = () =>
  [...container.querySelectorAll("button")].find((b) =>
    b.querySelector("svg.lucide-send"),
  );

describe("ChatView 在途发送入口", () => {
  it("external 流式中：输入框可用、停止与发送并存，Enter 触发 onExternalSend", async () => {
    const onExternalSend = vi.fn();
    await render({
      mode: "external",
      externalIsStreaming: true,
      onExternalSend,
      onExternalStop: () => {},
    });

    expect(textarea().disabled).toBe(false);
    expect(stopBtn()).not.toBeNull();
    expect(sendBtn()).not.toBeUndefined();

    const setter = Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )!.set!;
    await act(async () => {
      setter.call(textarea(), "补充");
      textarea().dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => {
      textarea().dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Enter",
          bubbles: true,
          cancelable: true,
        }),
      );
    });

    expect(onExternalSend).toHaveBeenCalledWith("补充", []);
    expect(textarea().value).toBe("");
  });

  it("external 模式展示已排队输入", async () => {
    await render({
      mode: "external",
      externalIsStreaming: true,
      onExternalSend: () => {},
      onExternalStop: () => {},
      externalQueued: [{ id: "q1", text: "排队中的补充" }],
    });

    expect(container.textContent).toContain("已排队");
    expect(container.textContent).toContain("排队中的补充");
  });

  it("internal 流式中：输入框禁用且只显示停止按钮", async () => {
    const encoder = new TextEncoder();
    let controller!: ReadableStreamDefaultController<Uint8Array>;
    const stream = new ReadableStream<Uint8Array>({
      start(c) {
        controller = c;
      },
    });
    global.fetch = vi.fn(() =>
      Promise.resolve(new Response(stream, { status: 200 })),
    );

    await render({ initialQuery: "你好" });
    await act(async () => {
      controller.enqueue(
        encoder.encode(
          `event: text_delta\ndata: ${JSON.stringify({ content: "…", conversation_id: "c" })}\n\n`,
        ),
      );
    });

    await vi.waitFor(() => expect(textarea().disabled).toBe(true));
    expect(stopBtn()).not.toBeNull();
    expect(sendBtn()).toBeUndefined();
  });
});

// ── steering 切分后的终态与耗时 ──────────────────────────────────────────
//
// 切分出的占位气泡没有任何内容，终态行是它唯一的渲染物；耗时起点必须越过
// steering 消息（时间戳是排队时刻）向前取本轮真正的起点。

function frame(event: string, data: unknown): string {
  return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}

function controllableStream() {
  const encoder = new TextEncoder();
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const stream = new ReadableStream<Uint8Array>({
    start(c) {
      controller = c;
    },
  });
  return {
    response: new Response(stream, { status: 200 }),
    push: (...frames: string[]) => {
      for (const f of frames) controller.enqueue(encoder.encode(f));
    },
  };
}

const T0 = 1_700_000_000_000;

/** 一轮在途 + 一条已排队输入，并把它切分成独立气泡（模拟 user_steer 到达）。 */
async function steeredTurn() {
  const s = controllableStream();
  global.fetch = vi.fn(() => Promise.resolve(s.response));
  await usePecoChatStore.getState().sendMessage("第一条", "test-token");
  s.push(frame("text_delta", { content: "旧", conversation_id: "c" }));
  await vi.waitFor(() =>
    expect(usePecoChatStore.getState().messages.at(-1)?.content).toBe("旧"),
  );

  await usePecoChatStore.getState().sendMessage("补充", "test-token");
  s.push(frame("user_steer", { count: 1, text: "补充", conversation_id: "c" }));
  await vi.waitFor(() =>
    expect(usePecoChatStore.getState().queued).toHaveLength(0),
  );
}

async function renderCurrentMessages() {
  await render({
    mode: "external",
    externalIsStreaming: false,
    initialMessages: usePecoChatStore.getState().messages,
    onExternalSend: () => {},
    onExternalStop: () => {},
  });
  const row = container.querySelector('[data-slot="turn-status"]');
  expect(row).not.toBeNull();
  return row as HTMLElement;
}

describe("ChatView steering 切分后的终态行", () => {
  beforeEach(() => {
    __resetPecoStreamForTests();
    usePecoChatStore.setState({
      messages: [],
      isStreaming: false,
      paused: false,
      queued: [],
      error: null,
      sessionKey: 0,
    });
  });

  it("切分出的空白占位仍渲染取消终态行", async () => {
    await steeredTurn();

    usePecoChatStore.getState().abortStream();

    const row = await renderCurrentMessages();
    expect(row.getAttribute("data-status")).toBe("cancelled");
    expect(row.textContent).toContain("取消");
  });

  it("耗时从本轮真正的起点算起，不取 steering 的排队时刻", async () => {
    const now = vi.spyOn(Date, "now");
    now.mockReturnValue(T0);
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    await usePecoChatStore.getState().sendMessage("第一条", "test-token");
    s.push(frame("text_delta", { content: "旧", conversation_id: "c" }));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().messages.at(-1)?.content).toBe("旧"),
    );

    // 排队 60 秒后才被并入当前轮；轮次再跑 30 秒收尾
    now.mockReturnValue(T0 + 60_000);
    await usePecoChatStore.getState().sendMessage("补充", "test-token");
    s.push(
      frame("user_steer", { count: 1, text: "补充", conversation_id: "c" }),
    );
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().queued).toHaveLength(0),
    );

    now.mockReturnValue(T0 + 90_000);
    usePecoChatStore.getState().abortStream();

    const row = await renderCurrentMessages();
    expect(row.textContent).toContain("1 分 30 秒");
    expect(row.textContent).not.toContain("30.0 秒");
  });
});
