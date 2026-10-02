// MemoryGraphView 组件测试 —— 渲染 / 空态 / 错误重试 / 选中面板 / 单节点（design §5.3 F9）

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryGraphView } from "../MemoryGraphView";
import { getMemoryGraph } from "@/api/memory";
import type { MemoryGraphResponse } from "@/types/memory";

vi.mock("@/api/memory", () => ({
  getMemoryGraph: vi.fn(),
  listMemoryDocuments: vi.fn(),
}));

// jsdom 未实现 ResizeObserver，radix ScrollArea 直接引用它。
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

const GRAPH = vi.mocked(getMemoryGraph);

const THREE_NODES: MemoryGraphResponse = {
  nodes: [
    { id: "n1", name: "小C" },
    { id: "n2", name: "chen" },
    { id: "n3", name: "男" },
  ],
  edges: [
    { source: "n1", target: "n2", predicate: "朋友", weight: 0.95 },
    { source: "n1", target: "n3", predicate: "性别", weight: 0.9 },
  ],
  truncated: false,
};

let container: HTMLDivElement;
let root: Root;

/** 挂载视图并等待首屏请求落地。 */
async function renderView() {
  await act(async () => {
    root.render(<MemoryGraphView />);
  });
  await flush();
}

/** 让已 resolve 的 promise 链跑完。 */
async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function buttonByText(text: string): HTMLButtonElement {
  const btn = Array.from(container.querySelectorAll("button")).find((b) =>
    b.textContent?.includes(text),
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn as HTMLButtonElement;
}

function panel(): HTMLElement | null {
  return container.querySelector('[data-testid="memory-selected-panel"]');
}

function clickNode(id: string) {
  const el = container.querySelector(`[data-testid="memory-node-${id}"]`);
  if (!el) throw new Error(`node not found: ${id}`);
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;

  GRAPH.mockReset();

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("MemoryGraphView", () => {
  it("渲染 3 个节点名与 2 条谓词", async () => {
    GRAPH.mockResolvedValue(THREE_NODES);

    await renderView();

    expect(container.querySelector("svg")).not.toBeNull();
    const text = container.textContent ?? "";
    expect(text).toContain("小C");
    expect(text).toContain("chen");
    expect(text).toContain("男");
    expect(text).toContain("朋友");
    expect(text).toContain("性别");
  });

  it("0 节点时显示空状态且不渲染 svg", async () => {
    GRAPH.mockResolvedValue({ nodes: [], edges: [], truncated: false });

    await renderView();

    expect(container.textContent).toContain("还没有记忆图谱");
    expect(container.querySelector("svg")).toBeNull();
  });

  it("API 失败时显示错误与重试，点击后重新拉取", async () => {
    GRAPH.mockRejectedValue(new Error("boom"));

    await renderView();

    expect(container.textContent).toContain("boom");
    expect(buttonByText("重试")).toBeTruthy();
    expect(GRAPH).toHaveBeenCalledTimes(1);

    await act(async () => {
      buttonByText("重试").click();
    });
    await flush();

    expect(GRAPH).toHaveBeenCalledTimes(2);
  });

  it("点击节点显示名与直接关系；点空白 / Esc 收起", async () => {
    GRAPH.mockResolvedValue(THREE_NODES);

    await renderView();
    expect(panel()).toBeNull();

    clickNode("n1");
    const p = panel();
    expect(p).not.toBeNull();
    const panelText = p?.textContent ?? "";
    expect(panelText).toContain("小C");
    expect(panelText).toContain("朋友");
    expect(panelText).toContain("chen");
    expect(panelText).toContain("性别");
    expect(panelText).toContain("男");

    // 点空白（事件目标就是 <svg>）收起
    const svg = container.querySelector("svg");
    if (!svg) throw new Error("svg not found");
    act(() => {
      svg.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(panel()).toBeNull();

    // 再次选中后用 Esc 收起
    clickNode("n2");
    expect(panel()).not.toBeNull();
    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });
    expect(panel()).toBeNull();
  });

  it("单节点 / 0 边渲染不抛错（包围盒除零保护）", async () => {
    GRAPH.mockResolvedValue({
      nodes: [{ id: "only", name: "孤立点" }],
      edges: [],
      truncated: false,
    });

    await renderView();

    expect(container.querySelector("svg")).not.toBeNull();
    expect(container.textContent).toContain("孤立点");
  });
});
