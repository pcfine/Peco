// ChatView external 附件通路 —— uploadFile 解耦附件入口、文档 chip 预览与透出
//
// 回归重点：附件入口由 uploadFile 决定（与 supportsImages 解耦，D6）；
// 文档不依赖视觉能力，无文本的附件-only 消息允许发送（D7）。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ChatView } from "../ChatView";
import { useAuthStore } from "@/stores/authStore";
import { TooltipProvider } from "@/components/ui/tooltip";
import { toast } from "sonner";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

// createRoot + act 需要显式承诺 act 环境
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

/** 统一附件端点：图片与文档都经它上传（此处只测文档分支）。 */
const uploadFile = vi.fn(
  async (
    file: File,
  ): Promise<{ id: string; kind: "image" | "document"; name: string }> => ({
    id: "f1",
    kind: "document",
    name: file.name,
  }),
);

async function render(props: Partial<ComponentProps<typeof ChatView>> = {}) {
  await act(async () => {
    root.render(
      <TooltipProvider>
        <ChatView
          mode="external"
          streamUrl={(m) => `/api/peco/stream?message=${encodeURIComponent(m)}`}
          uploadFile={uploadFile}
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

async function chooseFile(name: string, type = "application/octet-stream") {
  const input = container.querySelector(
    'input[type="file"]',
  ) as HTMLInputElement;
  const file = new File([new Uint8Array([1, 2, 3])], name, { type });
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

describe("ChatView external 附件通路", () => {
  it("附件入口由 uploadFile 决定：supportsImages=false 时仍可用", async () => {
    await render({ supportsImages: false, uploadImage: undefined });

    const attachBtn = container.querySelector(
      'button[aria-label="添加附件"]',
    ) as HTMLButtonElement;
    expect(attachBtn).not.toBeNull();
    expect(attachBtn.disabled).toBe(false);
  });

  it("文档经 chip 预览，随文本交给 onExternalSend 并清空", async () => {
    const onExternalSend = vi.fn();
    await render({ onExternalSend });

    await chooseFile("季度报告.pdf", "application/pdf");
    expect(container.textContent).toContain("季度报告.pdf");

    await typeText("总结这份报告");
    clickSend();

    expect(onExternalSend).toHaveBeenCalledTimes(1);
    expect(onExternalSend).toHaveBeenCalledWith("总结这份报告", [
      { id: "f1", kind: "document", name: "季度报告.pdf" },
    ]);
    // 预览已清空
    expect(container.textContent).not.toContain("季度报告.pdf");
  });

  it("附件-only（无文本）允许发送", async () => {
    const onExternalSend = vi.fn();
    await render({ onExternalSend });

    await chooseFile("笔记.txt", "text/plain");
    // 无文本但有待发送附件 —— 发送按钮可用
    const btn = [...container.querySelectorAll("button")].find((b) =>
      b.querySelector("svg.lucide-send"),
    ) as HTMLButtonElement;
    expect(btn.disabled).toBe(false);

    clickSend();

    expect(onExternalSend).toHaveBeenCalledWith("", [
      { id: "f1", kind: "document", name: "笔记.txt" },
    ]);
  });

  it("supportsImages=false 时图片被拒（toast），文档不受影响", async () => {
    const onExternalSend = vi.fn();
    await render({ onExternalSend, supportsImages: false });

    await chooseFile("photo.png", "image/png");
    expect(toast.error).toHaveBeenCalledWith("当前模型不支持图片输入");
    expect(container.textContent).not.toContain("photo.png");

    await chooseFile("笔记.txt", "text/plain");
    expect(container.textContent).toContain("笔记.txt");
  });

  it("快照用户消息渲染文档附件 chip", async () => {
    await render({
      initialMessages: [
        {
          role: "user",
          content: "总结这个文档",
          turnIndex: 0,
          attachments: [{ name: "季度报告.pdf" }],
        },
        { role: "assistant", content: "要点如下", turnIndex: 0 },
      ],
    });

    expect(container.textContent).toContain("季度报告.pdf");
    expect(container.textContent).toContain("总结这个文档");
    // 文档正文（附件解析文本）不来自附件名字段 —— 附件只渲染为 chip
    expect(container.querySelectorAll('img[alt="用户图片"]')).toHaveLength(0);
  });
});
