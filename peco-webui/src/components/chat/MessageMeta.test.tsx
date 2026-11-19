import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { toast } from "sonner";
import {
  MessageMeta,
  type MessageMetaProps,
} from "@/components/chat/MessageMeta";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

const TIME = { label: "12:34", iso: "2026-01-02T12:34:00.000Z" };

const COPY_LABEL = "复制消息文本";
const COPIED_LABEL = "已复制";

const originalClipboard = Object.getOwnPropertyDescriptor(
  navigator,
  "clipboard",
);
const originalExecCommand = Object.getOwnPropertyDescriptor(
  document,
  "execCommand",
);

/** jsdom 默认没有 `navigator.clipboard`，`undefined` 就是非安全上下文的形状。 */
function stubClipboard(value: unknown) {
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    writable: true,
    value,
  });
}

function stubExecCommand(value: unknown) {
  Object.defineProperty(document, "execCommand", {
    configurable: true,
    writable: true,
    value,
  });
}

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  if (originalClipboard) {
    Object.defineProperty(navigator, "clipboard", originalClipboard);
  } else {
    delete (navigator as { clipboard?: unknown }).clipboard;
  }
  if (originalExecCommand) {
    Object.defineProperty(document, "execCommand", originalExecCommand);
  } else {
    delete (document as { execCommand?: unknown }).execCommand;
  }
});

function renderMeta(props: Partial<MessageMetaProps> = {}) {
  return render(
    <MessageMeta align="start" time={TIME} copyText="正文" {...props} />,
  );
}

function hasClass(el: Element, name: string): boolean {
  return el.className.split(/\s+/).includes(name);
}

