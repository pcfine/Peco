// MemoryGraphView 图例 / 类型过滤 / 关系组着色
// 关键约束：过滤走可见性掩码，重跑 dagre 会让大图跳变并白付一次布局开销

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { layout } from "@dagrejs/dagre";
import { MemoryGraphView } from "../MemoryGraphView";
import { getMemoryGraph } from "@/api/memory";
import type { MemoryGraphResponse } from "@/types/memory";

// 只包一层 spy 保留真实布局：既能断言「布局算了几次」，又走真实代码路径。
vi.mock("@dagrejs/dagre", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@dagrejs/dagre")>();
  return { ...actual, layout: vi.fn(actual.layout) };
});

vi.mock("@/api/memory", () => ({
  getMemoryGraph: vi.fn(),
  listMemoryDocuments: vi.fn(),
}));

/** 兜底灰 —— 闭集外类型 / 未映射关系组 / 闭集外关系组共用，与组件内常量逐字一致。 */
const FALLBACK_HEX = "#a1a1aa";

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

const GRAPH = vi.mocked(getMemoryGraph);
const LAYOUT = vi.mocked(layout);

/** 4 节点：已知类型 ×3 + 闭集外类型 ×1；边覆盖已知组 / 未映射 / 空谓词。 */
const GRAPH_DATA: MemoryGraphResponse = {
  nodes: [
    { id: "p1", name: "peco", type: "SoftwareProject" },
    { id: "c1", name: "sanitize_kb_name", type: "CodeSymbol" },
    { id: "k1", name: "@private_memory", type: "KnowledgeBase" },
    { id: "x1", name: "说不清的东西", type: "NotATypeFromBackend" },
  ],
  edges: [
    {
      source: "p1",
      target: "c1",
      predicate: "使用",
      weight: 0.8,
      group: "使用依赖",
    },
    {
      source: "p1",
      target: "k1",
      predicate: "记录于",
      weight: 0.7,
      group: "定义记录",
    },
    { source: "x1", target: "p1", predicate: "谁知道呢", weight: 0.5 },
    { source: "x1", target: "c1", predicate: "", weight: 0.4 },
  ],
  truncated: false,
};

let container: HTMLDivElement;
let root: Root;

