// ChatView external 模式暂停/恢复入口 —— 以 externalPaused prop 为权威暂停态
//
// 回归重点：external 模式的 SSE 由 store 管理，组件本地循环收不到
// `looper_state`，故暂停态必须由 prop 驱动；入口仍受「运行中 + 回调齐备」
// 双重门控（既有契约，不改）。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ChatView } from "../ChatView";
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

const pauseBtn = () => container.querySelector('button[aria-label="暂停"]');
const pendingBtn = () =>
  container.querySelector('button[aria-label="暂停中…"]');
const resumeBtn = () => container.querySelector('button[aria-label="恢复"]');

async function render(props: Partial<ComponentProps<typeof ChatView>> = {}) {
  await act(async () => {
    root.render(
      <TooltipProvider>
        <ChatView
          mode="external"
          streamUrl={(m) => `/api/peco/stream?message=${m}`}
          initialMessages={[]}
          onExternalSend={() => {}}
          onExternalStop={() => {}}
          externalIsStreaming={false}
          {...props}
        />
      </TooltipProvider>,
    );
  });
}

const onPause = () => vi.fn();
const onResume = () => vi.fn();

describe("ChatView external 模式暂停/恢复入口", () => {
  it("externalPaused=false 时显示「暂停」，点击触发 onPause", async () => {
    const pause = onPause();
    const resume = onResume();

    await render({
      externalIsStreaming: true,
      externalPaused: false,
      onPause: pause,
      onResume: resume,
    });

    expect(pauseBtn()).not.toBeNull();
    expect(resumeBtn()).toBeNull();

    await act(async () => {
      (pauseBtn() as HTMLButtonElement).click();
    });
    expect(pause).toHaveBeenCalledTimes(1);
    expect(resume).not.toHaveBeenCalled();
  });

  it("externalPaused=true 时显示「恢复」，点击触发 onResume", async () => {
    const pause = onPause();
    const resume = onResume();

    await render({
      externalIsStreaming: true,
      externalPaused: true,
      onPause: pause,
      onResume: resume,
    });

    expect(resumeBtn()).not.toBeNull();
    expect(pauseBtn()).toBeNull();

    await act(async () => {
      (resumeBtn() as HTMLButtonElement).click();
    });
    expect(resume).toHaveBeenCalledTimes(1);
    expect(pause).not.toHaveBeenCalled();
  });

  it("非流式时不出现入口（即使回调齐备）", async () => {
    await render({
      externalIsStreaming: false,
      externalPaused: false,
      onPause: onPause(),
      onResume: onResume(),
    });

    expect(pauseBtn()).toBeNull();
    expect(resumeBtn()).toBeNull();
  });

  it("未成对提供回调时不出现入口（即使流式中）", async () => {
    await render({
      externalIsStreaming: true,
      externalPaused: false,
      onPause: onPause(),
    });

    expect(pauseBtn()).toBeNull();
    expect(resumeBtn()).toBeNull();
  });

  it("点击暂停进入「暂停中…」，服务端挂起后切「恢复」", async () => {
    const pause = onPause();
    const resume = onResume();
    const props = {
      externalIsStreaming: true,
      externalPaused: false,
      onPause: pause,
      onResume: resume,
    };

    await render(props);
    await act(async () => {
      (pauseBtn() as HTMLButtonElement).click();
    });
    expect(pause).toHaveBeenCalledTimes(1);
    expect(pendingBtn()).not.toBeNull();
    expect(pauseBtn()).toBeNull();

    // 服务端确认挂起 → 暂态清除，入口切「恢复」
    await render({ ...props, externalPaused: true });
    expect(resumeBtn()).not.toBeNull();
    expect(pendingBtn()).toBeNull();
  });

  it("暂态期间再点一次撤回待办，触发 onResume", async () => {
    const pause = onPause();
    const resume = onResume();
    await render({
      externalIsStreaming: true,
      externalPaused: false,
      onPause: pause,
      onResume: resume,
    });

    await act(async () => {
      (pauseBtn() as HTMLButtonElement).click();
    });
    await act(async () => {
      (pendingBtn() as HTMLButtonElement).click();
    });
    expect(pause).toHaveBeenCalledTimes(1);
    expect(resume).toHaveBeenCalledTimes(1);
  });

  it("isStreaming 转 false 时清除待生效暂态", async () => {
    const pause = onPause();
    const resume = onResume();
    const props = {
      externalIsStreaming: true,
      externalPaused: false,
      onPause: pause,
      onResume: resume,
    };

    await render(props);
    await act(async () => {
      (pauseBtn() as HTMLButtonElement).click();
    });
    expect(pendingBtn()).not.toBeNull();

    await render({ ...props, externalIsStreaming: false });
    expect(container.querySelector('button[aria-label="暂停"]')).toBeNull();
    expect(pendingBtn()).toBeNull();

    // 重新进入流式：暂态已被清除，回到「暂停」
    await render(props);
    expect(pauseBtn()).not.toBeNull();
    expect(pendingBtn()).toBeNull();
  });
});
