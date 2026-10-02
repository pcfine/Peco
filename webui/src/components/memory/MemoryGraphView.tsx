// 记忆图谱视图 —— 取数 + dagre 布局 + 纯 SVG 渲染 + 选中 / 平移 / 缩放（design §3.5.3）

import {
  useCallback,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
} from "react";
import { graphlib, layout } from "@dagrejs/dagre";
import axios from "axios";
import { RefreshCw } from "lucide-react";
import { getMemoryGraph } from "@/api/memory";
import type { EdgeLabel } from "@dagrejs/dagre";
import type {
  MemoryEdge,
  MemoryGraphResponse,
  MemoryNode,
} from "@/types/memory";
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
const MIN_SCALE = 0.25;
const MAX_SCALE = 3;

interface PlacedNode {
  id: string;
  name: string;
  x: number;
  y: number;
}

interface PlacedEdge {
  key: string;
  source: string;
  target: string;
  predicate: string;
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

/** 把 dagre 折线转成 SVG path（无点则回退为源→靶中心直线）。 */
function edgePath(
  points: { x: number; y: number }[],
  ox: number,
  oy: number,
  fallback: { x1: number; y1: number; x2: number; y2: number },
): string {
  if (points.length >= 2) {
    return points
      .map((p, i) => `${i === 0 ? "M" : "L"} ${p.x + ox} ${p.y + oy}`)
      .join(" ");
  }
  return `M ${fallback.x1 + ox} ${fallback.y1 + oy} L ${fallback.x2 + ox} ${fallback.y2 + oy}`;
}

function truncateName(name: string): string {
  return name.length > NAME_MAX ? `${name.slice(0, NAME_MAX)}…` : name;
}

export function MemoryGraphView({ ref }: MemoryGraphViewProps) {
  const [data, setData] = useState<MemoryGraphResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  const [view, setView] = useState({ tx: 0, ty: 0, k: 1 });
  const dragRef = useRef<{ x: number; y: number } | null>(null);

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

    // multigraph: true 必须 —— 并行边 (s,t,predicate) 可能重复，非 multigraph 会丢边
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

    const ids = new Set(data.nodes.map((n) => n.id));
    // idx = 同 (source, target, predicate) 的并行边序号，与 React key 同源
    const counters = new Map<string, number>();
    const rawEdges: { key: string; name: string; edge: MemoryEdge }[] = [];
    for (const e of data.edges) {
      if (!ids.has(e.source) || !ids.has(e.target)) continue;
      const base = `${e.source}→${e.target}#${e.predicate}`;
      const idx = counters.get(base) ?? 0;
      counters.set(base, idx + 1);
      const name = `${e.predicate}#${idx}`;
      g.setEdge(e.source, e.target, {}, name);
      rawEdges.push({ key: `${base}#${idx}`, name, edge: e });
    }

    layout(g);

    const nodes: PlacedNode[] = data.nodes.map((n) => {
      const pos = g.node(n.id);
      return { id: n.id, name: n.name, x: pos?.x ?? 0, y: pos?.y ?? 0 };
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
    if (!Number.isFinite(minX)) {
      minX = 0;
      minY = 0;
      maxX = NODE_W;
      maxY = NODE_H;
    }

    const edges: PlacedEdge[] = rawEdges.map(({ key, name, edge }) => {
      const label = g.edge({
        v: edge.source,
        w: edge.target,
        name,
      }) as EdgeLabel | undefined;
      return {
        key,
        source: edge.source,
        target: edge.target,
        predicate: edge.predicate,
        points: (label?.points ?? []).map((p) => ({ x: p.x, y: p.y })),
      };
    });

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
        // source === selected 用 →，target === selected 用 ←
        arrow: outgoing ? "→" : "←",
        neighborName: nameById.get(outgoing ? e.target : e.source) ?? "",
        weight: e.weight,
      });
    }
    return out;
  }, [data, selectedNodeId]);

  const nodePosById = useMemo(() => {
    const map = new Map<string, PlacedNode>();
    for (const n of layoutData?.nodes ?? []) map.set(n.id, n);
    return map;
  }, [layoutData]);

  const resetView = () => setView({ tx: 0, ty: 0, k: 1 });

  const handleWheel = (e: React.WheelEvent<SVGSVGElement>) => {
    const factor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
    const rect = e.currentTarget.getBoundingClientRect();
    const px = e.clientX - rect.left;
    const py = e.clientY - rect.top;
    setView((v) => {
      const k = Math.min(MAX_SCALE, Math.max(MIN_SCALE, v.k * factor));
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
        <div className="relative h-[520px] flex-1 overflow-hidden rounded-md border bg-muted/10">
          <svg
            viewBox={`0 0 ${layoutData.width} ${layoutData.height}`}
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
              <marker
                id="memory-graph-arrow"
                viewBox="0 0 10 7"
                refX={10}
                refY={3.5}
                markerWidth={8}
                markerHeight={6}
                orient="auto-start-reverse"
              >
                <polygon
                  points="0 0, 10 3.5, 0 7"
                  fill="var(--color-border, #d4d4d8)"
                />
              </marker>
            </defs>

            <g transform={`translate(${view.tx},${view.ty}) scale(${view.k})`}>
              {/* 边：path + 箭头 + 中点谓词 */}
              {layoutData.edges.map((pe) => {
                const from = nodePosById.get(pe.source);
                const to = nodePosById.get(pe.target);
                const fallback = {
                  x1: (from?.x ?? 0) + NODE_W / 2,
                  y1: from?.y ?? 0,
                  x2: (to?.x ?? 0) - NODE_W / 2,
                  y2: to?.y ?? 0,
                };
                const d = edgePath(pe.points, layoutData.ox, layoutData.oy, {
                  x1: fallback.x1,
                  y1: fallback.y1,
                  x2: fallback.x2,
                  y2: fallback.y2,
                });
                // dagre 无控制点时回退为源→靶直线，谓词标签仍摆在直线中点
                const midPath =
                  pe.points.length >= 2
                    ? pe.points
                    : [
                        { x: fallback.x1, y: fallback.y1 },
                        { x: fallback.x2, y: fallback.y2 },
                      ];
                const mid = polylineMidpoint(midPath);
                const midX = mid.x + layoutData.ox;
                const midY = mid.y + layoutData.oy;
                const active =
                  neighborIds === null ||
                  pe.source === selectedNodeId ||
                  pe.target === selectedNodeId;
                return (
                  <g key={pe.key} opacity={active ? 1 : 0.12}>
                    <path
                      d={d}
                      fill="none"
                      stroke="var(--color-border, #d4d4d8)"
                      strokeWidth={1.5}
                      markerEnd="url(#memory-graph-arrow)"
                    />
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
                return (
                  <g
                    key={n.id}
                    data-testid={`memory-node-${n.id}`}
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
                      fill="var(--color-muted, #f4f4f5)"
                      stroke={
                        isSelected
                          ? "var(--color-primary, #18181b)"
                          : "var(--color-border, #d4d4d8)"
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

          <Button
            variant="outline"
            size="sm"
            className="absolute right-2 top-2"
            onClick={resetView}
          >
            <RefreshCw className="h-4 w-4" />
            复位
          </Button>
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
                        <span className="min-w-0 truncate">
                          <span className="font-medium">{r.predicate}</span>{" "}
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
