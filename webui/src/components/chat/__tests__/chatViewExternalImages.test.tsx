// ChatView external 模式图片通路 —— 待发送图片经 onExternalSend 透出并清空预览

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ChatView } from "../ChatView";
import { useAuthStore } from "@/stores/authStore";
import { TooltipProvider } from "@/components/ui/tooltip";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

// createRoot + act 需要显式承诺 act 环境
(
  globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

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
});

const UPLOADED = { id: "a.png", url: "/uploads/chat/u/a.png" };

async function render(props: Partial<ComponentProps<typeof ChatView>> = {}) {
  await act(async () => {
    root.render(
      <TooltipProvider>
        <ChatView
        mode="external"
        streamUrl={(m) => `/api/peco/stream?message=${encodeURIComponent(m)}`}
        supportsImages
        uploadImage={async () => UPLOADED}
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

async function chooseFile(name: string) {
  const input = container.querySelector(
    'input[type="file"]',
  ) as HTMLInputElement;
  const file = new File([new Uint8Array([1, 2, 3])], name, {
    type: "image/png",
  });
  Object.defineProperty(input, "files", { value: [file], configurable: true });
  await act(async () => {
    input.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

async function typeText(text: string) {
  const ta = container.querySelector("textarea") as HTMLTextAreaElement;
  const setter = Object.getOwnPropertyDescriptor(
    HTMLTextAreaElement.prototype,
    "value",
  )!.set!;
  await act(async () => {
    setter.call(ta, text);
    ta.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function clickSend() {
  const btn = [...container.querySelectorAll("button")].find((b) =>
    b.querySelector("svg.lucide-send"),
  );
  if (!btn) throw new Error("send button not found");
  act(() => {
    btn.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

describe("ChatView external 图片通路", () => {
  it("待发送图片随文本交给 onExternalSend 并清空预览", async () => {
    const onExternalSend = vi.fn();
    await render({ onExternalSend });

    await chooseFile("a.png");
    await typeText("看这张图");
    expect(
      container.querySelectorAll('img[alt="待发送图片"]').length,
    ).toBe(1);

    clickSend();

    expect(onExternalSend).toHaveBeenCalledTimes(1);
    expect(onExternalSend).toHaveBeenCalledWith("看这张图", [UPLOADED]);
    expect(
      container.querySelectorAll('img[alt="待发送图片"]').length,
    ).toBe(0);
  });

  it("无图时 onExternalSend 收到空数组", async () => {
    const onExternalSend = vi.fn();
    await render({ onExternalSend });

    await typeText("纯文本");
    clickSend();

    expect(onExternalSend).toHaveBeenCalledWith("纯文本", []);
  });
});
