import { useMemo } from "react";
import { graphlib, layout } from "@dagrejs/dagre";
import type { DagNode, DagEdge } from "@/types/workflow";
import { cn } from "@/lib/utils";
import {
  CheckCircle2,
  XCircle,
  Circle,
  SkipForward,
  Loader2,
} from "lucide-react";

interface DagExecutionViewProps {
  nodes: DagNode[];
  edges: DagEdge[];
  className?: string;
}

const NODE_WIDTH = 130;
const NODE_HEIGHT = 52;

const STATUS_STYLES: Record<
  string,
  { bg: string; stroke: string; icon: React.ReactNode }
> = {
  pending: {
    bg: "var(--color-muted, #f4f4f5)",
    stroke: "var(--color-border, #d4d4d8)",
    icon: null,
  },
  running: {
    bg: "var(--color-blue-50, #eff6ff)",
    stroke: "var(--color-blue-500, #3b82f6)",
    icon: null,
  },
  success: {
    bg: "var(--color-green-50, #f0fdf4)",
    stroke: "var(--color-green-500, #22c55e)",
    icon: null,
  },
  failed: {
    bg: "var(--color-red-50, #fef2f2)",
    stroke: "var(--color-red-500, #ef4444)",
    icon: null,
  },
  skipped: {
    bg: "var(--color-amber-50, #fffbeb)",
    stroke: "var(--color-amber-400, #fbbf24)",
    icon: null,
  },
};

function StatusIcon({ status, size = 14 }: { status?: string; size?: number }) {
  switch (status) {
    case "running":
      return (
        <Loader2
          className="animate-spin"
          size={size}
          style={{ color: "#3b82f6" }}
        />
      );
    case "success":
      return <CheckCircle2 size={size} style={{ color: "#22c55e" }} />;
    case "failed":
      return <XCircle size={size} style={{ color: "#ef4444" }} />;
    case "skipped":
      return <SkipForward size={size} style={{ color: "#fbbf24" }} />;
    default:
      return <Circle size={size} style={{ color: "#d4d4d8" }} />;
  }
}

/**
 * Live DAG visualization driven by SSE events.
 * Color-codes nodes by execution status: pending → running → success/failed/skipped.
 */
export function DagExecutionView({
  nodes,
  edges,
  className,
}: DagExecutionViewProps) {
  const positionedNodes = useMemo(() => {
    if (nodes.length === 0) return [];

    const g = new graphlib.Graph({ directed: true });
    g.setGraph({
      rankdir: "TB",
      nodesep: 50,
      ranksep: 70,
      marginx: 24,
      marginy: 24,
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
      };
    });
  }, [nodes, edges]);

  const bounds = useMemo(() => {
    if (positionedNodes.length === 0)
      return { w: 400, h: 200, minX: 0, minY: 0 };
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
      w: Math.max(400, maxX - minX + 60),
      h: Math.max(200, maxY - minY + 60),
      minX,
      minY,
    };
  }, [positionedNodes]);

  const offsetX = -bounds.minX + 30;
  const offsetY = -bounds.minY + 30;

  if (nodes.length === 0) {
    return (
      <div
        className={cn(
          "flex items-center justify-center rounded-md border bg-muted/10 text-sm text-muted-foreground",
          className,
        )}
        style={{ minHeight: 200 }}
      >
        No steps defined
      </div>
    );
  }

  return (
    <div className={cn("overflow-auto", className)}>
      <svg
        viewBox={`0 0 ${bounds.w} ${bounds.h}`}
        className="w-full"
        style={{ minHeight: bounds.h }}
      >
        <defs>
          <marker
            id="exec-arrowhead"
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

        {/* Edges */}
        {edges.map((edge) => {
          const from = positionedNodes.find((n) => n.id === edge.from);
          const to = positionedNodes.find((n) => n.id === edge.to);
          if (!from || !to) return null;

          const x1 = (from.x ?? 0) + offsetX;
          const y1 = (from.y ?? 0) + NODE_HEIGHT / 2 + offsetY;
          const x2 = (to.x ?? 0) + offsetX;
          const y2 = (to.y ?? 0) - NODE_HEIGHT / 2 + offsetY;
          const midY = (y1 + y2) / 2;

          const toStatus = to.status;
          const edgeColor =
            toStatus === "running"
              ? "var(--color-blue-400, #60a5fa)"
              : toStatus === "success"
                ? "var(--color-green-400, #4ade80)"
                : toStatus === "failed"
                  ? "var(--color-red-400, #f87171)"
                  : "var(--color-border, #d4d4d8)";

          return (
            <path
              key={`${edge.from}-${edge.to}`}
              d={`M ${x1} ${y1} C ${x1} ${midY}, ${x2} ${midY}, ${x2} ${y2}`}
              fill="none"
              stroke={edgeColor}
              strokeWidth={1.5}
              markerEnd="url(#exec-arrowhead)"
            />
          );
        })}

        {/* Nodes */}
        {positionedNodes.map((node) => {
          const style =
            STATUS_STYLES[node.status ?? "pending"] ?? STATUS_STYLES.pending;
          const nx = (node.x ?? 0) - NODE_WIDTH / 2 + offsetX;
          const ny = (node.y ?? 0) - NODE_HEIGHT / 2 + offsetY;
          const isRunning = node.status === "running";

          return (
            <g key={node.id}>
              <rect
                x={nx}
                y={ny}
                width={NODE_WIDTH}
                height={NODE_HEIGHT}
                rx={8}
                fill={style.bg}
                stroke={style.stroke}
                strokeWidth={isRunning ? 2.5 : 1.5}
                className={isRunning ? "animate-pulse" : ""}
              />
              {/* Status icon */}
              <foreignObject
                x={nx + 6}
                y={ny + NODE_HEIGHT / 2 - 8}
                width={16}
                height={16}
              >
                <StatusIcon status={node.status} size={14} />
              </foreignObject>
              {/* Name */}
              <text
                x={nx + 28}
                y={ny + NODE_HEIGHT / 2 - 4}
                fontSize={12}
                fontWeight={600}
                fill="currentColor"
              >
                {node.name.length > 12
                  ? node.name.slice(0, 11) + "…"
                  : node.name}
              </text>
              {/* Type label */}
              <text
                x={nx + 28}
                y={ny + NODE_HEIGHT / 2 + 12}
                fontSize={10}
                fill="currentColor"
                opacity={0.5}
              >
                {node.type}
              </text>
            </g>
          );
        })}
      </svg>
    </div>
  );
}
