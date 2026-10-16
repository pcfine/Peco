// 记忆图谱视图 —— 取数 + dagre 布局 + 纯 SVG 渲染 + 选中 / 平移 / 缩放

import {
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { graphlib, layout } from "@dagrejs/dagre";
import axios from "axios";
import { Minus, Plus, RefreshCw } from "lucide-react";
import { getMemoryGraph } from "@/api/memory";
import type { EdgeLabel } from "@dagrejs/dagre";
import type {
  MemoryEdge,
  MemoryGraphResponse,
  MemoryNode,
} from "@/types/memory";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";

/** 供父组件（`MemoryPage`）触发重新拉取的句柄。 */
export interface MemoryGraphViewHandle {
  refresh: () => void;
}

interface MemoryGraphViewProps {
  ref?: React.Ref<MemoryGraphViewHandle>;
}

/** 节点盒尺寸（与 dagre `setNode` 一致）。 */
const NODE_W = 140;
const NODE_H = 40;
/** 节点名超过 10 字截断为 `…`（完整名放 `<title>`）。 */
const NAME_MAX = 10;
const PAD = 16;
/** 平行边之间的垂直间距（px）—— 同一对节点的多条边据此错开。 */
const EDGE_SPREAD = 9;

/** `Other` / 未知类型 / 未映射及闭集外关系组共用的兜底色。 */
const FALLBACK_COLOR = "#a1a1aa";

/**
 * 节点类型闭集 + 配色。顺序即图例展示序；`Other` 用兜底灰。
 * 闭集外的取值（含字段缺失）在图例里全部归到「未知」一行，不新增行。
 */
const NODE_TYPES: { name: string; color: string }[] = [
  { name: "Person", color: "#ef4444" },
  { name: "Organization", color: "#f97316" },
  { name: "SoftwareProject", color: "#f59e0b" },
  { name: "SoftwareComponent", color: "#84cc16" },
  { name: "Technology", color: "#22c55e" },
  { name: "CodeSymbol", color: "#10b981" },
  { name: "Document", color: "#14b8a6" },
  { name: "KnowledgeBase", color: "#06b6d4" },
  { name: "Agent", color: "#3b82f6" },
  { name: "Issue", color: "#6366f1" },
  { name: "Task", color: "#8b5cf6" },
  { name: "Commit", color: "#a855f7" },
  { name: "Concept", color: "#d946ef" },
  { name: "Other", color: FALLBACK_COLOR },
];

/**
 * 关系组清单 + 配色（已知组）。谓词到组的映射由后端读侧派生、随边下发，前端只认
 * 组名 —— 后端扩充映射表时这里不用改：闭集外的组名自成一行、用兜底灰。
 */
const RELATION_GROUPS: { name: string; color: string }[] = [
  { name: "使用依赖", color: "#0ea5e9" },
  { name: "组成包含", color: "#14b8a6" },
  { name: "定义记录", color: "#6366f1" },
  { name: "修复变更", color: "#22c55e" },
  { name: "人员归属", color: "#f97316" },
  { name: "偏好", color: "#ec4899" },
  { name: "缺陷问题", color: "#ef4444" },
  { name: "名称标识", color: "#a855f7" },
  { name: "集成配置", color: "#eab308" },
];

/** 「未知」图例行的键：与任何真实类型名都不重合，免得出现在闭集里造成误判。 */
const UNKNOWN_TYPE = "__unknown_type__";

/** 「未映射」图例行的键：仅 `group` 缺省或为空串时归到这里；闭集外组名自成一行。 */
const UNMAPPED_GROUP = "__unmapped__";

/** 节点 `type` 原文 → 图例键；闭集外（含字段缺失）统一归到「未知」。 */
function typeKeyOf(type: string | undefined): string {
  return type && NODE_TYPES.some((t) => t.name === type) ? type : UNKNOWN_TYPE;
}

/**
 * 边 `group` 原文 → 图例键。缺省 / 空串即未映射（不是错误）；其余（含闭集外的组名）
 * 按原样自成一行 —— 后端扩充映射表后新组不会被误并进「未映射」。
 */
function groupKeyOf(group: string | undefined): string {
  return group ? group : UNMAPPED_GROUP;
}

/** 图例键 → 节点色。 */
function nodeColorOf(typeKey: string): string {
  return NODE_TYPES.find((t) => t.name === typeKey)?.color ?? FALLBACK_COLOR;
}

/** 图例键 → 边色。 */
function groupColorOf(groupKey: string): string {
  return (
    RELATION_GROUPS.find((g) => g.name === groupKey)?.color ?? FALLBACK_COLOR
  );
}

/** 组名 → 稳定 id 后缀。组名可能是任意文本（含中文 / 空格），直接入 SVG id 不安全。 */
function groupNameHash(name: string): string {
  let h = 2166136261;
  for (let i = 0; i < name.length; i++) {
    h = Math.imul(h ^ name.charCodeAt(i), 16777619);
  }
  return (h >>> 0).toString(36);
}

/**
 * 箭头 marker id —— 组名含中文，不宜直接做 SVG id。已知组按序号，未映射用兜底，
 * 闭集外的组名按名哈希：同组同名同 id、不同组不同 id。
 */
function arrowMarkerId(groupKey: string): string {
  const i = RELATION_GROUPS.findIndex((g) => g.name === groupKey);
  if (i >= 0) return `memory-graph-arrow-g${i}`;
  if (groupKey === UNMAPPED_GROUP) return "memory-graph-arrow-fallback";
  return `memory-graph-arrow-x${groupNameHash(groupKey)}`;
}

/** 箭头 marker 基线（已知组 + 未映射）；闭集外组名的 marker 随数据追加。 */
const ARROW_MARKERS: { id: string; color: string }[] = [
  ...RELATION_GROUPS.map((g) => ({
    id: arrowMarkerId(g.name),
    color: g.color,
  })),
  { id: arrowMarkerId(UNMAPPED_GROUP), color: FALLBACK_COLOR },
];

/**
 * 布局图上的「节点对」键。用 JSON 元组而非拼接分隔符：node id 由后端给出，
 * 拼接分隔符一旦出现在 id 里会把两条不同关系折叠成同一条（静默丢边）。
 */
function pairKey(source: string, target: string): string {
  return JSON.stringify([source, target]);
}
/**
 * 缩放区间。viewBox 与容器像素 1:1（见 `vw`/`vh` 与 `fitView`），故 `k` 就是
 * 相对 1:1 的屏幕倍率：k=1 时节点 140×40 px、字号 12px（可读）。上下界给足
 * 余量以实现「自由缩放」——大图能缩小看全貌，也能放大到看清节点。
 */
const MIN_SCALE = 0.02;
const MAX_SCALE = 5;
/** 滚轮每格缩放系数。 */
const WHEEL_STEP = 1.2;

interface PlacedNode {
  id: string;
  name: string;
  /** 图例键（闭集外已归为「未知」）。 */
  type: string;
  x: number;
  y: number;
}

interface PlacedEdge {
  key: string;
  source: string;
  target: string;
  predicate: string;
  /** 图例键（`group` 缺省已归为「未映射」）。 */
  group: string;
  /** 最终绘制走线（已含平行边垂直偏移）。 */
  points: { x: number; y: number }[];
}

interface GraphLayout {
  nodes: PlacedNode[];
  edges: PlacedEdge[];
  ox: number;
  oy: number;
  width: number;
  height: number;
}

/** 后端 `ApiError` → 可读文案（与既有页面的局部 helper 同口径）。 */
function getApiErrorMessage(err: unknown): string | undefined {
  if (axios.isAxiosError(err)) {
    if (err.response?.data?.details) return String(err.response.data.details);
    if (err.response?.data?.message) return String(err.response.data.message);
    if (err.message) return err.message;
  }
  if (err instanceof Error) return err.message;
  return undefined;
}

/** 沿折线取「弧长中点」，用于摆放谓词标签。 */
function polylineMidpoint(pts: { x: number; y: number }[]): {
  x: number;
  y: number;
} {
  if (pts.length === 0) return { x: 0, y: 0 };
  if (pts.length === 1) return pts[0];
  let total = 0;
  const segs: number[] = [];
  for (let i = 1; i < pts.length; i += 1) {
    const len = Math.hypot(pts[i].x - pts[i - 1].x, pts[i].y - pts[i - 1].y);
    segs.push(len);
    total += len;
  }
  if (total === 0) return pts[0];
  let acc = 0;
  for (let i = 0; i < segs.length; i += 1) {
    if (acc + segs[i] >= total / 2) {
      const t = segs[i] === 0 ? 0 : (total / 2 - acc) / segs[i];
      return {
        x: pts[i].x + (pts[i + 1].x - pts[i].x) * t,
        y: pts[i].y + (pts[i + 1].y - pts[i].y) * t,
      };
    }
    acc += segs[i];
  }
  return pts[pts.length - 1];
}

/** 折线 → SVG path（调用方保证点 ≥ 2）。 */
function polylinePath(
  points: { x: number; y: number }[],
  ox: number,
  oy: number,
): string {
  return points
    .map((p, i) => `${i === 0 ? "M" : "L"} ${p.x + ox} ${p.y + oy}`)
    .join(" ");
}

/** 沿折线法向平移 `off` px —— 同一对节点的平行边据此错开，避免完全重叠。 */
function shiftPolyline(
  points: { x: number; y: number }[],
  off: number,
): { x: number; y: number }[] {
  if (off === 0) return points;
  const dx = points[points.length - 1].x - points[0].x;
  const dy = points[points.length - 1].y - points[0].y;
  const len = Math.hypot(dx, dy);
  if (len === 0) return points;
  const nx = -dy / len;
  const ny = dx / len;
  return points.map((p) => ({ x: p.x + nx * off, y: p.y + ny * off }));
}

/** dagre 抛错时的兜底排布：按序网格，保证节点可见、页面不白屏。 */
function gridLayout(
  nodes: MemoryNode[],
): Map<string, { x: number; y: number }> {
  const cols = Math.max(1, Math.ceil(Math.sqrt(nodes.length)));
  const out = new Map<string, { x: number; y: number }>();
  nodes.forEach((n, i) => {
    out.set(n.id, {
      x: (i % cols) * (NODE_W + 40) + NODE_W / 2,
      y: Math.floor(i / cols) * (NODE_H + 40) + NODE_H / 2,
    });
  });
  return out;
}

/** 视口尺寸（px）。viewBox 与之一致 ⇒ SVG 用户单位 == CSS 像素。 */
interface Viewport {
  w: number;
  h: number;
}

/** 让整张图恰好铺满视口并居中（缩放 + 平移一次算好）。 */
function fitView(
  vp: Viewport,
  layout: { width: number; height: number },
): { tx: number; ty: number; k: number } {
  const k = Math.min(vp.w / layout.width, vp.h / layout.height);
  // 尺寸退化（0 / NaN）时退回 1:1，绝不产生 NaN transform。
  const kk = Number.isFinite(k) && k > 0 ? k : 1;
  return {
    k: kk,
    tx: (vp.w - layout.width * kk) / 2,
    ty: (vp.h - layout.height * kk) / 2,
  };
}

function truncateName(name: string): string {
  return name.length > NAME_MAX ? `${name.slice(0, NAME_MAX)}…` : name;
}

export function MemoryGraphView({ ref }: MemoryGraphViewProps) {
  const [data, setData] = useState<MemoryGraphResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  /** 被隐藏的节点类型（图例键）。空集 = 全开；只影响渲染掩码，不持久化。 */
  const [hiddenTypes, setHiddenTypes] = useState<Set<string>>(new Set());
  const [view, setView] = useState({ tx: 0, ty: 0, k: 1 });
  const dragRef = useRef<{ x: number; y: number } | null>(null);
  // SVG 容器的真实像素尺寸（决定 viewBox）；未测得时退回内容尺寸 ⇒ k=1。
  const wrapRef = useRef<HTMLDivElement>(null);
  const [vp, setVp] = useState<Viewport>({ w: 0, h: 0 });
  // 已「适应内容」过的布局对象；换了一份数据才重新复位，避免每次渲染都跳回。
  const fittedRef = useRef<GraphLayout | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const graph = await getMemoryGraph();
      setData(graph);
    } catch (err) {
      setError(getApiErrorMessage(err) ?? "加载记忆图谱失败");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useImperativeHandle(ref, () => ({ refresh: load }), [load]);

  const toggleType = useCallback((key: string) => {
    setHiddenTypes((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  // Esc 收起选中面板
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setSelectedNodeId(null);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const layoutData = useMemo<GraphLayout | null>(() => {
    if (!data || data.nodes.length === 0) return null;

    // dagre 3.1.x 有两类已知布局抛错，都会打断整个 useMemo、使整页白屏：
    //   ① 交集计算：跨层长边的 dummy 落在空 rank 上拿不到坐标 → 该边
    //      `points` 出现 NaN → intersectRect 抛
    //      `Not possible to find intersection inside of the rectangle`
    //      （实测由「同一对节点间多条平行边 + 孤立点」触发）。
    //   ② 模块级跨调用状态：bundle 内的 `useDynamic` 缓存保存**上一张**布局图
    //      （`var ke=null,xe=null`），连续布局不同图时可能抛错。
    // 规避 ①：交给 dagre 的图每条 (source,target) 只喂一条代表边（实测把 ①
    // 的触发率降到 0）；平行边改在本层按序号垂直展开，关系不丢（选中节点侧栏
    // 本就逐条列出图上全部关系）。
    // 规避 ②：不依赖——下面的 try/catch 是**最终保底**，它兜住一切布局异常，
    // 包括 ② 和未来新形态。**不要因为它看起来没被触发就删掉**：删掉即退回白屏。
    const ids = new Set(data.nodes.map((n) => n.id));
    // idx = 同 (source, target) 的平行边序号（与 React key、垂直偏移同源）
    const pairSeen = new Map<string, number>();
    const reps = new Map<string, MemoryEdge>();
    const drawEdges: {
      key: string;
      edge: MemoryEdge;
      pair: string;
      idx: number;
    }[] = [];
    for (const e of data.edges) {
      if (!ids.has(e.source) || !ids.has(e.target)) continue;
      const pair = pairKey(e.source, e.target);
      const idx = pairSeen.get(pair) ?? 0;
      pairSeen.set(pair, idx + 1);
      if (!reps.has(pair)) reps.set(pair, e);
      drawEdges.push({
        key: `${pair}#${e.predicate}#${idx}`,
        edge: e,
        pair,
        idx,
      });
    }

    const g = new graphlib.Graph({ directed: true, multigraph: true });
    g.setDefaultEdgeLabel(() => ({}));
    g.setGraph({
      rankdir: "LR",
      nodesep: 32,
      ranksep: 72,
      marginx: 24,
      marginy: 24,
    });
    for (const n of data.nodes) {
      g.setNode(n.id, { width: NODE_W, height: NODE_H });
    }
    for (const [pair, e] of reps) g.setEdge(e.source, e.target, {}, pair);

    let placed = new Map<string, { x: number; y: number }>();
    let routed = new Map<string, { x: number; y: number }[]>();
    try {
      layout(g);
      for (const n of data.nodes) {
        const pos = g.node(n.id);
        placed.set(n.id, { x: pos?.x ?? 0, y: pos?.y ?? 0 });
      }
      for (const [pair, e] of reps) {
        const label = g.edge({ v: e.source, w: e.target, name: pair }) as
          EdgeLabel | undefined;
        routed.set(
          pair,
          (label?.points ?? []).map((p) => ({ x: p.x, y: p.y })),
        );
      }
    } catch (err) {
      // 兜底：布局异常绝不升级为白屏 —— 退化为按序网格，边走直线。
      // 必须留痕：兜底一旦静默，用户只会看到「布局变丑」而无从察觉。
      console.warn(
        "[memory-graph] dagre layout failed; falling back to grid",
        err,
      );
      placed = gridLayout(data.nodes);
      routed = new Map();
    }

    const nodes: PlacedNode[] = data.nodes.map((n) => {
      const pos = placed.get(n.id);
      return {
        id: n.id,
        name: n.name,
        type: typeKeyOf(n.type),
        x: pos?.x ?? 0,
        y: pos?.y ?? 0,
      };
    });

    // 平行边共用代表边走线，按序号垂直展开（代表边居中）；无走线点的边补成
    // 「源→靶」直线（各让出节点半宽），保证每个 PlacedEdge.points 都 ≥ 2 点。
    const posById = new Map(nodes.map((n) => [n.id, n]));
    const edges: PlacedEdge[] = drawEdges.map(({ key, edge, pair, idx }) => {
      const routedPoints = routed.get(pair);
      const base =
        routedPoints && routedPoints.length >= 2
          ? routedPoints
          : [
              {
                x: (posById.get(edge.source)?.x ?? 0) + NODE_W / 2,
                y: posById.get(edge.source)?.y ?? 0,
              },
              {
                x: (posById.get(edge.target)?.x ?? 0) - NODE_W / 2,
                y: posById.get(edge.target)?.y ?? 0,
              },
            ];
      const total = pairSeen.get(pair) ?? 1;
      return {
        key,
        source: edge.source,
        target: edge.target,
        predicate: edge.predicate,
        group: groupKeyOf(edge.group),
        points: shiftPolyline(base, (idx - (total - 1) / 2) * EDGE_SPREAD),
      };
    });

    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (const n of nodes) {
      minX = Math.min(minX, n.x - NODE_W / 2);
      minY = Math.min(minY, n.y - NODE_H / 2);
      maxX = Math.max(maxX, n.x + NODE_W / 2);
      maxY = Math.max(maxY, n.y + NODE_H / 2);
    }
    // 边已含平行边偏移，一并纳入包围盒（避免贴边裁切）
    for (const pe of edges) {
      for (const p of pe.points) {
        minX = Math.min(minX, p.x);
        minY = Math.min(minY, p.y);
        maxX = Math.max(maxX, p.x);
        maxY = Math.max(maxY, p.y);
      }
    }
    if (!Number.isFinite(minX)) {
      minX = 0;
      minY = 0;
      maxX = NODE_W;
      maxY = NODE_H;
    }

    const ox = -minX + PAD;
    const oy = -minY + PAD;
    return {
      nodes,
      edges,
      ox,
      oy,
      // Math.max(1, …) 保护：单节点 / 0 边时包围盒不退化，防 scale 除零
      width: Math.max(1, maxX - minX) + PAD * 2,
      height: Math.max(1, maxY - minY) + PAD * 2,
    };
  }, [data]);

  /**
   * 图例行：类型 14 类 + 「未知」；关系组为已知 9 组 ∪ 本轮出现的闭集外组名（+ 仅当
   * 存在无 `group` 的边时才有的「未映射」）。类型行清单恒定；关系组行随数据里出现的
   * 额外组名增减，计数取当前 `nodes` / `edges`。
   */
  const legend = useMemo(() => {
    if (!data) return null;
    const typeCounts = new Map<string, number>();
    for (const n of data.nodes) {
      const key = typeKeyOf(n.type);
      typeCounts.set(key, (typeCounts.get(key) ?? 0) + 1);
    }
    const knownGroups = new Set(RELATION_GROUPS.map((g) => g.name));
    const groupCounts = new Map<string, number>();
    const extraGroups: string[] = [];
    const seenExtra = new Set<string>();
    let hasUnmapped = false;
    for (const e of data.edges) {
      const key = groupKeyOf(e.group);
      groupCounts.set(key, (groupCounts.get(key) ?? 0) + 1);
      if (key === UNMAPPED_GROUP) {
        hasUnmapped = true;
      } else if (!knownGroups.has(key) && !seenExtra.has(key)) {
        seenExtra.add(key);
        extraGroups.push(key);
      }
    }
    extraGroups.sort();
    const withCounts = (
      rows: { key: string; label: string; color: string }[],
      counts: Map<string, number>,
    ) => rows.map((row) => ({ ...row, count: counts.get(row.key) ?? 0 }));
    return {
      types: withCounts(
        [
          ...NODE_TYPES.map((t) => ({
            key: t.name,
            label: t.name,
            color: t.color,
          })),
          { key: UNKNOWN_TYPE, label: "未知", color: FALLBACK_COLOR },
        ],
        typeCounts,
      ),
      groups: withCounts(
        [
          ...RELATION_GROUPS.map((g) => ({
            key: g.name,
            label: g.name,
            color: g.color,
          })),
          ...extraGroups.map((name) => ({
            key: name,
            label: name,
            color: FALLBACK_COLOR,
          })),
          ...(hasUnmapped
            ? [{ key: UNMAPPED_GROUP, label: "未映射", color: FALLBACK_COLOR }]
            : []),
        ],
        groupCounts,
      ),
    };
  }, [data]);

  /**
   * 箭头 marker 定义：已知组 + 未映射之外，再为本轮出现的闭集外组名补一个 ——
   * `arrowMarkerId` 会按其名生成专属 id，对应的 marker 不建则箭头引用悬空、不显示。
   */
  const arrowMarkers = useMemo(() => {
    const markers = [...ARROW_MARKERS];
    const seen = new Set(markers.map((m) => m.id));
    if (data) {
      for (const e of data.edges) {
        const key = groupKeyOf(e.group);
        const id = arrowMarkerId(key);
        if (seen.has(id)) continue;
        seen.add(id);
        markers.push({ id, color: groupColorOf(key) });
      }
    }
    return markers;
  }, [data]);

  /**
   * 过滤掩码：只决定「画不画」，**不参与布局** —— `layoutData` 的依赖里没有
   * `hiddenTypes`，所以开关类型不会重跑 dagre（重排会让大图跳变，还白付一次
   * 布局开销）。隐藏一个类型会连带隐藏它的悬空边。
   */
  const hiddenNodeIds = useMemo(() => {
    if (!layoutData || hiddenTypes.size === 0) return null;
    const ids = new Set<string>();
    for (const n of layoutData.nodes) {
      if (hiddenTypes.has(n.type)) ids.add(n.id);
    }
    return ids;
  }, [layoutData, hiddenTypes]);

  // 把 viewBox 设为「与容器像素 1:1」——这是放大得动的前提。
  // 旧实现 viewBox = 整图 bbox，浏览器先把整图压进视口（77 节点实测 fit≈0.12），
  // 再乘 k≤3 ⇒ 有效放大上限仅 0.36×，节点永远糊。改成 1:1 后 k=1 即真实 1:1。
  useLayoutEffect(() => {
    const el = wrapRef.current;
    if (!el || !layoutData) return;
    const w = el.clientWidth;
    const h = el.clientHeight;
    if (w > 0 && h > 0) setVp((p) => (p.w === w && p.h === h ? p : { w, h }));
    // 新布局到达：复位为「适应内容」（首帧测量不到时用内容尺寸兜底）。
    if (fittedRef.current !== layoutData) {
      fittedRef.current = layoutData;
      const size =
        w > 0 && h > 0
          ? { w, h }
          : { w: layoutData.width, h: layoutData.height };
      setView(fitView(size, layoutData));
    }
  }, [layoutData]);

  // 容器尺寸变化只更新 viewBox（保留用户当前缩放），不强制复位。
  useEffect(() => {
    const el = wrapRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      const w = el.clientWidth;
      const h = el.clientHeight;
      if (w > 0 && h > 0) setVp((p) => (p.w === w && p.h === h ? p : { w, h }));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [layoutData]);

  /** 选中节点的直接邻居 id 集（客户端从已取回的 nodes/edges 过滤，不新增请求）。 */
  const neighborIds = useMemo(() => {
    if (!data || !selectedNodeId) return null;
    const set = new Set<string>();
    for (const e of data.edges) {
      if (e.source === selectedNodeId) set.add(e.target);
      else if (e.target === selectedNodeId) set.add(e.source);
    }
    return set;
  }, [data, selectedNodeId]);

  const selectedNode: MemoryNode | null = useMemo(() => {
    if (!data || !selectedNodeId) return null;
    return data.nodes.find((n) => n.id === selectedNodeId) ?? null;
  }, [data, selectedNodeId]);

  const relations = useMemo(() => {
    if (!data || !selectedNodeId) return [];
    const nameById = new Map(data.nodes.map((n) => [n.id, n.name]));
    const counters = new Map<string, number>();
    const out: {
      key: string;
      predicate: string;
      /** 关系组原文（未映射为 `undefined`）。 */
      group?: string;
      arrow: string;
      neighborName: string;
      weight: number;
    }[] = [];
    for (const e of data.edges) {
      const outgoing = e.source === selectedNodeId;
      const incoming = e.target === selectedNodeId;
      if (!outgoing && !incoming) continue;
      const base = `${e.source}→${e.target}#${e.predicate}`;
      const idx = counters.get(base) ?? 0;
      counters.set(base, idx + 1);
      out.push({
        key: `${base}#${idx}`,
        predicate: e.predicate,
        group: e.group,
        // source === selected 用 →，target === selected 用 ←
        arrow: outgoing ? "→" : "←",
        neighborName: nameById.get(outgoing ? e.target : e.source) ?? "",
        weight: e.weight,
      });
    }
    return out;
  }, [data, selectedNodeId]);

  // 未测得视口（含 jsdom 无布局）时退回内容尺寸 ⇒ viewBox 与内容同尺寸、k=1。
  const vw = vp.w || layoutData?.width || 1;
  const vh = vp.h || layoutData?.height || 1;

  const clampScale = (k: number) => Math.min(MAX_SCALE, Math.max(MIN_SCALE, k));

  /** 复位 = 适应内容（缩放到看见全图并居中）。 */
  const resetView = () => {
    if (layoutData) setView(fitView({ w: vw, h: vh }, layoutData));
  };

  /** 以视口中心为锚点缩放（按钮用）。 */
  const zoomBy = (factor: number) => {
    const cx = vw / 2;
    const cy = vh / 2;
    setView((v) => {
      const k = clampScale(v.k * factor);
      const ratio = k / v.k;
      return { k, tx: cx - ratio * (cx - v.tx), ty: cy - ratio * (cy - v.ty) };
    });
  };

  const handleWheel = (e: React.WheelEvent<SVGSVGElement>) => {
    const factor = e.deltaY < 0 ? WHEEL_STEP : 1 / WHEEL_STEP;
    const rect = e.currentTarget.getBoundingClientRect();
    // viewBox 与容器像素 1:1 ⇒ 客户端坐标即用户单位，锚点无需再折算。
    const px = e.clientX - rect.left;
    const py = e.clientY - rect.top;
    setView((v) => {
      const k = clampScale(v.k * factor);
      const ratio = k / v.k;
      return {
        k,
        tx: px - ratio * (px - v.tx),
        ty: py - ratio * (py - v.ty),
      };
    });
  };

  /** 仅空白处起拖 = 平移。 */
  const handleMouseDown = (e: React.MouseEvent<SVGSVGElement>) => {
    if (e.target !== e.currentTarget) return;
    dragRef.current = { x: e.clientX, y: e.clientY };
  };

  const handleMouseMove = (e: React.MouseEvent<SVGSVGElement>) => {
    const start = dragRef.current;
    if (!start) return;
    setView((v) => ({
      ...v,
      tx: v.tx + (e.clientX - start.x),
      ty: v.ty + (e.clientY - start.y),
    }));
    dragRef.current = { x: e.clientX, y: e.clientY };
  };

  const handleMouseUp = () => {
    dragRef.current = null;
  };

  /** 点击空白（事件目标就是 <svg> 本身）= 收起选中。 */
  const handleBackgroundClick = (e: React.MouseEvent<SVGSVGElement>) => {
    if (e.target === e.currentTarget) setSelectedNodeId(null);
  };

  if (loading) {
    return (
      <div className="space-y-3" data-testid="memory-graph-loading">
        <Skeleton className="h-[520px] w-full" />
        <div className="flex gap-3">
          <Skeleton className="h-10 w-40" />
          <Skeleton className="h-10 w-40" />
          <Skeleton className="h-10 w-40" />
        </div>
      </div>
    );
  }

  if (error) {
    return <ErrorBanner message={error} onRetry={() => void load()} />;
  }

  if (!layoutData || !data) {
    return (
      <EmptyState
        title="还没有记忆图谱"
        description="与 peco 对话后会自动沉淀实体与关系"
      />
    );
  }

  return (
    <div className="space-y-2">
      {data.truncated && (
        <p className="text-xs text-muted-foreground">
          图谱已截断：仅展示前若干节点 / 关系。
        </p>
      )}
      <div className="flex gap-4">
        {legend && (
          <Card
            className="w-60 shrink-0 gap-3 py-4"
            data-testid="memory-graph-legend"
          >
            <CardHeader className="px-4">
              <CardTitle className="text-base">图例</CardTitle>
            </CardHeader>
            <CardContent className="px-4">
              <ScrollArea className="h-[440px]">
                <div className="space-y-4 pr-3">
                  <section>
                    <h3 className="mb-1 text-xs font-medium text-muted-foreground">
                      节点类型
                    </h3>
                    <ul className="space-y-0.5">
                      {legend.types.map((row) => {
                        const hidden = hiddenTypes.has(row.key);
                        return (
                          <li key={row.key}>
                            <button
                              type="button"
                              data-testid={`memory-legend-type-${row.key}`}
                              aria-pressed={!hidden}
                              title={`${hidden ? "显示" : "隐藏"} ${row.label}`}
                              onClick={() => toggleType(row.key)}
                              className={`flex w-full items-center gap-2 rounded px-1 py-0.5 text-left text-xs hover:bg-accent ${hidden ? "opacity-40" : ""}`}
                            >
                              <span
                                className="size-2.5 shrink-0 rounded-sm"
                                style={{ backgroundColor: row.color }}
                              />
                              <span className="min-w-0 flex-1 truncate">
                                {row.label}
                              </span>
                              <span className="shrink-0 tabular-nums text-muted-foreground">
                                {row.count}
                              </span>
                            </button>
                          </li>
                        );
                      })}
                    </ul>
                  </section>
                  <section>
                    <h3 className="mb-1 text-xs font-medium text-muted-foreground">
                      关系组
                    </h3>
                    <ul className="space-y-0.5">
                      {legend.groups.map((row) => (
                        <li
                          key={row.key}
                          data-testid={`memory-legend-group-${row.key}`}
                          className="flex items-center gap-2 px-1 py-0.5 text-xs"
                        >
                          <span
                            className="size-2.5 shrink-0 rounded-sm"
                            style={{ backgroundColor: row.color }}
                          />
                          <span className="min-w-0 flex-1 truncate">
                            {row.label}
                          </span>
                          <span className="shrink-0 tabular-nums text-muted-foreground">
                            {row.count}
                          </span>
                        </li>
                      ))}
                    </ul>
                  </section>
                </div>
              </ScrollArea>
            </CardContent>
          </Card>
        )}
        <div
          ref={wrapRef}
          className="relative h-[520px] flex-1 overflow-hidden rounded-md border bg-muted/10"
        >
          <svg
            viewBox={`0 0 ${vw} ${vh}`}
            className="h-full w-full touch-none select-none"
            role="img"
            aria-label="记忆图谱"
            onWheel={handleWheel}
            onMouseDown={handleMouseDown}
            onMouseMove={handleMouseMove}
            onMouseUp={handleMouseUp}
            onMouseLeave={handleMouseUp}
            onClick={handleBackgroundClick}
          >
            <defs>
              {arrowMarkers.map((m) => (
                <marker
                  key={m.id}
                  id={m.id}
                  viewBox="0 0 10 7"
                  refX={10}
                  refY={3.5}
                  markerWidth={8}
                  markerHeight={6}
                  orient="auto-start-reverse"
                >
                  <polygon points="0 0, 10 3.5, 0 7" fill={m.color} />
                </marker>
              ))}
            </defs>

            <g transform={`translate(${view.tx},${view.ty}) scale(${view.k})`}>
              {/* 边：path + 箭头 + 中点谓词 */}
              {layoutData.edges.map((pe) => {
                const d = polylinePath(pe.points, layoutData.ox, layoutData.oy);
                const mid = polylineMidpoint(pe.points);
                const midX = mid.x + layoutData.ox;
                const midY = mid.y + layoutData.oy;
                const active =
                  neighborIds === null ||
                  pe.source === selectedNodeId ||
                  pe.target === selectedNodeId;
                const hidden =
                  hiddenNodeIds !== null &&
                  (hiddenNodeIds.has(pe.source) ||
                    hiddenNodeIds.has(pe.target));
                return (
                  <g
                    key={pe.key}
                    display={hidden ? "none" : undefined}
                    opacity={active ? 1 : 0.12}
                  >
                    <path
                      d={d}
                      fill="none"
                      stroke={groupColorOf(pe.group)}
                      strokeWidth={1.5}
                      markerEnd={`url(#${arrowMarkerId(pe.group)})`}
                    />
                    {/* 谓词为空（后端异常数据）时不画标签，不抛错 */}
                    {pe.predicate ? (
                      <text
                        x={midX}
                        y={midY}
                        textAnchor="middle"
                        dominantBaseline="middle"
                        fontSize={11}
                        fill="var(--color-muted-foreground, #71717a)"
                        stroke="var(--color-background, #ffffff)"
                        strokeWidth={3}
                        paintOrder="stroke"
                      >
                        {pe.predicate}
                      </text>
                    ) : null}
                  </g>
                );
              })}

              {/* 节点：rect + name */}
              {layoutData.nodes.map((n) => {
                const x = n.x - NODE_W / 2 + layoutData.ox;
                const y = n.y - NODE_H / 2 + layoutData.oy;
                const isSelected = n.id === selectedNodeId;
                const dimmed =
                  neighborIds !== null && !isSelected && !neighborIds.has(n.id);
                const tint = nodeColorOf(n.type);
                return (
                  <g
                    key={n.id}
                    data-testid={`memory-node-${n.id}`}
                    display={hiddenNodeIds?.has(n.id) ? "none" : undefined}
                    className="cursor-pointer"
                    opacity={dimmed ? 0.25 : 1}
                    onClick={() => setSelectedNodeId(n.id)}
                  >
                    <title>{n.name}</title>
                    <rect
                      x={x}
                      y={y}
                      width={NODE_W}
                      height={NODE_H}
                      rx={6}
                      fill={tint}
                      fillOpacity={0.18}
                      stroke={
                        isSelected ? "var(--color-primary, #18181b)" : tint
                      }
                      strokeWidth={isSelected ? 2.5 : 1.5}
                    />
                    <text
                      x={x + NODE_W / 2}
                      y={y + NODE_H / 2}
                      textAnchor="middle"
                      dominantBaseline="middle"
                      fontSize={12}
                      fontWeight={isSelected ? 600 : 500}
                      fill="var(--color-foreground, #18181b)"
                    >
                      {truncateName(n.name)}
                    </text>
                  </g>
                );
              })}
            </g>
          </svg>

          <div className="absolute right-2 top-2 flex flex-col gap-1">
            <Button
              variant="outline"
              size="icon-sm"
              aria-label="放大"
              title="放大"
              onClick={() => zoomBy(WHEEL_STEP)}
            >
              <Plus className="h-4 w-4" />
            </Button>
            <Button
              variant="outline"
              size="icon-sm"
              aria-label="缩小"
              title="缩小"
              onClick={() => zoomBy(1 / WHEEL_STEP)}
            >
              <Minus className="h-4 w-4" />
            </Button>
            <Button
              variant="outline"
              size="icon-sm"
              aria-label="适应内容"
              title="适应内容"
              onClick={resetView}
            >
              <RefreshCw className="h-4 w-4" />
            </Button>
          </div>
        </div>

        {selectedNode && (
          <Card
            className="w-72 shrink-0 gap-3"
            data-testid="memory-selected-panel"
          >
            <CardHeader>
              <CardTitle className="text-base break-all">
                {selectedNode.name}
              </CardTitle>
            </CardHeader>
            <CardContent>
              <ScrollArea className="max-h-[420px]">
                {relations.length === 0 ? (
                  <p className="text-sm text-muted-foreground">暂无直接关系</p>
                ) : (
                  <ul className="divide-y">
                    {relations.map((r) => (
                      <li
                        key={r.key}
                        className="flex items-center justify-between gap-2 py-1.5 text-sm"
                      >
                        <span className="flex min-w-0 items-center gap-1 truncate">
                          <span className="font-medium">{r.predicate}</span>
                          {r.group && (
                            <Badge
                              variant="secondary"
                              className="shrink-0 px-1.5 py-0 text-[10px] font-normal"
                            >
                              {r.group}
                            </Badge>
                          )}
                          {r.arrow} <span>{r.neighborName}</span>
                        </span>
                        <span className="shrink-0 text-xs tabular-nums text-muted-foreground">
                          {r.weight.toFixed(2)}
                        </span>
                      </li>
                    ))}
                  </ul>
                )}
              </ScrollArea>
            </CardContent>
          </Card>
        )}
      </div>
    </div>
  );
}
