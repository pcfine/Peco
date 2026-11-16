import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { TooltipProvider } from "@/components/ui/tooltip";
import { ChatView, type ChatMessage } from "@/components/chat/ChatView";

const WAKE_CONTENT = [
  "[peco-wake] 后台任务结果（2 项）",
  "- task=t_a state=done target=ws_a dir=/tmp/a digest=改了三个文件",
].join("\n");

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
