// PecoChatPage 的产物面板在宽视口（≥1440）下：与对话并排、非模态。
//
// 用例前缀 J1/J2/J4–J8、J11–J14 是面板判据的编号，宽窄两侧共用一套、各文件只压
// 自己形态的那几条。窄屏抽屉形态在 unifiedDrawer.test.tsx，跨断点切换在 resize 文件。真实宽度、可交互性、高度链
// 只有浏览器能验（jsdom 无布局引擎），这里只守"写法在、语义对、焦点归属对"。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { usePecoArtifactStore } from "@/stores/pecoArtifactStore";
import {
  __resetPecoStreamForTests,
  usePecoChatStore,
} from "@/stores/pecoChatStore";
import type { ArtifactItem } from "@/types/artifact";
import { PecoChatPage } from "../PecoChatPage";
import {
  installFlippableMatchMedia,
  type FlippableViewport,
} from "./flippableMatchMedia";

const chatViewMounts = vi.hoisted(() => ({ count: 0 }));

vi.mock("@/components/chat/ChatView", async (importOriginal) => {
  const [{ createElement, useEffect }, actual] = await Promise.all([
    import("react"),
    importOriginal<typeof import("@/components/chat/ChatView")>(),
  ]);
  return {
    ...actual,
    // 计数放在 effect 里：重渲染不算挂载，卸载重建才算
    ChatView: ({ headerActions }: { headerActions?: unknown }) => {
      useEffect(() => {
        chatViewMounts.count += 1;
      }, []);
      return createElement(
        "div",
        { "data-testid": "chat-view" },
        headerActions as never,
      );
    },
  };
});

vi.mock("@/api/agents", () => ({
  getAssistantSupportsImages: vi.fn(async () => false),
}));

vi.mock("@/api/peco", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/api/peco")>()),
  listPecoArtifacts: vi.fn(() => new Promise(() => {})),
  getPecoSession: vi.fn(() => new Promise(() => {})),
}));

// 查看器挂载即取流；这里要的是"弹窗开着"，取流永不落地最省事
vi.mock("@/api/artifactBlob", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/api/artifactBlob")>()),
  getArtifactObjectUrl: vi.fn(() => new Promise(() => {})),
}));

/**
 * 图片与文档各一条：`TabsTrigger` 在分类计数为 0 时置灰，只种文档时「图片」
 * 点不动，J8 会误红；J13 需要图片卡上的预览按钮。
 */
const imageArtifact: ArtifactItem = {
  id: "a1",
  conversation_id: "c1",
  kind: "image",
  mime: "image/png",
  title: "照片.png",
  size_bytes: 1024,
  url: "/api/artifacts/a1/content",
  download_url: "/api/artifacts/a1/download",
  created_at: "2026-10-20 12:00:00",
  available: true,
};

const documentArtifact: ArtifactItem = {
  id: "a2",
  conversation_id: "c1",
  kind: "document",
  mime: "application/pdf",
  title: "报告.pdf",
  size_bytes: 2048,
  url: "/api/artifacts/a2/content",
  download_url: "/api/artifacts/a2/download",
  created_at: "2026-10-20 12:00:00",
  available: true,
};

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

/** 图片卡进视口即取流，挂载时就会构造观察器；这里不需要真触发进入视口。 */
class IntersectionObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

let container: HTMLDivElement;
let root: Root;
let viewport: FlippableViewport;

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

async function render() {
  await act(async () => {
    root.render(<PecoChatPage />);
  });
  await flush();
}

function openButton(): HTMLButtonElement {
  const button = container.querySelector<HTMLButtonElement>(
    '[aria-label^="打开产物面板"]',
  );
  if (!button) throw new Error("open button not found");
  return button;
}

/** 并排容器是内联在页面里的，作用域是 `container`（窄屏抽屉在 body 上的 Portal 里）。 */
function aside(): HTMLElement | null {
  return container.querySelector<HTMLElement>('[data-slot="artifact-drawer"]');
}

function sheetContent(rootNode: ParentNode): Element | null {
  return rootNode.querySelector('[data-slot="sheet-content"]');
}

