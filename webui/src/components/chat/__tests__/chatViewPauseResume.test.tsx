// ChatView 暂停/恢复入口 —— 只在流式进行中、且父组件提供回调时出现
//
// 回归重点：① 入口受「运行中 + 回调齐备」双重门控；② 暂停态只认服务端
// `looper_state` 事件（POST 的 200 不代表已暂停，步进边界才生效），故按钮
// 在事件到达前保持「暂停」、到达后才切「恢复」。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ChatView, reduceStreamEvent } from "../ChatView";
import { useAuthStore } from "@/stores/authStore";
import { TooltipProvider } from "@/components/ui/tooltip";

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
});

function frame(event: string, data: unknown): string {
  return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}

/** 可手动推送事件、保持打开的 SSE 流（不关闭 → streaming 保持 true）。 */
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

const pauseBtn = () => container.querySelector('button[aria-label="暂停"]');
const resumeBtn = () => container.querySelector('button[aria-label="恢复"]');

async function render(
  props: Partial<ComponentProps<typeof ChatView>> = {},
) {
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

describe("ChatView 暂停/恢复入口", () => {
  it("未提供回调时不出现入口（即使正在流式）", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));

    await render({ initialQuery: "你好" });
    s.push(frame("text_delta", { content: "…", conversation_id: "c" }));
    await act(async () => {});

    expect(pauseBtn()).toBeNull();
    expect(resumeBtn()).toBeNull();
    // 停止按钮在流式中照常出现 —— 门控只作用于新增入口
    expect(
      container.querySelector('button[aria-label="暂停"]'),
    ).toBeNull();
  });

  it("流式进行中才出现；点击触发 onPause", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    const onPause = vi.fn();
    const onResume = vi.fn();

    await render({ initialQuery: "你好", onPause, onResume });

    await vi.waitFor(() => expect(pauseBtn()).not.toBeNull());
    expect(resumeBtn()).toBeNull();

    await act(async () => {
      (pauseBtn() as HTMLButtonElement).click();
    });
    expect(onPause).toHaveBeenCalledTimes(1);
    expect(onResume).not.toHaveBeenCalled();
  });

  it("收到 looper_state(paused) 后切换为恢复按钮，点击触发 onResume", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    const onPause = vi.fn();
    const onResume = vi.fn();

    await render({ initialQuery: "你好", onPause, onResume });
    await vi.waitFor(() => expect(pauseBtn()).not.toBeNull());

    // 事件到达前：仍是「暂停」（步进边界未到）
    await act(async () => {
      s.push(frame("looper_state", { paused: true, conversation_id: "c" }));
    });
    await vi.waitFor(() => expect(resumeBtn()).not.toBeNull());
    expect(pauseBtn()).toBeNull();

    await act(async () => {
      (resumeBtn() as HTMLButtonElement).click();
    });
    expect(onResume).toHaveBeenCalledTimes(1);
    expect(onPause).not.toHaveBeenCalled();
  });
});

describe("reduceStreamEvent looper_state", () => {
  it("不改动消息列表（纯状态事件）", () => {
    const messages = [
      { role: "user" as const, content: "你好", turnIndex: 0 },
    ];
    const next = reduceStreamEvent(
      {
        event: "looper_state",
        data: { paused: true, conversation_id: "c" },
      },
      messages,
    );
    expect(next).toEqual(messages);
  });
});
