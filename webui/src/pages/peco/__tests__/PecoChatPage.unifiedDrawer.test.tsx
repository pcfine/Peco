// PecoChatPage 的产物面板在窄视口（<1440）下：覆盖层抽屉
//
// 宽视口并排形态在 PecoChatPage.widePanel.test.tsx，跨断点切换在 resize 文件。
// 本文件只守窄屏：遮罩覆盖层、抽屉口径、关闭路径与筛选状态留在 store —— 抽屉关闭
// 即卸载，状态若留在组件内，每次重开都会退回「全部」。
//
// 编号约定：J3 是覆盖层形态判据、J10 是断点门槛判据；窄屏专属断言沿用本文件
// 原有序列（J1/J4/J5/J6/J7），关闭侧补 J3b。

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
import { installFlippableMatchMedia } from "./flippableMatchMedia";

vi.mock("@/components/chat/ChatView", async (importOriginal) => {
  const [{ createElement }, actual] = await Promise.all([
    import("react"),
    importOriginal<typeof import("@/components/chat/ChatView")>(),
  ]);
  return {
    ...actual,
    ChatView: ({ headerActions }: { headerActions?: unknown }) =>
      createElement(
        "div",
        { "data-testid": "chat-view" },
        headerActions as never,
      ),
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

/**
 * 图片与文档各一条：`TabsTrigger` 在分类计数为 0 时置灰，只种文档时「图片」
 * 点不动，J7 会误红。
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

/** 抽屉 Portal 到 body，作用域是 `document.body`（宽屏 aside 内联在 `container` 里）。 */
function sheet(rootNode: ParentNode): Element | null {
  return rootNode.querySelector('[data-slot="sheet-content"]');
}

function dialog(rootNode: ParentNode): Element | null {
  return rootNode.querySelector("#artifact-panel-sheet");
}

/** 浏览器里点按会先把焦点交给按钮，jsdom 不会，显式补上。 */
async function openSheet() {
  const trigger = openButton();
  trigger.focus();
  await act(async () => {
    trigger.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
  return trigger;
}

/** 按钮没有 `aria-label`，名字取自 sr-only 文本，按 textContent 即按名字定位。 */
function closeButton(rootNode: ParentNode): HTMLButtonElement {
  const found = Array.from(
    rootNode.querySelectorAll<HTMLButtonElement>("button"),
  ).find((button) => button.textContent?.trim() === "关闭");
  if (!found) throw new Error("抽屉内没有名为「关闭」的按钮");
  return found;
}

async function clickClose() {
  const close = closeButton(document.body);
  await act(async () => {
    close.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
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

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;
  globalThis.IntersectionObserver ??=
    IntersectionObserverStub as unknown as typeof IntersectionObserver;
  // 1280 是"侧栏展开时最窄的一档"：并排形态在这里会把对话区压到 480，必须降级成覆盖层
  installFlippableMatchMedia(1280);
  // Radix Dialog / DismissableLayer 用到的指针捕获与滚动 API，jsdom 未实现
  const proto = HTMLElement.prototype as unknown as Record<string, unknown>;
  proto.hasPointerCapture = () => false;
  proto.setPointerCapture = () => {};
  proto.releasePointerCapture = () => {};
  proto.scrollIntoView = () => {};

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

describe("PecoChatPage 产物面板在窄视口下是覆盖层抽屉", () => {
  it("J1 未打开时没有常驻面板：无 aside、无「产物 ·」文案，开关按钮在", async () => {
    await render();

    expect(container.querySelector("aside")).toBeNull();
    expect(container.textContent).not.toContain("产物 ·");
    expect(openButton()).toBeTruthy();
  });

  it("J3 打开后是 dialog 覆盖层：有遮罩，没有并排容器", async () => {
    await render();
    await openSheet();

    expect(dialog(document.body)).not.toBeNull();
    expect(sheet(document.body)).not.toBeNull();
    expect(
      document.querySelector('[data-slot="sheet-overlay"]'),
    ).not.toBeNull();
    expect(document.querySelector('[data-slot="artifact-drawer"]')).toBeNull();
  });

  it("J3b 点抽屉内「关闭」后抽屉卸载", async () => {
    await render();
    await openSheet();

    await clickClose();

    expect(dialog(document.body)).toBeNull();
  });

  it("J4 抽屉宽 480、左圆角、不被 sm:max-w-sm 压回", async () => {
    await render();
    await openSheet();

    const className = dialog(document.body)?.className ?? "";
    expect(className).toContain("rounded-l-lg");
    expect(className).toContain("480px");
    // `SheetContent` 自带 `sm:max-w-sm`(384)，少了这条会被压回 384
    expect(className).toContain("sm:max-w-none");
  });

  it("J5 面板内边距固定为抽屉口径，不再带常驻侧栏的 py-3", async () => {
    await render();
    await openSheet();

    const panel = dialog(document.body)?.querySelector(
      '[aria-label="对话产物"]',
    );
    const className = panel?.className ?? "";

    expect(className).toContain("px-4");
    expect(className).not.toContain("py-3");
  });

  it("J6 关闭后焦点回到开关按钮", async () => {
    await render();
    const trigger = await openSheet();

    await clickClose();

    expect(document.activeElement).toBe(trigger);
  });

  it("J7 关掉再打开：筛选仍是「图片」", async () => {
    await render();
    await openSheet();
    await activateTab(document.body, "图片");
    expect(activeTabText(document.body)).toContain("图片");

    await clickClose();
    expect(sheet(document.body)).toBeNull();

    await openSheet();

    expect(activeTabText(document.body)).toContain("图片");
  });

  // 断点若放到 1280，这一档会并排：对话区只剩 480，比抽屉还挤
  it("J10 1280 + 侧栏展开：仍是覆盖层，不为并排", async () => {
    // 视口 1280 由 beforeEach 的 fake 安装，这里不需要翻转：这一档要守的是
    // "1440 以下不并排"，不是"从宽屏缩回来"
    await render();
    await openSheet();

    expect(sheet(document.body)).not.toBeNull();
    expect(document.querySelector('[data-slot="artifact-drawer"]')).toBeNull();
  });
});