async function renderView() {
  await act(async () => {
    root.render(<MemoryGraphView />);
  });
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function legendRow(testId: string): HTMLElement {
  const el = container.querySelector<HTMLElement>(`[data-testid="${testId}"]`);
  if (!el) throw new Error(`legend row not found: ${testId}`);
  return el;
}

/** 节点类型行的开关按钮。 */
function legendTypeButton(key: string): HTMLButtonElement {
  return legendRow(`memory-legend-type-${key}`) as HTMLButtonElement;
}

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function nodeGroup(id: string): SVGGElement {
  const el = container.querySelector(`[data-testid="memory-node-${id}"]`);
  if (!el) throw new Error(`node not found: ${id}`);
  return el as SVGGElement;
}

/** 每个节点的 rect 坐标（布局产物；过滤不得改动它们）。 */
function nodePositions(): string[] {
  return Array.from(
    container.querySelectorAll("[data-testid^='memory-node-'] rect"),
  ).map((r) => `${r.getAttribute("x")},${r.getAttribute("y")}`);
}

/** 边 = 带 marker-end 的 path；逐条给出「是否被掩码隐藏 / 是否有谓词标签」。 */
function edgeStates(): { hidden: boolean; labelled: boolean }[] {
  return Array.from(container.querySelectorAll("svg path[marker-end]")).map(
    (p) => ({
      hidden:
        (p.parentElement as Element | null)?.getAttribute("display") === "none",
      labelled: Boolean(p.parentElement?.querySelector("text")),
    }),
  );
}

/** jsdom 把 style 里的颜色规范化成 `rgb()`，与 SVG 属性上的 hex 对齐用。 */
function toRgb(hex: string): string {
  const n = Number.parseInt(hex.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
}

/** 图例某行的色块颜色（`rgb()` 形式）。 */
function swatch(testId: string): string {
  const el = legendRow(testId).querySelector("span");
  return (el as HTMLElement | null)?.style.backgroundColor ?? "";
}

/** 边 path 的描边色，按绘制顺序。 */
function edgeStrokes(): string[] {
  return Array.from(container.querySelectorAll("svg path[marker-end]")).map(
    (p) => p.getAttribute("stroke") ?? "",
  );
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;

  GRAPH.mockReset();
  LAYOUT.mockClear();
  GRAPH.mockResolvedValue(GRAPH_DATA);

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("MemoryGraphView 图例", () => {
  it("类型与关系组各一块，色块 + 计数取自当前 nodes / edges", async () => {
    await renderView();

    expect(legendRow("memory-graph-legend")).toBeTruthy();
    expect(container.textContent).toContain("节点类型");
    expect(container.textContent).toContain("关系组");

    // 计数：SoftwareProject 1、CodeSymbol 1、KnowledgeBase 1、未知 1、其余 0
    expect(legendRow("memory-legend-type-SoftwareProject").textContent).toBe(
      "SoftwareProject1",
    );
    expect(legendRow("memory-legend-type-CodeSymbol").textContent).toBe(
      "CodeSymbol1",
    );
    expect(legendRow("memory-legend-type-Person").textContent).toBe("Person0");
    expect(legendRow("memory-legend-type-__unknown_type__").textContent).toBe(
      "未知1",
    );
    // Other 是闭集成员，不是「未知」行
    expect(legendRow("memory-legend-type-Other").textContent).toBe("Other0");

    // 关系组：使用依赖 1、定义记录 1、未映射 2（group 缺省 + predicate 空）
    expect(legendRow("memory-legend-group-使用依赖").textContent).toBe(
      "使用依赖1",
    );
    expect(legendRow("memory-legend-group-定义记录").textContent).toBe(
      "定义记录1",
    );
    expect(legendRow("memory-legend-group-__unmapped__").textContent).toBe(
      "未映射2",
    );
    expect(legendRow("memory-legend-group-偏好").textContent).toBe("偏好0");

    // 每行都有色块
    expect(
      legendRow("memory-legend-type-SoftwareProject")
        .querySelector("span")
        ?.getAttribute("style"),
    ).toContain("background-color");
  });

  it("闭集外类型仍渲染节点、并归到「未知」，空谓词不画标签也不抛错", async () => {
    await renderView();

    // 未知类型的节点照常渲染（灰，不退化成空白）
    expect(nodeGroup("x1")).toBeTruthy();
    expect(container.textContent).toContain("说不清的东西");

    // 4 条边都画出来，但空谓词那条没有标签
    const states = edgeStates();
    expect(states).toHaveLength(4);
    expect(states.filter((s) => s.labelled)).toHaveLength(3);
  });

  it("点图例开关只改可见性掩码：不重跑布局、节点位置不变、悬空边一起隐藏", async () => {
    await renderView();

    expect(LAYOUT).toHaveBeenCalledTimes(1);
    const before = nodePositions();
    expect(before).toHaveLength(4);

    const row = legendTypeButton("CodeSymbol");
    expect(row.getAttribute("aria-pressed")).toBe("true");

    click(row);

    // 掩码生效：CodeSymbol 节点与其两条悬空边隐藏
    expect(row.getAttribute("aria-pressed")).toBe("false");
    expect(nodeGroup("c1").getAttribute("display")).toBe("none");
    expect(nodeGroup("p1").getAttribute("display")).toBeNull();
    expect(edgeStates().filter((s) => s.hidden)).toHaveLength(2);

    // 关键回归：不重跑 dagre、不让节点跳变（过滤绝不许触发重新布局）
    expect(LAYOUT).toHaveBeenCalledTimes(1);
    expect(nodePositions()).toEqual(before);

    // 图例计数仍是全图口径（掩码不改数据）
    expect(legendRow("memory-legend-type-CodeSymbol").textContent).toBe(
      "CodeSymbol1",
    );

    // 再点一次恢复
    click(row);
    expect(nodeGroup("c1").getAttribute("display")).toBeNull();
    expect(edgeStates().filter((s) => s.hidden)).toHaveLength(0);
    expect(LAYOUT).toHaveBeenCalledTimes(1);
  });

  it("节点按类型、边按关系组着色，图例色块与图元同色", async () => {
    await renderView();

    const rect = (id: string) => nodeGroup(id).querySelector("rect");
    const stroke = (id: string) => rect(id)?.getAttribute("stroke") ?? "";

    // 三个已知类型三色，且填充与描边同色（填充透明度由 fillOpacity 控制）
    expect(new Set([stroke("p1"), stroke("c1"), stroke("k1")]).size).toBe(3);
    expect(rect("p1")?.getAttribute("fill")).toBe(stroke("p1"));
    expect(swatch("memory-legend-type-SoftwareProject")).toBe(
      toRgb(stroke("p1")),
    );

    // 闭集外类型走兜底灰，与图例「未知」色块同色
    expect(swatch("memory-legend-type-__unknown_type__")).toBe(
      toRgb(stroke("x1")),
    );

    // 边：两条未映射同色，已知组各一色，色块与边同色；箭头也随组换
    const strokes = edgeStrokes();
    expect(strokes).toHaveLength(4);
    expect(strokes[2]).toBe(strokes[3]);
    expect(new Set(strokes).size).toBe(3);
    expect(swatch("memory-legend-group-使用依赖")).toBe(toRgb(strokes[0]));
    expect(swatch("memory-legend-group-定义记录")).toBe(toRgb(strokes[1]));
    expect(swatch("memory-legend-group-__unmapped__")).toBe(toRgb(strokes[2]));
    const markers = Array.from(
      container.querySelectorAll("svg path[marker-end]"),
    ).map((p) => p.getAttribute("marker-end"));
    expect(new Set(markers).size).toBe(3);
  });

  it("选中节点时关系面板给出原文谓词 + 关系组（未映射不加组标签）", async () => {
    await renderView();

    click(nodeGroup("p1"));

    const panel = container.querySelector(
      '[data-testid="memory-selected-panel"]',
    );
    const text = panel?.textContent ?? "";
    expect(text).toContain("使用");
    expect(text).toContain("使用依赖");
    expect(text).toContain("记录于");
    expect(text).toContain("定义记录");
    expect(text).toContain("sanitize_kb_name");
    // group 缺省的边只有原文谓词，不编造组名
    expect(text).toContain("谁知道呢");
    expect(text).not.toContain("未映射");
  });

  it("闭集外的非空 group 自成一行，不并入「未映射」", async () => {
    GRAPH.mockResolvedValue({
      nodes: [
        { id: "p1", name: "peco", type: "SoftwareProject" },
        { id: "c1", name: "cargo", type: "CodeSymbol" },
        { id: "k1", name: "@private_memory", type: "KnowledgeBase" },
      ],
      edges: [
        {
          source: "p1",
          target: "c1",
          predicate: "编译",
          weight: 0.8,
          group: "新增关系组",
        },
        {
          source: "c1",
          target: "k1",
          predicate: "存于",
          weight: 0.7,
          group: "新增关系组",
        },
        {
          source: "p1",
          target: "k1",
          predicate: "写入",
          weight: 0.6,
          group: "另一组",
        },
        { source: "c1", target: "p1", predicate: "谁知道呢", weight: 0.5 },
      ],
      truncated: false,
    });

    await renderView();

    // 闭集外组名自成一行、计数取该组边数；「未映射」只数无 group 的那条
    expect(legendRow("memory-legend-group-新增关系组").textContent).toBe(
      "新增关系组2",
    );
    expect(legendRow("memory-legend-group-另一组").textContent).toBe("另一组1");
    expect(legendRow("memory-legend-group-__unmapped__").textContent).toBe(
      "未映射1",
    );

    // 额外组用兜底灰
    expect(swatch("memory-legend-group-新增关系组")).toBe(toRgb(FALLBACK_HEX));

    // 箭头 marker：两个额外组各一个稳定 id、未映射一个兜底 id —— 恰 3 个互异，
    // 且额外组不落回兜底 id（同组同名同 id、不同组不同 id）
    const markers = Array.from(
      container.querySelectorAll("svg path[marker-end]"),
    ).map((p) => p.getAttribute("marker-end"));
    expect(new Set(markers).size).toBe(3);
    expect(
      markers.filter((m) => m === "url(#memory-graph-arrow-fallback)"),
    ).toHaveLength(1);
  });
});
