import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { UserBubble } from "@/components/chat/UserBubble";
import type { ChatMessage } from "@/components/chat/ChatView";

const originalClipboard = Object.getOwnPropertyDescriptor(
  navigator,
  "clipboard",
);

function stubClipboard(value: unknown) {
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    writable: true,
    value,
  });
}

/** markdown 记号原样留着：复制的是原文，不是气泡里渲染出来的结果。 */
const RAW_CONTENT = "**加粗** 与 `代码` 都要原样";

function userMessage(extra: Partial<ChatMessage> = {}): ChatMessage {
  return { role: "user", content: "", turnIndex: 0, ...extra };
}

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => {
  vi.restoreAllMocks();
  if (originalClipboard) {
    Object.defineProperty(navigator, "clipboard", originalClipboard);
  } else {
    delete (navigator as { clipboard?: unknown }).clipboard;
  }
});

describe("用户气泡的复制按钮", () => {
  it("纯图片消息没有可复制正文，不出现复制按钮", () => {
    const { container } = render(
      <UserBubble
        message={userMessage({ images: ["data:image/png;base64,AAAA"] })}
      />,
    );

    expect(container.querySelector("[data-slot='message-copy']")).toBeNull();
    expect(screen.queryByRole("button", { name: "复制消息文本" })).toBeNull();
  });

  it("纯附件消息同样不出现复制按钮", () => {
    const { container } = render(
      <UserBubble
        message={userMessage({ attachments: [{ name: "a.pdf" }] })}
      />,
    );

    expect(container.querySelector("[data-slot='message-copy']")).toBeNull();
  });

  it("有正文时复制的是 message.content 原文", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    stubClipboard({ writeText });
    render(<UserBubble message={userMessage({ content: RAW_CONTENT })} />);

    fireEvent.click(screen.getByRole("button", { name: "复制消息文本" }));

    await waitFor(() => expect(writeText).toHaveBeenCalledWith(RAW_CONTENT));
  });

  it("图文同泡时按钮仍在气泡容器内，且容器就是 group 触发器", () => {
    const { container } = render(
      <UserBubble
        message={userMessage({
          content: RAW_CONTENT,
          images: ["data:image/png;base64,AAAA"],
        })}
      />,
    );

    const bubble = container.querySelector("[data-slot='user-bubble']");
    const meta = container.querySelector("[data-slot='message-meta']");
    expect(meta).not.toBeNull();
    // 行容器的显形与按钮的 pointer-events 都靠 group-hover，容器必须带 group
    expect(meta?.parentElement).toBe(bubble);
    expect(bubble?.className.split(/\s+/)).toContain("group");
    expect(bubble?.className.split(/\s+/)).toContain("relative");

    // 用户气泡右对齐：行右锚且不反转，按钮才落在气泡右边界内侧
    const metaClasses = (meta as Element).className.split(/\s+/);
    expect(metaClasses).toContain("right-0");
    expect(metaClasses).not.toContain("left-0");
    expect(metaClasses).not.toContain("flex-row-reverse");
  });
});
