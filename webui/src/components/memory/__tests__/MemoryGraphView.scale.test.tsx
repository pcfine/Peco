// 缩放可达性回归 —— viewBox 必须与容器像素 1:1，否则「适应内容」的压缩会吃掉
// 全部放大余量（旧实现：fit≈0.24 × MAX_SCALE=3 ⇒ 有效上限 ~0.7×，节点永远糊）。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryGraphView } from "../MemoryGraphView";
import { getMemoryGraph } from "@/api/memory";

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
const VP_W = 900;
const VP_H = 520;

let container: HTMLDivElement;
let root: Root;

/** 星形图：中心 + 30 个叶子 ⇒ 单列堆叠，高度远超视口（与真实大图同形）。 */
function tallGraph() {
  const nodes = [{ id: "hub", name: "中心", type: "Concept" }].concat(
    Array.from({ length: 30 }, (_, i) => ({
      id: `l${i}`,
      name: `实体${i}`,
      type: "Concept",
    })),
  );
  const edges = Array.from({ length: 30 }, (_, i) => ({
    source: "hub",
    target: `l${i}`,
    predicate: "关联",
    weight: 0.8,
  }));
  return { nodes, edges, truncated: false };
}

/** 屏幕上「相对 1:1」的实际倍率 = <g> 的 k ×（视口像素高 / viewBox 高）。 */
function effectiveScale(): number {
  const svg = container.querySelector("svg");
  if (!svg) throw new Error("svg not found");
  const vb = (svg.getAttribute("viewBox") ?? "").split(/\s+/).map(Number);
  const g = svg.querySelector("g[transform]");
  const m = /scale\(([\d.eE+-]+)\)/.exec(g?.getAttribute("transform") ?? "");
  const k = m ? Number(m[1]) : 1;
  return k * (VP_H / vb[3]);
}

function wheel(deltaY: number) {
  const svg = container.querySelector("svg");
  if (!svg) throw new Error("svg not found");
  act(() => {
    svg.dispatchEvent(new WheelEvent("wheel", { deltaY, bubbles: true }));
  });
}

async function render() {
  await act(async () => {
    root.render(<MemoryGraphView />);
  });
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;
  // jsdom 不做布局：显式给容器一个像素尺寸，走「真实浏览器」分支。
  Object.defineProperty(HTMLElement.prototype, "clientWidth", {
    configurable: true,
    get: () => VP_W,
  });
  Object.defineProperty(HTMLElement.prototype, "clientHeight", {
    configurable: true,
    get: () => VP_H,
  });
  GRAPH.mockReset();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  delete (HTMLElement.prototype as unknown as Record<string, unknown>)
    .clientWidth;
  delete (HTMLElement.prototype as unknown as Record<string, unknown>)
    .clientHeight;
});

describe("MemoryGraphView 缩放可达性", () => {
  it("大图初始「适应内容」(<1×)，但可继续放大到 ≥1:1（节点可读）", async () => {
    GRAPH.mockResolvedValue(tallGraph());
    await render();

    // 适应内容后整图压进视口 ⇒ 有效倍率远小于 1（节点只有几十像素）
    const fit = effectiveScale();
    expect(fit).toBeGreaterThan(0);
    expect(fit).toBeLessThan(0.6);

    // 连续滚轮放大：必须突破 1:1 —— 旧实现被 fit×3 卡在 <1，节点永远看不清
    for (let i = 0; i < 30; i += 1) wheel(-100);
    expect(effectiveScale()).toBeGreaterThan(1);

    // 节点渲染尺寸 = 140 × 有效倍率 ⇒ 至少 140px 宽（字号 ≥12px）
    expect(140 * effectiveScale()).toBeGreaterThanOrEqual(140);
  });

  it("放大有上限、缩小有下限，均不产生 NaN / Infinity", async () => {
    GRAPH.mockResolvedValue(tallGraph());
    await render();

    for (let i = 0; i < 60; i += 1) wheel(-100);
    const up = effectiveScale();
    expect(Number.isFinite(up)).toBe(true);
    expect(up).toBeLessThanOrEqual(5 + 1e-9);

    for (let i = 0; i < 200; i += 1) wheel(100);
    const down = effectiveScale();
    expect(Number.isFinite(down)).toBe(true);
    expect(down).toBeGreaterThan(0);
  });
});
