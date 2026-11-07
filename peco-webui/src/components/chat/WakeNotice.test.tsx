import { describe, expect, it } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { WakeNotice, isWakeNotice } from "@/components/chat/WakeNotice";

/** daemon 投递文本的样子：首行带项数，其后一项一行。 */
const WAKE_CONTENT = [
  "[peco-wake] 后台任务结果（2 项）",
  "- task=t_a state=done target=ws_a dir=/tmp/a digest=改了三个文件",
  "- task=t_b state=orphan target=ws_b dir=/tmp/b digest=任务在进程重启时中断，输出已丢失",
].join("\n");

function renderNotice(content: string) {
  return render(<WakeNotice content={content} />);
}

function trigger() {
  return screen.getByRole("button");
}

describe("唤醒通知折叠行", () => {
  it("默认折叠：正文不在 DOM 里", () => {
    const { container } = renderNotice(WAKE_CONTENT);

    expect(trigger()).toHaveAttribute("aria-expanded", "false");
    expect(container.querySelector("[data-slot='wake-body']")).toBeNull();
    expect(screen.queryByText(/task=t_a/)).toBeNull();
  });

  it("点击后展开：task= 与 digest= 都能读到", () => {
    renderNotice(WAKE_CONTENT);
    fireEvent.click(trigger());

    expect(trigger()).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText(/task=t_a/)).toBeInTheDocument();
    expect(screen.getByText(/digest=改了三个文件/)).toBeInTheDocument();
  });

  it("首行的项数上标签，不额外渲染第二行正文", () => {
    renderNotice(WAKE_CONTENT);

    expect(screen.getByText("后台任务通知 · 2 项")).toBeInTheDocument();
    expect(screen.queryByText(/task=t_b/)).toBeNull();
  });

  it("认不出的首行退化为不带项数的标签，正文仍可展开", () => {
    const content = "[peco-wake] 后台任务结果\n- task=t_c state=done";
    const { container } = renderNotice(content);

    expect(screen.getByText("后台任务通知")).toBeInTheDocument();
    expect(screen.queryByText(/项/)).toBeNull();
    expect(trigger()).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(trigger());
    // 不改写正文：模型收到的与用户点开的是同一段文本
    expect(
      container.querySelector("[data-slot='wake-body']")?.textContent,
    ).toBe(content);
  });

  it("展开体原样输出正文，保留换行结构", () => {
    const { container } = renderNotice(WAKE_CONTENT);
    fireEvent.click(trigger());

    const body = container.querySelector("[data-slot='wake-body']");
    expect(body?.textContent).toBe(WAKE_CONTENT);
    expect(body).toHaveClass("whitespace-pre-wrap");
  });

  it("折叠行是可聚焦的原生按钮：Tab 可达，Enter/Space 由按钮语义展开", () => {
    renderNotice(WAKE_CONTENT);
    trigger().focus();

    expect(trigger()).toHaveFocus();
    expect(trigger().tagName).toBe("BUTTON");
    expect(trigger()).toHaveAttribute("type", "button");
  });

  it("不与用户气泡同色", () => {
    const { container } = renderNotice(WAKE_CONTENT);

    expect(
      container.querySelector("[data-slot='wake-notice']"),
    ).not.toHaveClass("bg-primary");
    expect(trigger()).not.toHaveClass("bg-primary");
  });
});

describe("唤醒消息判定", () => {
  it.each([
    ["[peco-wake] 后台任务结果（1 项）\n- task=t_a", true],
    ["  [peco-wake] 后台任务结果（0 项）", true],
    ["你好，帮我看看 [peco-wake] 是什么", false],
    ["> [peco-wake] 引用来的前缀", false],
    ["", false],
  ])("%s → %s", (content, expected) => {
    expect(isWakeNotice(content)).toBe(expected);
  });
});
