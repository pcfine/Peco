// 页头右上角的右栏按钮组。
//
// 收起态下点任一模式都是「展开并切到这个模式」—— 用户点的是「看任务」，
// 不是「切模式」；先展开再切让一次点击就到目的地。
//
// 窄屏（work < 824）右栏是抽屉，这枚 chevron 就是它唯一的开关入口：抽屉盖住整条
// 页头，打开后页头点不到，收起来得靠抽屉内的关闭按钮、遮罩或 Esc。

import { PanelRightClose, PanelRightOpen } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  SPACE_PANEL_LABELS,
  type SpacePanelMode,
} from "@/components/spaces/useSpacePanelState";
import { cn } from "@/lib/utils";

interface SpacePanelModeBarProps {
  mode: SpacePanelMode;
  /** 这一栏此刻是否在用户眼前：内联态 = 未收起，窄屏 = 抽屉打开。 */
  expanded: boolean;
  /** 窄屏抽屉态：chevron 说的是「打开 / 收起」，内联态说「展开 / 收起」。 */
  drawer?: boolean;
  /** 抽屉关闭后焦点要回到这枚 chevron。 */
  triggerRef?: React.Ref<HTMLButtonElement>;
  /** 在飞任务数；0 时不出角标 —— 「任务 0」比不写更吵。 */
  inflight: number;
  onSelect: (mode: SpacePanelMode) => void;
  onToggle: () => void;
  /** 可选的模式集合：Peco 页多一个「总览」，两处都不该猜默认值。 */
  modes: readonly SpacePanelMode[];
}

export function SpacePanelModeBar({
  mode,
  expanded,
  drawer = false,
  triggerRef,
  inflight,
  onSelect,
  onToggle,
  modes,
}: SpacePanelModeBarProps) {
  const toggleLabel = expanded ? "收起右栏" : drawer ? "打开右栏" : "展开右栏";

  return (
    <div role="group" aria-label="右栏" className="flex items-center gap-1">
      {modes.map((item) => (
        <button
          key={item}
          type="button"
          aria-pressed={expanded && mode === item}
          onClick={() => onSelect(item)}
          className={cn(
            "h-7 shrink-0 rounded-full border px-2 text-caption font-medium",
            expanded && mode === item
              ? "border-brand/40 bg-brand/10 text-ink"
              : "border-transparent text-ink-muted hover:bg-line/50 hover:text-ink",
          )}
        >
          {SPACE_PANEL_LABELS[item]}
          {item === "tasks" && inflight > 0 && (
            <span className="ml-1 tabular-nums text-ink">{inflight}</span>
          )}
        </button>
      ))}
      <Button
        ref={triggerRef}
        variant="ghost"
        size="icon-sm"
        className="ml-1 size-7"
        aria-label={toggleLabel}
        aria-expanded={expanded}
        onClick={onToggle}
      >
        {expanded ? (
          <PanelRightClose className="h-4 w-4" />
        ) : (
          <PanelRightOpen className="h-4 w-4" />
        )}
      </Button>
    </div>
  );
}
