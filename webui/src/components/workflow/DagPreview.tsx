import { useMemo } from "react";
import { graphlib, layout } from "@dagrejs/dagre";
import type { DagNode, DagEdge } from "@/types/workflow";
import { cn } from "@/lib/utils";

interface DagPreviewProps {
  nodes: DagNode[];
  edges: DagEdge[];
  className?: string;
  width?: number;
  height?: number;
}

/** Color mapping by node type */
const TYPE_COLORS: Record<
  string,
  { bg: string; stroke: string; text: string }
> = {
  shell: {
    bg: "var(--color-blue-100, #dbeafe)",
    stroke: "var(--color-blue-500, #3b82f6)",
    text: "var(--color-blue-800, #1e40af)",
  },
  agent: {
    bg: "var(--color-purple-100, #f3e8ff)",
    stroke: "var(--color-purple-500, #a855f7)",
    text: "var(--color-purple-800, #6b21a8)",
  },
  llm: {
    bg: "var(--color-amber-100, #fef3c7)",
    stroke: "var(--color-amber-500, #f59e0b)",
    text: "var(--color-amber-800, #92400e)",
  },
  tool: {
    bg: "var(--color-emerald-100, #d1fae5)",
    stroke: "var(--color-emerald-500, #10b981)",
    text: "var(--color-emerald-800, #065f46)",
  },
};

const NODE_WIDTH = 120;
const NODE_HEIGHT = 48;
const FONT_SIZE = 12;

/**
 * Compute DAG layout using dagre and render as SVG.
 * Pure presentation — no interactivity in Phase 1.
 */
export function DagPreview({
  nodes,
  edges,
  className,
  width = 400,
  height = 200,
}: DagPreviewProps) {
  const positionedNodes = useMemo(() => {
    if (nodes.length === 0) return [];

    const g = new graphlib.Graph({ directed: true });
    g.setGraph({
      rankdir: "TB",
      nodesep: 40,
      ranksep: 60,
      marginx: 20,
      marginy: 20,
    });
    g.setDefaultEdgeLabel(() => ({}));

    for (const node of nodes) {
      g.setNode(node.id, { width: NODE_WIDTH, height: NODE_HEIGHT });
    }
    for (const edge of edges) {
      g.setEdge(edge.from, edge.to);
    }

    layout(g);

    return nodes.map((node) => {
      const pos = g.node(node.id);
      return {
        ...node,
        x: pos?.x ?? 0,
        y: pos?.y ?? 0,
        width: NODE_WIDTH,
        height: NODE_HEIGHT,
      };
    });
  }, [nodes, edges]);

  const graphBounds = useMemo(() => {
    if (positionedNodes.length === 0)
      return { w: width, h: height, minX: 0, minY: 0 };
    let minX = Infinity,
      minY = Infinity,
      maxX = -Infinity,
      maxY = -Infinity;
    for (const n of positionedNodes) {
      minX = Math.min(minX, (n.x ?? 0) - NODE_WIDTH / 2);
      minY = Math.min(minY, (n.y ?? 0) - NODE_HEIGHT / 2);
      maxX = Math.max(maxX, (n.x ?? 0) + NODE_WIDTH / 2);
      maxY = Math.max(maxY, (n.y ?? 0) + NODE_HEIGHT / 2);
    }
    return {
      w: Math.max(width, maxX - minX + 40),
      h: Math.max(height, maxY - minY + 40),
      minX,
      minY,
    };
  }, [positionedNodes, width, height]);

  if (nodes.length === 0) {
    return (
      <div
        className={cn(
          "flex items-center justify-center rounded-md border border-dashed bg-muted/20 text-sm text-muted-foreground",
          className,
        )}
        style={{ width, height }}
      >
        暂无步骤
      </div>
    );
  }

  const offsetX = -graphBounds.minX + 20;
  const offsetY = -graphBounds.minY + 20;

  // Build edge path data
  const edgePaths: { from: string; to: string; d: string }[] = [];
  for (const edge of edges) {
    const from = positionedNodes.find((n) => n.id === edge.from);
    const to = positionedNodes.find((n) => n.id === edge.to);
    if (!from || !to) continue;

    const x1 = (from.x ?? 0) + offsetX;
    const y1 = (from.y ?? 0) + NODE_HEIGHT / 2 + offsetY;
    const x2 = (to.x ?? 0) + offsetX;
    const y2 = (to.y ?? 0) - NODE_HEIGHT / 2 + offsetY;
    const midY = (y1 + y2) / 2;

    edgePaths.push({
      from: edge.from,
      to: edge.to,
      d: `M ${x1} ${y1} C ${x1} ${midY}, ${x2} ${midY}, ${x2} ${y2}`,
    });
  }

  return (
    <svg
      viewBox={`0 0 ${graphBounds.w} ${graphBounds.h}`}
      className={cn("w-full h-auto", className)}
      style={{ maxHeight: graphBounds.h }}
    >
      {/* Edge lines */}
      {edgePaths.map((ep) => (
        <path
          key={`${ep.from}-${ep.to}`}
          d={ep.d}
          fill="none"
          stroke="var(--color-border, #d4d4d8)"
          strokeWidth={1.5}
          markerEnd="url(#arrowhead)"
        />
      ))}

      {/* Arrowhead marker */}
      <defs>
        <marker
          id="arrowhead"
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

      {/* Nodes */}
      {positionedNodes.map((node) => {
        const colors = TYPE_COLORS[node.type] ?? TYPE_COLORS.shell;
        const nx = (node.x ?? 0) - NODE_WIDTH / 2 + offsetX;
        const ny = (node.y ?? 0) - NODE_HEIGHT / 2 + offsetY;

        return (
          <g key={node.id}>
            <rect
              x={nx}
              y={ny}
              width={NODE_WIDTH}
              height={NODE_HEIGHT}
              rx={6}
              fill={colors.bg}
              stroke={colors.stroke}
              strokeWidth={1.5}
            />
            <text
              x={nx + NODE_WIDTH / 2}
              y={ny + NODE_HEIGHT / 2 - 5}
              textAnchor="middle"
              fontSize={FONT_SIZE}
              fontWeight={500}
              fill={colors.text}
            >
              {node.name.length > 14 ? node.name.slice(0, 13) + "…" : node.name}
            </text>
            <text
              x={nx + NODE_WIDTH / 2}
              y={ny + NODE_HEIGHT / 2 + 10}
              textAnchor="middle"
              fontSize={10}
              fill={colors.text}
              opacity={0.6}
            >
              {node.type}
            </text>
          </g>
        );
      })}
    </svg>
  );
}
