// MemoryGraphView 删除 —— 选中面板入口 / 边数文案 / 局部移除 / 404 兜底

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryGraphView } from "../MemoryGraphView";
import { deleteMemoryNode, getMemoryGraph } from "@/api/memory";
import type { MemoryGraphResponse } from "@/types/memory";
import { toast } from "sonner";

vi.mock("@/api/memory", () => ({
  getMemoryGraph: vi.fn(),
  listMemoryDocuments: vi.fn(),
  deleteMemoryNode: vi.fn(),
}));

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn(), warning: vi.fn() },
}));

const GRAPH = vi.mocked(getMemoryGraph);
const DEL = vi.mocked(deleteMemoryNode);

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

const THREE_NODES: MemoryGraphResponse = {
  nodes: [
    { id: "n1", name: "小C", type: "Person" },
    { id: "n2", name: "chen", type: "Person" },
    { id: "n3", name: "rust", type: "Technology" },
  ],
  edges: [
    { source: "n1", target: "n2", predicate: "朋友", weight: 0.95 },
    { source: "n1", target: "n3", predicate: "喜欢", weight: 0.9 },
  ],
  truncated: false,
};

/** 空名节点：后端允许 `name` 为空串。 */
const UNNAMED: MemoryGraphResponse = {
  nodes: [{ id: "entity:Entity:x:1234567890ab", name: "", type: "Other" }],
  edges: [],
  truncated: false,
};

let container: HTMLDivElement;
let root: Root;

async function renderView() {
  await act(async () => {
    root.render(<MemoryGraphView />);
  });
  await flush();
}

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function clickNode(id: string) {
  const el = container.querySelector(`[data-testid="memory-node-${id}"]`);
  if (!el) throw new Error(`node not found: ${id}`);
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function panel(): HTMLElement | null {
  return container.querySelector('[data-testid="memory-selected-panel"]');
}

function openDelete() {
  const btn = container.querySelector('[data-testid="memory-node-delete"]');
  if (!btn) throw new Error("node delete button not found");
  act(() => {
    (btn as HTMLElement).dispatchEvent(
      new MouseEvent("click", { bubbles: true }),
    );
  });
}

function dialog(): HTMLElement | null {
  return document.body.querySelector(
    '[data-testid="memory-node-delete-dialog"]',
  );
}

function confirmButton(): HTMLButtonElement {
  const el = document.body.querySelector(
    '[data-testid="memory-node-delete-dialog-confirm"]',
  );
  if (!el) throw new Error("confirm button not found");
  return el as HTMLButtonElement;
}

function apiError(status: number, details: string) {
  return Object.assign(new Error("request failed"), {
    isAxiosError: true,
    response: { status, data: { error: "x", details } },
  });
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;

  GRAPH.mockReset();
  DEL.mockReset();
  vi.mocked(toast.error).mockReset();
  vi.mocked(toast.success).mockReset();
  vi.mocked(toast.warning).mockReset();

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("MemoryGraphView 删除", () => {
  it("J7 面板入口 → 弹层含节点名与边数 → 确认后按 id 删除并局部移除", async () => {
    GRAPH.mockResolvedValue(THREE_NODES);
    DEL.mockResolvedValue({
      entity_id: "n1",
      removed_edges: 2,
      removed_nodes: 1,
      audit_id: 5,
    });

    await renderView();
    clickNode("n1");
    expect(panel()).not.toBeNull();

    openDelete();
    await flush();
    expect(dialog()?.textContent).toContain("小C");
    expect(dialog()?.textContent).toContain("2 条关联关系");

    await act(async () => {
      confirmButton().click();
    });
    await flush();

    expect(DEL).toHaveBeenCalledTimes(1);
    expect(DEL).toHaveBeenCalledWith("n1");
    expect(
      container.querySelector('[data-testid="memory-node-n1"]'),
    ).toBeNull();
    expect(panel()).toBeNull();
    expect(dialog()).toBeNull();
  });

  it("J8 空名节点用兜底显示名，调用参数是 id 而非名称", async () => {
    GRAPH.mockResolvedValue(UNNAMED);
    DEL.mockResolvedValue({
      entity_id: "entity:Entity:x:1234567890ab",
      removed_edges: 0,
      removed_nodes: 1,
      audit_id: 6,
    });

    await renderView();
    clickNode("entity:Entity:x:1234567890ab");
    openDelete();
    await flush();

    expect(dialog()?.textContent).toContain("未命名节点（1234567890ab）");

    await act(async () => {
      confirmButton().click();
    });
    await flush();

    expect(DEL).toHaveBeenCalledWith("entity:Entity:x:1234567890ab");
  });

  it("J9 404 → warning toast + 清选中 + 重新拉图", async () => {
    GRAPH.mockResolvedValue(THREE_NODES);
    DEL.mockRejectedValue(apiError(404, "实体节点不存在或已被删除"));

    await renderView();
    clickNode("n1");
    openDelete();
    await flush();
    await act(async () => {
      confirmButton().click();
    });
    await flush();

    expect(vi.mocked(toast.warning)).toHaveBeenCalledWith("该实体节点已被删除");
    expect(panel()).toBeNull();
    expect(dialog()).toBeNull();
    expect(GRAPH).toHaveBeenCalledTimes(2);
  });
});
