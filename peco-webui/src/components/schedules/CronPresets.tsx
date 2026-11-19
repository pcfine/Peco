// cron 预设只认这几条固定表达式。命不中就只显示原文 —— 把 `*/15 8-18 * * 1-5`
// 猜成一句话，猜错的概率比猜对高。

import { cn } from "@/lib/utils";

export const CRON_PRESETS = [
  { expr: "0 9 * * *", label: "每天 9:00" },
  { expr: "0 9 * * 1", label: "每周一 9:00" },
  { expr: "0 * * * *", label: "每小时" },
] as const;

/** 预设的人读名；返回 `null` 表示这条表达式不在预设里。 */
export function cronPresetLabel(expr: string): string | null {
  const trimmed = expr.trim();
  const preset = CRON_PRESETS.find((item) => item.expr === trimmed);
  return preset ? preset.label : null;
}

interface CronPresetsProps {
  value: string;
  onPick: (expr: string) => void;
}

const CHIP_CLASS =
  "inline-flex h-8 items-center rounded-chip px-2 text-caption font-medium transition-colors";

export function CronPresets({ value, onPick }: CronPresetsProps) {
  const current = value.trim();
  const custom = current !== "" && cronPresetLabel(current) === null;

  return (
    <div className="flex flex-wrap items-center gap-1">
      {CRON_PRESETS.map((preset) => {
        const active = current === preset.expr;
        return (
          <button
            key={preset.expr}
            type="button"
            aria-pressed={active}
            onClick={() => onPick(preset.expr)}
            className={cn(
              CHIP_CLASS,
              active
                ? "bg-line text-ink"
                : "text-ink-muted hover:bg-line hover:text-ink",
            )}
          >
            {preset.label}
          </button>
        );
      })}
      {/* 「自定义」只在表达式不在预设里时出现：它是状态标记，没有可点的动作。 */}
      {custom && (
        <span className={cn(CHIP_CLASS, "bg-line text-ink")}>自定义</span>
      )}
    </div>
  );
}
