// 看板组合的开关面板：不拖拽（12 栅格上落点不可预测，键盘也无对等操作），
// 用「显示开关 + ↑↓」表达同一件事。

import { ArrowDown, ArrowUp, SlidersHorizontal } from "lucide-react";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Switch } from "@/components/ui/switch";
import { BOARD_PANEL_LABELS, type BoardPanelId } from "@/lib/board";
import { useBoardStore } from "@/stores/boardStore";

export function BoardEditorPopover() {
  const order = useBoardStore((state) => state.order);
  const hidden = useBoardStore((state) => state.hidden);
  const toggle = useBoardStore((state) => state.toggle);
  const move = useBoardStore((state) => state.move);

  const visibleCount = order.length - hidden.length;

  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          aria-haspopup="dialog"
          className="flex h-7 items-center gap-1.5 rounded-md px-2 text-xs text-ink-muted hover:bg-line hover:text-ink"
        >
          <SlidersHorizontal className="h-3.5 w-3.5" />
          调整看板
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-60 p-1.5">
        {order.map((id: BoardPanelId, index) => {
          const isHidden = hidden.includes(id);
          const lastVisible = !isHidden && visibleCount <= 1;
          return (
            <div key={id} className="flex h-8 items-center gap-2 px-1.5">
              <Switch
                checked={!isHidden}
                disabled={lastVisible}
                title={lastVisible ? "至少保留一块面板" : undefined}
                aria-label={`显示${BOARD_PANEL_LABELS[id]}`}
                onCheckedChange={() => toggle(id)}
              />
              <span className="flex-1 truncate text-sm text-ink">
                {BOARD_PANEL_LABELS[id]}
              </span>
              <button
                type="button"
                aria-label={`${BOARD_PANEL_LABELS[id]}上移`}
                disabled={index === 0}
                onClick={() => move(id, -1)}
                className="grid h-6 w-6 place-items-center rounded-sm text-ink-faint hover:bg-line hover:text-ink disabled:pointer-events-none disabled:opacity-40"
              >
                <ArrowUp className="h-3.5 w-3.5" />
              </button>
              <button
                type="button"
                aria-label={`${BOARD_PANEL_LABELS[id]}下移`}
                disabled={index === order.length - 1}
                onClick={() => move(id, 1)}
                className="grid h-6 w-6 place-items-center rounded-sm text-ink-faint hover:bg-line hover:text-ink disabled:pointer-events-none disabled:opacity-40"
              >
                <ArrowDown className="h-3.5 w-3.5" />
              </button>
            </div>
          );
        })}
      </PopoverContent>
    </Popover>
  );
}
