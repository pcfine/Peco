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