/** 点击后的复制链路是异步的，靠空 act 把微任务推完。 */
async function flushAsync() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe("气泡元信息行", () => {
  it("默认隐藏：行容器 hover 才显形，按钮未悬停时不吃指针事件", () => {
    const { container } = renderMeta();
    const row = container.querySelector("[data-slot='message-meta']");

    expect(row).not.toBeNull();
    expect(hasClass(row as Element, "opacity-0")).toBe(true);
    expect(hasClass(row as Element, "group-hover:opacity-100")).toBe(true);
    // 键盘聚焦按钮时行也要显形，否则焦点环落在不可见元素上
    expect(hasClass(row as Element, "focus-within:opacity-100")).toBe(true);

    const button = screen.getByRole("button", { name: COPY_LABEL });
    expect(hasClass(button, "pointer-events-none")).toBe(true);
    expect(hasClass(button, "group-hover:pointer-events-auto")).toBe(true);
  });

  it("行容器盒边紧贴气泡下沿：top-full、零外边距、固定行高内置中", () => {
    const { container } = renderMeta();
    const row = container.querySelector(
      "[data-slot='message-meta']",
    ) as Element;
    const classes = row.className.split(/\s+/);

    expect(hasClass(row, "top-full")).toBe(true);
    // 任何 mt-* 都会重新撑出死区，指针从气泡移向按钮时祖先 :hover 先断
    expect(classes.filter((name) => name.startsWith("mt-"))).toEqual([]);
    // 行高与按钮同高，时间戳与图标按同一条中线居中
    expect(hasClass(row, "h-6")).toBe(true);
    expect(hasClass(row, "items-center")).toBe(true);
    expect(
      hasClass(screen.getByRole("button", { name: COPY_LABEL }), "size-6"),
    ).toBe(true);
  });

  it("两种对齐的 DOM 顺序一致：助手行全宽均分，用户行右锚且不反转", () => {
    const start = renderMeta({ align: "start" }).container.querySelector(
      "[data-slot='message-meta']",
    );
    const end = renderMeta({ align: "end" }).container.querySelector(
      "[data-slot='message-meta']",
    );

    expect(hasClass(start as Element, "left-0")).toBe(true);
    expect(hasClass(start as Element, "justify-between")).toBe(true);
    expect(hasClass(end as Element, "right-0")).toBe(true);
    // 反转行方向会把按钮推到气泡左边界之外，窄气泡下悬停链断开
    expect(hasClass(end as Element, "flex-row-reverse")).toBe(false);
    expect(hasClass(end as Element, "left-0")).toBe(false);

    for (const row of [start as Element, end as Element]) {
      expect(row.children[0].getAttribute("data-slot")).toBe("message-time");
      expect(row.children[1].getAttribute("data-slot")).toBe("message-copy");
    }
  });

  it("用户行按钮落在气泡右边界内侧：末子节点是按钮，行方向不反转", () => {
    const row = renderMeta({ align: "end" }).container.querySelector(
      "[data-slot='message-meta']",
    ) as Element;

    // 行右锚 + 不反转 => 末子节点贴着右边界，与气泡 x 区间重叠，指针径直下落即可命中
    expect(row.firstElementChild?.getAttribute("data-slot")).toBe(
      "message-time",
    );
    expect(row.lastElementChild?.getAttribute("data-slot")).toBe(
      "message-copy",
    );
    // 一旦方向反转，末子节点就被摆到最左，窄气泡下按钮整个落到气泡外
    expect(hasClass(row, "flex-row-reverse")).toBe(false);
  });

  it("时间戳保留 dateTime，不再自带定位类", () => {
    const { container } = renderMeta();
    const time = container.querySelector("[data-slot='message-time']");

    expect(time).not.toBeNull();
    expect(time).toHaveAttribute("datetime", TIME.iso);
    expect(time?.textContent).toBe(TIME.label);
    // 定位已上移到行容器，残留 absolute/top-full 会让两个时间戳叠在一起
    expect(hasClass(time as Element, "absolute")).toBe(false);
    expect(hasClass(time as Element, "top-full")).toBe(false);
  });

  it("点击复制写入精确文本，勾标 1500ms 后复位", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    stubClipboard({ writeText });
    vi.useFakeTimers();
    renderMeta({ copyText: "**粗体** 与 `代码` 原样" });

    const button = screen.getByRole("button", { name: COPY_LABEL });
    fireEvent.click(button);
    await flushAsync();

    expect(writeText).toHaveBeenCalledWith("**粗体** 与 `代码` 原样");
    expect(screen.getByRole("button", { name: COPIED_LABEL })).toBe(button);

    act(() => {
      vi.advanceTimersByTime(1499);
    });
    expect(screen.getByRole("button", { name: COPIED_LABEL })).toBe(button);

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(screen.getByRole("button", { name: COPY_LABEL })).toBe(button);
  });

  it("剪贴板 API 不存在时走 textarea 兜底，成功后同样显示已复制", async () => {
    stubClipboard(undefined);
    const execCommand = vi.fn().mockReturnValue(true);
    stubExecCommand(execCommand);
    renderMeta({ copyText: "兜底文本" });

    const button = screen.getByRole("button", { name: COPY_LABEL });
    // 键盘用户按回车复制：焦点必须留在按钮上，否则每次复制完都要重新 Tab 回来
    button.focus();
    expect(document.activeElement).toBe(button);

    fireEvent.click(button);
    await flushAsync();

    expect(execCommand).toHaveBeenCalledWith("copy");
    expect(screen.getByRole("button", { name: COPIED_LABEL })).toBe(button);
    // 兜底节点用完即拆：留在 DOM 里的 textarea 会让下一个气泡复制到旧值
    expect(document.querySelector("textarea")).toBeNull();
    expect(document.activeElement).toBe(button);
  });

  it("剪贴板 API 抛错时同样落到兜底路径", async () => {
    stubClipboard({
      writeText: vi.fn().mockRejectedValue(new Error("denied")),
    });
    const execCommand = vi.fn().mockReturnValue(true);
    stubExecCommand(execCommand);
    renderMeta({ copyText: "兜底文本" });

    fireEvent.click(screen.getByRole("button", { name: COPY_LABEL }));
    await flushAsync();

    expect(execCommand).toHaveBeenCalledWith("copy");
    expect(screen.getByRole("button", { name: COPIED_LABEL })).toBeTruthy();
  });

  it("两条路径都失败时提示手动选择，图标与标签都不改", async () => {
    stubClipboard({
      writeText: vi.fn().mockRejectedValue(new Error("denied")),
    });
    stubExecCommand(vi.fn().mockReturnValue(false));
    renderMeta();

    fireEvent.click(screen.getByRole("button", { name: COPY_LABEL }));
    await flushAsync();

    expect(toast.error).toHaveBeenCalledWith("复制失败，请手动选择文本");
    expect(screen.getByRole("button", { name: COPY_LABEL })).toBeTruthy();
    expect(toast.success).not.toHaveBeenCalled();
  });

  it.each([
    ["省略", undefined],
    ["空串", ""],
    ["全空白", "   "],
  ])("copyText %s时不渲染按钮，时间戳照旧", (_case, value) => {
    const { container } = renderMeta({ copyText: value });

    expect(container.querySelector("[data-slot='message-copy']")).toBeNull();
    expect(screen.queryByRole("button")).toBeNull();
    expect(
      container.querySelector("[data-slot='message-time']"),
    ).not.toBeNull();
  });

  it("只有正文没有时间戳时行照样渲染，但不落 time 元素", () => {
    const { container } = renderMeta({ time: null, copyText: "正文" });

    const row = container.querySelector("[data-slot='message-meta']");
    expect(row).not.toBeNull();
    expect(container.querySelector("[data-slot='message-time']")).toBeNull();
    expect(
      screen.getByRole("button", { name: COPY_LABEL }),
    ).toBeInTheDocument();
  });

  it("execCommand 也不存在时按失败处理：不抛异常、给出提示、不留 textarea", async () => {
    stubClipboard(undefined);
    stubExecCommand(undefined);
    renderMeta({ copyText: "兜底文本" });

    fireEvent.click(screen.getByRole("button", { name: COPY_LABEL }));
    await flushAsync();

    expect(toast.error).toHaveBeenCalledWith("复制失败，请手动选择文本");
    expect(screen.getByRole("button", { name: COPY_LABEL })).toBeTruthy();
    expect(document.querySelector("textarea")).toBeNull();
  });

  it("既无时间也无正文时整行不落 DOM", () => {
    const { container } = renderMeta({ time: null, copyText: undefined });

    expect(container.querySelector("[data-slot='message-meta']")).toBeNull();
  });
});
