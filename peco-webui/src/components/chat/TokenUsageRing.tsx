// TokenUsageRing — 上下文窗口用量圆环。默认仅显示圆环，百分比与具体用量在 hover 时展示。

import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";

interface TokenUsageRingProps {
  /** 当前已用上下文 token 数（input_tokens）。 */
  inputTokens: number;
  /** 上下文窗口总量（token）。 */
  contextWindow: number;
  /** 本次输出 token 数（可选，仅用于 hover 提示）。 */
  outputTokens?: number;
}

const RADIUS = 9.5;
const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

/** 根据用量百分比选择圆环颜色。 */
function colorClassFor(pct: number): string {
  if (pct >= 90) return "stroke-red-500";
  if (pct >= 70) return "stroke-amber-500";
  return "stroke-primary";
}

/** 紧凑 token 格式化：45000 → "45k"，1000000 → "1M"，800 → "800"。 */
function formatTokens(n: number): string {
  if (n >= 1_000_000) return trimUnit(n / 1_000_000, "M");
  if (n >= 1_000) return trimUnit(n / 1_000, "k");
  return String(n);
}

/** 保留一位小数并去掉尾随 .0，再拼接单位。 */
function trimUnit(value: number, unit: string): string {
  const rounded = Math.round(value * 10) / 10;
  const text = Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
  return `${text}${unit}`;
}

export function TokenUsageRing({
  inputTokens,
  contextWindow,
  outputTokens = 0,
}: TokenUsageRingProps) {
  const safeInput = Math.max(inputTokens, 0);
  const ratio = contextWindow > 0 ? Math.min(safeInput / contextWindow, 1) : 0;
  const pct = safeInput > 0 ? Math.max(1, Math.round(ratio * 100)) : 0;
  const dashOffset = CIRCUMFERENCE * (1 - ratio);

  const tooltip = `${pct}% · ${formatTokens(inputTokens)} / ${formatTokens(
    contextWindow,
  )} tokens${outputTokens > 0 ? ` · output ${formatTokens(outputTokens)}` : ""}`;

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <svg
          viewBox="0 0 24 24"
          className="h-6 w-6 shrink-0 cursor-help"
          role="img"
          aria-label={`上下文用量 ${pct}%`}
        >
          <g transform="rotate(-90 12 12)">
            <circle
              cx={12}
              cy={12}
              r={RADIUS}
              fill="none"
              strokeWidth={3}
              className="stroke-muted-foreground/20"
            />
            <circle
              cx={12}
              cy={12}
              r={RADIUS}
              fill="none"
              strokeWidth={3}
              strokeLinecap="round"
              strokeDasharray={CIRCUMFERENCE}
              strokeDashoffset={dashOffset}
              className={colorClassFor(pct)}
            />
          </g>
        </svg>
      </TooltipTrigger>
      <TooltipContent>{tooltip}</TooltipContent>
    </Tooltip>
  );
}
