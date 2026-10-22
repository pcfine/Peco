// PecoChatPage 的产物抽屉：关闭后焦点回到开关按钮
//
// 抽屉不是 Radix Trigger 打开的，Radix 手里没有 triggerRef —— 关闭时若不自
// 己还，默认归还被 preventDefault 掉，焦点落到 body。这里只压这一条契约，
// 聊天流整体换成占位（它的内部行为不在本文件射程内）。

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
 * 抽屉里得有产物：全部分类的 tab 都置灰时 Radix 的初始焦点会挑 tabpanel 而不是
 * tab，这里要压的恰是"焦点落在 tab 上"。
 */
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
  if (!button) throw new Error("sheet trigger not found");
  return button;
}

function sheet(): Element | null {
  return document.querySelector('[data-slot="sheet-content"]');
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

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;
  // 恒 false = 窄视口：面板走 Sheet 分支，初始聚焦与归还都归 Radix
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

describe("PecoChatPage 产物抽屉的焦点归还", () => {
  it("点关闭按钮后焦点回到开关按钮", async () => {
    await render();
    const trigger = await openSheet();
    // 打开后焦点进了抽屉的 tab 序列，才有"归还"可言。不能只断言"不再是开关
    // 按钮"：面板根容器 tabIndex={-1} 且可被程序化聚焦，焦点落它同样为真
    const active = document.activeElement;
    expect(active).toHaveAttribute("role", "tab");
    expect(sheet()?.contains(active)).toBe(true);

    const close = Array.from(sheet()?.querySelectorAll("button") ?? []).find(
      (button) => button.textContent?.trim() === "关闭",
    );
    if (!close) throw new Error("sheet close button not found");
    await act(async () => {
      close.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();

    // 变异点：SheetContent 不接管 onCloseAutoFocus → Radix 没有 triggerRef，
    // 默认归还被跳过 → 焦点落 body
    expect(sheet()).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  /**
   * 遮罩关闭：Radix 的 DismissableLayer 在 pointerdown 上判定"点在层外"，但要等到
   * 随后的 click 才真正 dismiss（`deferPointerDownOutside`），故三个事件依次派发。
   */
  it("点遮罩关闭后焦点同样回到开关按钮", async () => {
    await render();
    const trigger = await openSheet();
    const overlay = document.querySelector('[data-slot="sheet-overlay"]');
    if (!overlay) throw new Error("sheet overlay not found");

    await act(async () => {
      for (const type of ["pointerdown", "pointerup", "click"]) {
        overlay.dispatchEvent(
          new MouseEvent(type, { bubbles: true, button: 0 }),
        );
      }
    });
    await flush();

    expect(sheet()).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  /**
   * 新用户第一次打开抽屉的真实状态：没有任何可用 tab，Radix 的初始焦点退到
   * 内容区（tabpanel），读屏据此念出空状态。种子只在本用例里换成空面板 ——
   * 其余用例要压在可用 tab 上。
   */
  it("空面板：打开后焦点落在抽屉内的 tabpanel，关闭后回开关按钮", async () => {
    usePecoArtifactStore.setState({
      loaded: true,
      pages: [],
      total: 0,
      nextOffset: 0,
    });
    await render();
    const trigger = await openSheet();

    // role 只说明落点是内容区，说明不了落点上有什么可念；空状态文案一起钉住，
    // 才压住"没有可用 tab 时把空状态念给读屏"这条理由
    const active = document.activeElement;
    expect(active).toHaveAttribute("role", "tabpanel");
    expect(sheet()?.contains(active)).toBe(true);
    expect(active?.textContent).toContain("还没有产物");

    await act(async () => {
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      );
    });
    await flush();

    expect(sheet()).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("按 Escape 关闭后焦点同样回到开关按钮", async () => {
    await render();
    const trigger = await openSheet();

    await act(async () => {
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      );
    });
    await flush();

    expect(sheet()).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });
});
