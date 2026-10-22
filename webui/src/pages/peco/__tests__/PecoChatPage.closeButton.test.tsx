// PecoChatPage 产物抽屉的关闭按钮：中文名、class 写法、关闭语义、未被移出 Tab 序列
//
// jsdom 没有排版与 Tailwind：这里只守「写法在、语义对」，尺寸/颜色/对比度的生效性
// 不在本文件射程内。防扩散用独立夹具：共享 sheet 的默认关闭按钮不得带上抽屉的样式。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { usePecoArtifactStore } from "@/stores/pecoArtifactStore";
import {
  __resetPecoStreamForTests,
  usePecoChatStore,
} from "@/stores/pecoChatStore";
import type { ArtifactItem } from "@/types/artifact";
import { PecoChatPage } from "../PecoChatPage";

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

const documentArtifact: ArtifactItem = {
  id: "a1",
  conversation_id: "c1",
  kind: "document",
  mime: "application/pdf",
  title: "报告.pdf",
  size_bytes: 1024,
  url: "/api/artifacts/a1/content",
  download_url: "/api/artifacts/a1/download",
  created_at: "2026-10-20 12:00:00",
  available: true,
};

class ResizeObserverStub {
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

async function render(node: ReactElement) {
  await act(async () => {
    root.render(node);
  });
  await flush();
}

function sheet(): Element | null {
  return document.querySelector('[data-slot="sheet-content"]');
}

function openButton(): HTMLButtonElement {
  const button = container.querySelector<HTMLButtonElement>(
    '[aria-label^="打开产物面板"]',
  );
  if (!button) throw new Error("sheet trigger not found");
  return button;
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

/**
 * 按钮没有 `aria-label`，名字就取自 sr-only 文本，故按 textContent 定位即按名字定位；
 * 内置那颗叫「Close」，不会在这里被误当成本次的按钮。
 */
function closeButton(): HTMLButtonElement {
  const found = Array.from(
    sheet()?.querySelectorAll<HTMLButtonElement>("button") ?? [],
  ).find((button) => button.textContent?.trim() === "关闭");
  if (!found) throw new Error("抽屉内没有名为「关闭」的按钮");
  return found;
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;
  // 恒 false = 窄视口：面板走 Sheet 分支。本文件只压这颗关闭按钮，不问断点形态
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  }));
  // Radix Dialog / DismissableLayer 用到的指针捕获与滚动 API，jsdom 未实现
  const proto = HTMLElement.prototype as unknown as Record<string, unknown>;
  proto.hasPointerCapture = () => false;
  proto.setPointerCapture = () => {};
  proto.releasePointerCapture = () => {};
  proto.scrollIntoView = () => {};

  __resetPecoStreamForTests();
  usePecoChatStore.setState({ messages: [], loaded: true, loading: false });
  usePecoArtifactStore.getState().__resetForTests();
  // loaded=true 让面板与页面都不再发起产物请求
  usePecoArtifactStore.setState({
    loaded: true,
    pages: [documentArtifact],
    total: 1,
    nextOffset: 1,
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

describe("PecoChatPage 产物抽屉的关闭按钮", () => {
  it("J1 抽屉内存在名为「关闭」的按钮", async () => {
    await render(<PecoChatPage />);
    await openSheet();

    expect(closeButton().textContent).toBe("关闭");
  });

  it("J2 按钮带 hover 红色 token", async () => {
    await render(<PecoChatPage />);
    await openSheet();

    expect(closeButton().className).toContain("hover:text-destructive");
  });

  it("J3 按钮带手型光标", async () => {
    await render(<PecoChatPage />);
    await openSheet();

    expect(closeButton().className).toContain("cursor-pointer");
  });

  it("J4 按钮命中区 32px、常态色不用更淡的 muted", async () => {
    await render(<PecoChatPage />);
    await openSheet();

    expect(closeButton().className).toContain("size-8");
    expect(closeButton().className).toContain("text-foreground/80");
  });

  it("J5 共享 Sheet 的默认关闭按钮未被污染", async () => {
    // `SheetContent` 自带 `SheetPortal`，没有 Root context 会抛错；
    // 不给 `open` 则 Presence 不挂载，夹具里量不到任何按钮
    await render(
      <Sheet open>
        <SheetContent aria-describedby={undefined}>
          <SheetTitle className="sr-only">x</SheetTitle>
        </SheetContent>
      </Sheet>,
    );

    // 默认关闭按钮没有 data-slot（sheet.tsx 直接渲染 SheetPrimitive.Close），
    // 只按结构定位：它是 SheetContent 的唯一直接子 button
    const content = document.querySelector('[data-slot="sheet-content"]');
    const close =
      content?.querySelector<HTMLButtonElement>(":scope > button") ?? null;
    if (!close) throw new Error("默认关闭按钮未渲染");
    expect(close.className).not.toContain("hover:text-destructive");
    expect(close.className).toContain("opacity-70");
  });

  it("J6 点按钮后抽屉关闭", async () => {
    await render(<PecoChatPage />);
    await openSheet();

    const close = closeButton();
    await act(async () => {
      close.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();

    expect(sheet()).toBeNull();
  });

  it("J7 关闭后焦点回到开关按钮", async () => {
    await render(<PecoChatPage />);
    const trigger = await openSheet();

    const close = closeButton();
    await act(async () => {
      close.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();

    expect(sheet()).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  // jsdom 不实现 Tab 导航，无法证明「按 Tab 真能走到」。这条只守「没被人显式
  // 移出序列」，不要把它当可达性证据 —— 那条只能靠浏览器。
  it("J8 按钮未被显式移出 Tab 序列", async () => {
    await render(<PecoChatPage />);
    await openSheet();

    const close = closeButton();
    // 判「在序列内」，不是判「没写属性」：显式 tabIndex={0} 同样是可达的
    expect(close.tabIndex).toBeGreaterThanOrEqual(0);
    expect(close.disabled).toBe(false);
  });
});
