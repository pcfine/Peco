// 页头右上角的右栏按钮组。
//
// 收起态下点任一模式都是「展开并切到这个模式」—— 用户点的是「看任务」，
// 不是「切模式」；先展开再切让一次点击就到目的地。

import { PanelRightClose, PanelRightOpen } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  SPACE_PANEL_LABELS,
  SPACE_PANEL_MODES,
  type SpacePanelMode,
} from "@/components/spaces/useSpacePanelState";
import { cn } from "@/lib/utils";

interface SpacePanelModeBarProps {
  mode: SpacePanelMode;
  collapsed: boolean;
  /** 在飞任务数；0 时不出角标 —— 「任务 0」比不写更吵。 */
  inflight: number;
  onSelect: (mode: SpacePanelMode) => void;
  onToggleCollapsed: () => void;
}

export function SpacePanelModeBar({
  mode,
  collapsed,
  inflight,
  onSelect,
  onToggleCollapsed,
}: SpacePanelModeBarProps) {
  return (
    <div role="group" aria-label="右栏" className="flex items-center gap-1">
      {SPACE_PANEL_MODES.map((item) => (
        <button
          key={item}
          type="button"
          aria-pressed={!collapsed && mode === item}
          onClick={() => onSelect(item)}
          className={cn(
            "h-7 shrink-0 rounded-[4px] px-2 text-2xs text-ink-muted hover:text-ink",
            !collapsed && mode === item && "bg-line text-ink",
          )}
        >
          {SPACE_PANEL_LABELS[item]}
          {item === "tasks" && inflight > 0 && (
            <span className="ml-1 tabular-nums text-ink">{inflight}</span>
          )}
        </button>
      ))}
      <Button
        variant="ghost"
        size="icon-sm"
        className="ml-1 size-7"
        aria-label={collapsed ? "展开右栏" : "收起右栏"}
        aria-expanded={!collapsed}
        onClick={onToggleCollapsed}
      >
        {collapsed ? (
          <PanelRightOpen className="h-4 w-4" />
        ) : (
          <PanelRightClose className="h-4 w-4" />
        )}
      </Button>
    </div>
  );
}
