// dagre 布局抛错时的兜底路径 —— 布局异常绝不允许升级为整页白屏（回归）

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryGraphView } from "../MemoryGraphView";
import { getMemoryGraph } from "@/api/memory";

// 仅替换 layout，保留真实 graphlib（Graph 构造仍需可用）。
vi.mock("@dagrejs/dagre", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@dagrejs/dagre")>();
  return {
    ...actual,
    layout: () => {
      throw new Error(
        "Not possible to find intersection inside of the rectangle",
      );
    },
  };
});

vi.mock("@/api/memory", () => ({
  getMemoryGraph: vi.fn(),
  listMemoryDocuments: vi.fn(),
}));

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

const GRAPH = vi.mocked(getMemoryGraph);

let container: HTMLDivElement;
let root: Root;

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

describe("MemoryGraphView dagre 兜底", () => {
  it("layout 抛错时仍渲染全部节点与谓词（网格兜底，不白屏）", async () => {
    GRAPH.mockResolvedValue({
      nodes: [
        { id: "a", name: "peco", type: "SoftwareProject" },
        { id: "b", name: "Docling", type: "Technology" },
      ],
      edges: [{ source: "a", target: "b", predicate: "使用", weight: 0.8 }],
      truncated: false,
    });

    await act(async () => {
      root.render(<MemoryGraphView />);
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelector("svg")).not.toBeNull();
    const text = container.textContent ?? "";
    expect(text).toContain("peco");
    expect(text).toContain("Docling");
    expect(text).toContain("使用");
    expect(container.querySelectorAll("[data-testid^='memory-node-']")).toHaveLength(2);
  });
});
