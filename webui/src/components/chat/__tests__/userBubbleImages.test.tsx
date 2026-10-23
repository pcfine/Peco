// 用户消息图文气泡 + 图片点击放大 —— 同泡图文顺序、缩略图、空正文、失败态、关闭路径与焦点归还

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { UserBubble } from "../UserBubble";
import { UserImageLightbox } from "../UserImageLightbox";
import type { ChatMessage } from "../ChatView";

// createRoot + act 需要显式承诺 act 环境
(
  globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

const TS = 1767225600000;
const A = "/uploads/chat/u/a.png";
const B = "/uploads/chat/u/b.png";
const C = "/uploads/chat/u/c.png";

// I5b 的关闭计数与焦点目标：重复调用不可从 DOM 观察（setPreviewIndex(null) 幂等），
// 只能把 onClose 换成计数器
let closeCalls = 0;
let harnessTrigger: HTMLButtonElement | null = null;

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  closeCalls = 0;
  harnessTrigger = null;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

function userMessage(overrides: Partial<ChatMessage> = {}): ChatMessage {
  return {
    id: "u1",
    role: "user",
    content: "看图",
    turnIndex: 0,
    timestampMs: TS,
    ...overrides,
  };
}

async function renderUser(message: ChatMessage) {
  await act(async () => {
    root.render(<UserBubble message={message} />);
  });
}

/** radix 的焦点归还是在 setTimeout(…, 0) 里做的，不 flush 就断言焦点必假红 */
async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

const triggers = () =>
  Array.from(
    container.querySelectorAll<HTMLButtonElement>(
      '[data-slot="user-image-trigger"]',
    ),
  );
// 弹窗挂 body，不在 container 里
const dialog = () => document.querySelector('[data-slot="dialog-content"]');
const dialogTitle = () => document.querySelector('[data-slot="dialog-title"]');

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

async function openLightbox(index = 0) {
  const trigger = triggers()[index];
  if (!trigger) throw new Error(`没有第 ${index} 个图片触发按钮`);
  await click(trigger);
  return trigger;
}

describe("图文气泡结构", () => {
  it("I1 图片与文字同泡，且排在正文后面", async () => {
    await renderUser(userMessage({ images: [A] }));

    const imgs = container.querySelectorAll('img[alt="用户图片"]');
    expect(imgs).toHaveLength(1);
    const textBlock = container.querySelector('[data-slot="user-text"]');
    expect(textBlock).not.toBeNull();
    expect(imgs[0].closest('[data-slot="user-text"]')).toBe(textBlock);

    // 正文文本节点必须在图片槽位之前
    const imageSlot = textBlock!.querySelector('[data-slot="user-images"]');
    const textNode = Array.from(textBlock!.childNodes).find(
      (node) =>
        node.nodeType === Node.TEXT_NODE && node.textContent?.includes("看图"),
    );
    expect(imageSlot).not.toBeNull();
    expect(textNode).toBeDefined();
    expect(
      textNode!.compareDocumentPosition(imageSlot!) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("I9 空正文/空白正文不造空蓝药丸", async () => {
    for (const content of ["", " ", "\u00a0"]) {
      await renderUser(userMessage({ content, images: [A] }));

      expect(container.querySelector('[data-slot="user-text"]')).toBeNull();
      expect(container.querySelector(".bg-primary")).toBeNull();
      expect(
        container.querySelectorAll('[data-slot="user-images"] img'),
      ).toHaveLength(1);
    }
  });

  it("I13 图、正文、文档附件同泡且各归各位", async () => {
    await renderUser(
      userMessage({
        content: "看图",
        images: [A],
        attachments: [{ name: "季度报告.pdf" }],
      }),
    );

    const textBlocks = container.querySelectorAll('[data-slot="user-text"]');
    expect(textBlocks).toHaveLength(1);
    expect(textBlocks[0].textContent).toContain("季度报告.pdf");
    expect(textBlocks[0].textContent).toContain("看图");
    expect(textBlocks[0].querySelector('img[alt="用户图片"]')).not.toBeNull();
    expect(
      container.querySelector('[data-slot="user-images"]')?.textContent,
    ).toBe("");
    expect(
      container.querySelectorAll('[data-slot="message-time"]'),
    ).toHaveLength(1);
  });

  it("I12 时间戳唯一且挂组容器（hover 分组、定位基准）", async () => {
    await renderUser(userMessage({ images: [A] }));

    const bubble = container.querySelector(
      '[data-slot="user-bubble"]',
    ) as HTMLElement;
    const times = container.querySelectorAll('[data-slot="message-time"]');
    expect(times).toHaveLength(1);
    expect(times[0].parentElement).toBe(bubble);
    expect(bubble).toHaveClass("group", "relative");
  });
});

describe("图片块尺寸与失败态", () => {
  it("I11 每行最多 6 张，单元可收缩", async () => {
    await renderUser(userMessage({ images: [A, B, C, A] }));
    let rows = Array.from(
      container.querySelectorAll('[data-slot="user-image-row"]'),
    );
    expect(rows).toHaveLength(1);
    expect(Array.from(rows).map((r) => r.children.length)).toEqual([4]);

    await renderUser(userMessage({ images: [A, B, C, A, B, C, A] }));
    rows = Array.from(
      container.querySelectorAll('[data-slot="user-image-row"]'),
    );
    expect(rows).toHaveLength(2);
    expect(Array.from(rows).map((r) => r.children.length)).toEqual([6, 1]);

    await renderUser(userMessage({ images: Array(13).fill(A) }));
    rows = Array.from(
      container.querySelectorAll('[data-slot="user-image-row"]'),
    );
    expect(rows.map((r) => r.children.length)).toEqual([6, 6, 1]);

    // 定尺 + shrink-0 会溢出；flex-wrap 会优先换行而不是收缩
    for (const trigger of triggers()) {
      const classes = trigger.className.split(" ");
      expect(classes).toContain("w-24");
      expect(classes).toContain("min-w-0");
      expect(classes).toContain("shrink");
      expect(classes).toContain("aspect-square");
      expect(classes).not.toContain("shrink-0");
      expect(classes).not.toContain("flex-wrap");
    }
  });

  it("I15 所有缩略图同尺（96px 上限、cover 裁切）", async () => {
    // jsdom 无布局，仅守卫同尺与裁切类名；真实尺寸由浏览器验证
    for (const images of [[A], [A, B], [A, B, C, A]]) {
      await renderUser(userMessage({ images }));
      const all = triggers();
      expect(all).toHaveLength(images.length);
      for (const trigger of all) {
        expect(trigger).toHaveClass("w-24", "aspect-square");
        const img = trigger.querySelector("img");
        expect(img).toHaveClass("object-cover", "h-full", "w-full");
      }
    }
  });

  it("I9b 空串 src 直接进失败态，不给可点的触发按钮", async () => {
    await renderUser(userMessage({ content: "", images: [""] }));

    const placeholder = container.querySelector(
      '[data-slot="user-image-error"]',
    );
    expect(placeholder).not.toBeNull();
    expect(triggers()).toHaveLength(0);
    expect(dialog()).toBeNull();
    // 删掉 `w-24` 会退化成零宽，错误态看不见
    expect(placeholder).toHaveClass("w-24", "aspect-square");
  });

  it("I10 加载失败占位不可聚焦、不可点", async () => {
    await renderUser(userMessage({ images: [A, B] }));

    const img = container.querySelector('img[alt="用户图片"]');
    if (!img) throw new Error("没有渲染图片");
    await act(async () => {
      img.dispatchEvent(new Event("error"));
    });

    const placeholders = container.querySelectorAll(
      '[data-slot="user-image-error"]',
    );
    expect(placeholders).toHaveLength(1);
    expect(placeholders[0].textContent).toBe("图片加载失败");
    expect(placeholders[0].tagName).toBe("DIV");
    expect(placeholders[0].getAttribute("tabindex")).toBeNull();
    // 失败的那张退出触发按钮之列，兄弟不受影响
    expect(triggers()).toHaveLength(1);
  });
});

describe("焦点可见性", () => {
  it("I16 三档触发按钮都带焦点环类名", async () => {
    // jsdom 算不出对比度，这条只防「类名被删」；真实可见性由浏览器实测。
    for (const images of [[A], [A, B], [A, B, C, A]]) {
      await renderUser(userMessage({ images }));
      const all = triggers();
      expect(all).toHaveLength(images.length);
      for (const trigger of all) {
        expect(trigger).toHaveClass("focus-visible:ring-[3px]");
      }
    }
  });
});

describe("放大视图打开", () => {
  it("I3 点图打开，portal 挂在 body", async () => {
    await renderUser(userMessage({ images: [A] }));
    await openLightbox();

    expect(dialog()).not.toBeNull();
    expect(container.querySelector('[data-slot="dialog-content"]')).toBeNull();
  });

  it("I4 只显示被点的那张，标题用视图文案", async () => {
    await renderUser(userMessage({ images: [A, B, C] }));
    await openLightbox(1);

    const imgs = dialog()!.querySelectorAll("img");
    expect(imgs).toHaveLength(1);
    expect(imgs[0].getAttribute("src")).toBe(B);
    // 灯箱不复用「用户图片」这个 alt，否则按 alt 计数的既有用例会跟着变
    expect(imgs[0].getAttribute("alt")).toBe("图片预览");
    expect(dialogTitle()!.textContent).toContain("第 2 张图片，共 3 张");
  });

  it("I7 触发元素是原生 button 且带 haspopup", async () => {
    await renderUser(userMessage({ images: [A] }));

    const trigger = triggers()[0];
    expect(trigger.tagName).toBe("BUTTON");
    expect(trigger.getAttribute("type")).toBe("button");
    expect(trigger.getAttribute("aria-haspopup")).toBe("dialog");
    // 它不是开关，打开态由 dialog 是否存在表达
    expect(trigger.hasAttribute("aria-expanded")).toBe(false);
    expect(trigger.getAttribute("aria-label")).toBe("放大查看图片");
  });

  it("I8 单图与多图的 aria-label 各自带张数", async () => {
    await renderUser(userMessage({ images: [A] }));
    expect(triggers()[0].getAttribute("aria-label")).toBe("放大查看图片");

    await renderUser(userMessage({ images: [A, B, C] }));
    expect(triggers().map((t) => t.getAttribute("aria-label"))).toEqual([
      "放大查看第 1 张图片，共 3 张",
      "放大查看第 2 张图片，共 3 张",
      "放大查看第 3 张图片，共 3 张",
    ]);
  });

  it("I14 触发按钮文案与视图标题互不串用", async () => {
    await renderUser(userMessage({ images: [A, B, C] }));
    const trigger = await openLightbox(0);

    expect(trigger.getAttribute("aria-label")).toBe(
      "放大查看第 1 张图片，共 3 张",
    );
    expect(dialogTitle()!.textContent).toBe("第 1 张图片，共 3 张");
  });
});

describe("关闭路径与焦点归还", () => {
  it("I5 Esc 关闭后焦点回触发图", async () => {
    await renderUser(userMessage({ images: [A] }));
    const trigger = await openLightbox();
    await flush();

    await act(async () => {
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      );
    });
    await flush();

    expect(dialog()).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("I6 关闭按钮关闭后焦点回触发图，且不自建 data-slot", async () => {
    await renderUser(userMessage({ images: [A] }));
    const trigger = await openLightbox();
    await flush();

    // 自建按钮之外不该再有任何按钮：ui/dialog 的默认关闭按钮没有中文 label，
    // 只按 label 取元素会漏掉它多出来的一颗
    const content = dialog();
    if (!content) throw new Error("放大视图没打开");
    expect(content.querySelectorAll("button")).toHaveLength(1);

    // 默认关闭按钮带英文 "Close" 文本与另一套定位类串，自建的不复用它们
    const close = document.querySelector('[aria-label="关闭图片预览"]');
    if (!close) throw new Error("没有关闭按钮");
    expect(close.querySelector(".sr-only")).toBeNull();
    expect(close.textContent?.trim()).toBe("");
    await click(close);
    await flush();

    expect(dialog()).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("I5b 点容器空白关闭，onClose 恰好一次且焦点回触发图", async () => {
    await act(async () => {
      root.render(<ControlledLightbox />);
    });

    const content = dialog();
    if (!content) throw new Error("放大视图没打开");
    await act(async () => {
      for (const type of ["pointerdown", "pointerup", "click"]) {
        content.dispatchEvent(
          new MouseEvent(type, { bubbles: true, button: 0 }),
        );
      }
    });
    await flush();

    expect(dialog()).toBeNull();
    expect(closeCalls).toBe(1);
    expect(document.activeElement).toBe(harnessTrigger);
  });
});

// ── I5b 的计数宿主 ─────────────────────────────────────────────────────────

function ControlledLightbox() {
  const triggerElRef = useRef<HTMLButtonElement | null>(null);
  const [image, setImage] = useState<{ src: string; label: string } | null>({
    src: A,
    label: "图片预览",
  });

  return (
    <>
      <button
        type="button"
        ref={(el) => {
          triggerElRef.current = el;
          harnessTrigger = el;
        }}
      >
        打开
      </button>
      <UserImageLightbox
        image={image}
        triggerElRef={triggerElRef}
        onClose={() => {
          closeCalls += 1;
          setImage(null);
        }}
      />
    </>
  );
}