/** 按钮没有 `aria-label`，名字取自 sr-only 文本，按 textContent 即按名字定位。 */
function closeButton(rootNode: ParentNode): HTMLButtonElement {
  const found = Array.from(
    rootNode.querySelectorAll<HTMLButtonElement>("button"),
  ).find((button) => button.textContent?.trim() === "关闭");
  if (!found) throw new Error("产物面板里没有名为「关闭」的按钮");
  return found;
}

/** radix TabsTrigger 在 mousedown（button=0）上激活，click 不切 tab。 */
async function activateTab(rootNode: ParentNode, label: string) {
  const tab = Array.from(
    rootNode.querySelectorAll<HTMLButtonElement>('[role="tab"]'),
  ).find((element) => element.textContent?.includes(label));
  if (!tab) throw new Error(`tab not found: ${label}`);
  await act(async () => {
    tab.dispatchEvent(
      new MouseEvent("mousedown", { bubbles: true, button: 0 }),
    );
  });
  await flush();
}

function activeTabText(rootNode: ParentNode): string {
  const active = rootNode.querySelector('[role="tab"][data-state="active"]');
  return active?.textContent ?? "";
}

/** 浏览器里点按会先把焦点交给按钮，jsdom 不会，显式补上。 */
async function openPanel() {
  const trigger = openButton();
  trigger.focus();
  await act(async () => {
    trigger.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
  return trigger;
}

async function clickClose() {
  const close = closeButton(container);
  await act(async () => {
    close.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
}

/** 键盘事件必须从真实焦点元素派发：`document` 上的派发命中不了宽屏的 `onKeyDown`。 */
async function pressEscOn(target: EventTarget) {
  await act(async () => {
    target.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
  });
  await flush();
}

function hasAriaHiddenAncestor(element: Element): boolean {
  for (let node = element.parentElement; node; node = node.parentElement) {
    if (node.getAttribute("aria-hidden") === "true") return true;
  }
  return false;
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;
  globalThis.IntersectionObserver ??=
    IntersectionObserverStub as unknown as typeof IntersectionObserver;
  // Radix Dialog / DismissableLayer 用到的指针捕获与滚动 API，jsdom 未实现
  const proto = HTMLElement.prototype as unknown as Record<string, unknown>;
  proto.hasPointerCapture = () => false;
  proto.setPointerCapture = () => {};
  proto.releasePointerCapture = () => {};
  proto.scrollIntoView = () => {};

  viewport = installFlippableMatchMedia(1440);
  chatViewMounts.count = 0;

  __resetPecoStreamForTests();
  usePecoChatStore.setState({ messages: [], loaded: true, loading: false });
  // filter 上一次用例的值会跨用例泄漏
  usePecoArtifactStore.getState().__resetForTests();
  // loaded=true 让面板与页面都不再发起产物请求
  usePecoArtifactStore.setState({
    loaded: true,
    pages: [imageArtifact, documentArtifact],
    total: 2,
    nextOffset: 2,
  });

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
  __resetPecoStreamForTests();
  usePecoArtifactStore.getState().__resetForTests();
});

describe("PecoChatPage 产物面板在宽视口下与对话并排", () => {
  it("J1 未打开：没有 aside，也没有任何 dialog", async () => {
    await render();

    expect(aside()).toBeNull();
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    expect(sheetContent(document.body)).toBeNull();
    expect(openButton()).toBeTruthy();
  });

  it("J2 打开后是内联 aside：不是 dialog、不挂 sheet 内容与遮罩", async () => {
    await render();
    await openPanel();

    expect(aside()).not.toBeNull();
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    expect(sheetContent(document.body)).toBeNull();
    expect(document.querySelector('[data-slot="sheet-overlay"]')).toBeNull();
  });

  it("J4 并排容器宽度、圆角、边框、flex 列都在，且不是 fixed 定位", async () => {
    await render();
    await openPanel();

    const className = aside()?.className ?? "";
    expect(className).toContain("w-[480px]");
    expect(className).toContain("shrink-0");
    expect(className).toContain("border-l");
    expect(className).toContain("rounded-l-lg");
    expect(className).toContain("flex");
    expect(className).toContain("flex-col");
    expect(className).toContain("h-full");
    // 行向 flex 子项靠 stretch 决定高度，min-h-0 约束的是主轴，留它只会误导
    expect(className).not.toContain("min-h-0");
    // `relative` 是关闭按钮的定位祖先：删掉它，按钮会退到内容区容器的右上角
    // （离面板 480px）。jsdom 没有布局引擎，这里只能证明"配置还在"，定位效果
    // 必须靠浏览器量测（关闭按钮四边落在 aside 的 rect 内）—— 别把这条当效果判据
    expect(className).toContain("relative");
    expect(className).not.toContain("fixed");
  });

  it("J5 对话区没有被移出无障碍树", async () => {
    await render();
    await openPanel();

    const chat = container.querySelector('[data-testid="chat-view"]');
    if (!chat) throw new Error("chat view not found");
    expect(hasAriaHiddenAncestor(chat)).toBe(false);
  });

  it("J6 点「关闭」后 aside 卸载，焦点回到开关按钮", async () => {
    await render();
    const trigger = await openPanel();

    await clickClose();

    expect(aside()).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  // 焦点在面板内：从焦点元素派发 —— 宽屏 onKeyDown 挂在 aside 上，
  // 从 document 派发命中 0 次，会把正确实现测红
  it("J7a 焦点在面板内按 Esc：面板关闭", async () => {
    await render();
    await openPanel();
    const focused = document.activeElement;
    if (!focused || !aside()?.contains(focused)) {
      throw new Error("焦点没有落在面板内");
    }

    await pressEscOn(focused);

    expect(aside()).toBeNull();
  });

  it("J7b 焦点在面板外按 Esc：面板不关闭", async () => {
    await render();
    await openPanel();

    const trigger = openButton();
    trigger.focus();
    await pressEscOn(trigger);

    expect(aside()).not.toBeNull();
  });

  it("J8 关掉再打开：筛选仍是「图片」", async () => {
    await render();
    await openPanel();
    await activateTab(container, "图片");
    expect(activeTabText(container)).toContain("图片");

    await clickClose();
    expect(aside()).toBeNull();

    await openPanel();

    expect(activeTabText(container)).toContain("图片");
  });

  it("J11 打开→关闭、宽→窄→宽 之后 ChatView 只挂载过一次", async () => {
    await render();
    await openPanel();
    await clickClose();
    await openPanel();

    viewport.setViewportWidth(1280);
    await flush();
    viewport.setViewportWidth(1440);
    await flush();

    expect(chatViewMounts.count).toBe(1);
  });

  it("J12 开关按钮的 aria-controls 指向真实存在的容器，且不设 aria-haspopup", async () => {
    await render();
    await openPanel();

    const openBtn = openButton();
    const controls = openBtn.getAttribute("aria-controls");
    expect(controls).not.toBeNull();
    expect(document.getElementById(controls as string)).not.toBeNull();
    // 不是「不等于 dialog」：menu / listbox / 拼错都会漏过
    expect(openBtn).not.toHaveAttribute("aria-haspopup");
  });

  it("J12b 关闭态不带 aria-controls：面板没挂载，任何 id 都是悬空引用", async () => {
    await render();

    const openBtn = openButton();
    expect(openBtn).toHaveAttribute("aria-expanded", "false");
    expect(openBtn).not.toHaveAttribute("aria-controls");
  });

  it("J13 预览弹窗内按 Esc：只关预览，面板仍然打开", async () => {
    await render();
    await openPanel();
    const previewButton = container.querySelector<HTMLButtonElement>(
      '[aria-label="预览 照片.png"]',
    );
    if (!previewButton) throw new Error("预览按钮未渲染");
    await act(async () => {
      previewButton.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();

    const dialogContent = document.querySelector(
      '[data-slot="dialog-content"]',
    );
    if (!dialogContent) throw new Error("预览弹窗未打开");
    const inner = dialogContent.querySelector("button");
    if (!inner) throw new Error("预览弹窗内没有可派发的元素");

    await pressEscOn(inner);

    expect(document.querySelector('[data-slot="dialog-content"]')).toBeNull();
    expect(aside()).not.toBeNull();
  });

  it("J14 点开关打开后焦点落在面板内，从这里按 Esc 就关得掉", async () => {
    await render();
    await openPanel();

    const focused = document.activeElement;
    expect(focused).not.toBeNull();
    expect(aside()?.contains(focused as Node)).toBe(true);

    await pressEscOn(focused as Element);

    expect(aside()).toBeNull();
  });
});
