import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { TooltipProvider } from "@/components/ui/tooltip";
import { ChatView, type ChatMessage } from "@/components/chat/ChatView";

const WAKE_CONTENT = [
  "[peco-wake] 后台任务结果（2 项）",
  "- task=t_a state=done target=ws_a dir=/tmp/a digest=改了三个文件",
].join("\n");

const originalClipboard = Object.getOwnPropertyDescriptor(
  navigator,
  "clipboard",
);

/** jsdom 没有 `navigator.clipboard`，复制按钮的断言得自己装一个。 */
function stubClipboard(value: unknown) {
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    writable: true,
    value,
  });
}

afterEach(() => {
  vi.restoreAllMocks();
  if (originalClipboard) {
    Object.defineProperty(navigator, "clipboard", originalClipboard);
  } else {
    delete (navigator as { clipboard?: unknown }).clipboard;
  }
});

function renderMessages(messages: ChatMessage[]) {
  return render(
    <TooltipProvider>
      <ChatView
        mode="external"
        streamUrl={() => "/api/stream"}
        initialMessages={messages}
        onExternalSend={() => {}}
        onExternalStop={() => {}}
        externalIsStreaming={false}
      />
    </TooltipProvider>,
  );
}

describe("对话里的唤醒消息", () => {
  it("唤醒投递渲染成折叠通知行，而不是用户气泡", () => {
    const { container } = renderMessages([
      { role: "user", content: WAKE_CONTENT, turnIndex: 0 },
    ]);

    expect(container.querySelector("[data-slot='wake-notice']")).not.toBeNull();
    expect(container.querySelector("[data-slot='user-bubble']")).toBeNull();
    // 折叠态拿不到正文：唤醒消息的内容不许在首屏里展开成一堵墙
    expect(screen.queryByText(/task=t_a/)).toBeNull();
  });

  it("普通 user 消息仍是用户气泡，不被前缀判定误伤", () => {
    const { container } = renderMessages([
      { role: "user", content: "帮我看下 [peco-wake] 是什么", turnIndex: 0 },
    ]);

    expect(container.querySelector("[data-slot='wake-notice']")).toBeNull();
    expect(screen.getByText("帮我看下 [peco-wake] 是什么")).toBeInTheDocument();
  });

  it("一条唤醒通知与一条用户消息并存时各走各的渲染", () => {
    const { container } = renderMessages([
      { role: "user", content: "跑了吗", turnIndex: 0 },
      { role: "user", content: WAKE_CONTENT, turnIndex: 1 },
    ]);

    expect(
      container.querySelectorAll("[data-slot='user-bubble']"),
    ).toHaveLength(1);
    expect(
      container.querySelectorAll("[data-slot='wake-notice']"),
    ).toHaveLength(1);
  });
});

describe("消息区垂直锚定", () => {
  function scrollerOf(container: HTMLElement): HTMLElement {
    const scroller = container.querySelector<HTMLElement>(
      "[data-slot='message-scroller']",
    );
    expect(scroller).not.toBeNull();
    return scroller as HTMLElement;
  }

  function hasClass(el: Element, name: string): boolean {
    return el.className.split(/\s+/).includes(name);
  }

  it("滚动容器内只有一层带 my-auto 的 message-list，间距随内容包下移", () => {
    const { container } = renderMessages([
      { role: "user", content: "在吗", turnIndex: 0 },
    ]);
    const scroller = scrollerOf(container);

    // 居中层必须是滚动容器的唯一子元素：多一层就会各自算一次自动外边距
    expect(scroller.children).toHaveLength(1);
    const list = scroller.querySelector("[data-slot='message-list']");
    expect(list).not.toBeNull();
    expect(hasClass(list as Element, "my-auto")).toBe(true);
    expect(hasClass(list as Element, "space-y-4")).toBe(true);

    // 居中靠 flex 列 + margin-block:auto；滚动容器自身不再带间距
    expect(hasClass(scroller, "flex")).toBe(true);
    expect(hasClass(scroller, "flex-col")).toBe(true);
    expect(hasClass(scroller, "space-y-4")).toBe(false);
    expect(hasClass(scroller, "justify-center")).toBe(false);
  });

  it("user 与 assistant 气泡全部是 message-list 的后代", () => {
    const { container } = renderMessages([
      { role: "user", content: "第一问", turnIndex: 0 },
      { role: "assistant", content: "第一答", turnIndex: 0 },
      { role: "user", content: "第二问", turnIndex: 1 },
      { role: "assistant", content: "第二答", turnIndex: 1 },
    ]);
    const scroller = scrollerOf(container);
    const list = scroller.querySelector<HTMLElement>(
      "[data-slot='message-list']",
    ) as HTMLElement;
    expect(list).not.toBeNull();

    const bubbles = [
      ...scroller.querySelectorAll("[data-slot='user-bubble']"),
      ...scroller.querySelectorAll("[data-slot='assistant-result']"),
    ];
    // 两种角色各自都在场，否则「全是后代」可能只是因为另一种根本没渲染
    expect(scroller.querySelectorAll("[data-slot='user-bubble']")).toHaveLength(
      2,
    );
    expect(
      scroller.querySelectorAll("[data-slot='assistant-result']"),
    ).toHaveLength(2);
    for (const bubble of bubbles) {
      expect(list.contains(bubble)).toBe(true);
    }
  });
});

describe("气泡下的复制按钮", () => {
  it("助手正文气泡带复制按钮，复制的是原始 markdown", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    stubClipboard({ writeText });
    const { container } = renderMessages([
      {
        role: "assistant",
        content: "**粗体** 与 `代码`",
        turnIndex: 0,
        timestampMs: Date.now(),
      },
    ]);

    const bubble = container.querySelector("[data-slot='assistant-result']");
    expect(bubble).not.toBeNull();
    // 显形与 pointer-events 都靠 group-hover，气泡容器必须带 group
    expect((bubble as Element).className.split(/\s+/)).toContain("group");
    const button = (bubble as Element).querySelector(
      "[data-slot='message-copy']",
    );
    expect(button).not.toBeNull();

    // 助手正文气泡左对齐：行全宽、时间与按钮分居两端
    const meta = (bubble as Element).querySelector(
      "[data-slot='message-meta']",
    );
    expect(meta).not.toBeNull();
    const metaClasses = (meta as Element).className.split(/\s+/);
    expect(metaClasses).toContain("left-0");
    expect(metaClasses).toContain("justify-between");

    fireEvent.click(button as Element);

    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith("**粗体** 与 `代码`"),
    );
    // 渲染结果里确实有 strong，复制值仍是带记号的原文
    expect((bubble as Element).querySelector("strong")).not.toBeNull();
  });

  it("用户气泡下的复制按钮给的是用户原文", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    stubClipboard({ writeText });
    const { container } = renderMessages([
      { role: "user", content: "**粗体** 问一句", turnIndex: 0 },
    ]);

    const bubble = container.querySelector("[data-slot='user-bubble']");
    const button = (bubble as Element).querySelector(
      "[data-slot='message-copy']",
    );
    expect(button).not.toBeNull();

    fireEvent.click(button as Element);

    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith("**粗体** 问一句"),
    );
  });

  it("错误气泡不带复制按钮", () => {
    const { container } = renderMessages([
      { role: "assistant", content: "出错了", turnIndex: 0, isError: true },
    ]);

    const bubble = container.querySelector("[data-slot='assistant-bubble']");
    expect(bubble).not.toBeNull();
    expect(
      (bubble as Element).querySelector("[data-slot='message-copy']"),
    ).toBeNull();
  });
});
