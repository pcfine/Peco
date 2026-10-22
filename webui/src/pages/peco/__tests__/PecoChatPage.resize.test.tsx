// PecoChatPage 的产物面板跨断点切换（J9）：形态换、面板不关。
//
// 单一 open 状态是这条契约的前提：若 open 状态跟着断点分家，窗口一缩就会把用户
// 正看着的面板收起来。翻转通道由可翻转的 matchMedia fake 提供。

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

function aside(): HTMLElement | null {
  return container.querySelector<HTMLElement>('[data-slot="artifact-drawer"]');
}

function sheet(rootNode: ParentNode): Element | null {
  return rootNode.querySelector('[data-slot="sheet-content"]');
}

/** 不返回节点：调用方一律用 `openButton()` 现取，避免拿到跨越形态切换的旧引用。 */
async function openPanel() {
  const trigger = openButton();
  trigger.focus();
  await act(async () => {
    trigger.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
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

  __resetPecoStreamForTests();
  usePecoChatStore.setState({ messages: [], loaded: true, loading: false });
  usePecoArtifactStore.getState().__resetForTests();
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

describe("PecoChatPage 产物面板跨断点", () => {
  it("J9 打开状态下宽→窄→宽：面板始终打开，并排容器与抽屉互换", async () => {
    await render();
    await openPanel();
    expect(aside()).not.toBeNull();
    expect(sheet(document.body)).toBeNull();

    viewport.setViewportWidth(1280);
    await flush();
    expect(aside()).toBeNull();
    expect(sheet(document.body)).not.toBeNull();

    // 每次翻转后重新取按钮：形态切换若把开关一起重建，翻转前捕获的旧节点会
    // 脱离文档却仍带着旧属性，`toHaveAttribute` 照样绿 —— 那只能证明"曾经开过"。
    // 两档都要各自断言：仅看最后一次，窄屏那边的 aria-expanded 就没人守了。
    const narrowTrigger = openButton();
    expect(narrowTrigger).toHaveAttribute("aria-expanded", "true");
    expect(narrowTrigger).toHaveAttribute(
      "aria-controls",
      "artifact-panel-sheet",
    );

    viewport.setViewportWidth(1440);
    await flush();
    expect(aside()).not.toBeNull();
    expect(sheet(document.body)).toBeNull();

    const wideTrigger = openButton();
    expect(wideTrigger).toHaveAttribute("aria-expanded", "true");
    expect(wideTrigger).toHaveAttribute(
      "aria-controls",
      "artifact-panel-aside",
    );
  });
});
