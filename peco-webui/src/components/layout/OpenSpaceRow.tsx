// 打开集的一行：主点击进聊天，「状态」进状态页，「×」只移出打开集 —— 不关闭空间。
// 三个动作都是原生 button/link，键盘与中键新开标签都天然可用。

import { Link } from "react-router-dom";
import { X } from "lucide-react";
import {
  SpaceStatusMark,
  spaceGroupOf,
} from "@/components/layout/SpaceStatusMark";
import { spaceLabel } from "@/components/layout/spaceLabel";
import { cn } from "@/lib/utils";
import type { Space } from "@/types/daemon";

interface OpenSpaceRowProps {
  space: Space;
  /** 收起态（rail 64）只留状态点：文字没了，`title` 与可见文本等价。 */
  collapsed?: boolean;
  active: boolean;
  onSelect: () => void;
  onRemove: () => void;
}

export function OpenSpaceRow({
  space,
  collapsed = false,
  active,
  onSelect,
  onRemove,
}: OpenSpaceRowProps) {
  const name = spaceLabel(space);

  return (
    <div
      className={cn(
        "relative flex items-center transition-colors duration-[120ms]",
        active ? "bg-line/60" : "hover:bg-line/40",
      )}
    >
      {active && (
        <span
          data-testid="rail-active-bar"
          className="absolute inset-y-1 left-0 w-[3px] rounded-full bg-brand"
        />
      )}
      <button
        type="button"
        onClick={onSelect}
        aria-current={active ? "true" : undefined}
        aria-label={`${name}（团队 ${space.team_id}）`}
        title={name}
        data-ws-id={space.ws_id}
        data-space-row=""
        className={cn(
          "flex h-8 min-w-0 flex-1 items-center gap-2",
          collapsed ? "justify-center px-1" : "pl-3",
          active ? "text-ink" : "text-ink-muted",
        )}
      >
        <SpaceStatusMark group={spaceGroupOf(space)} />
        {!collapsed && (
          <>
            <span className="min-w-0 truncate text-sm">{name}</span>
            {space.inflight_dispatch > 0 && (
              <span className="ml-auto shrink-0 tabular-nums text-2xs text-ink-faint">
                {space.inflight_dispatch}
              </span>
            )}
          </>
        )}
      </button>
      {!collapsed && (
        <Link
          to={`/spaces/${encodeURIComponent(space.ws_id)}/status`}
          aria-label={`${name} 状态`}
          className="grid h-6 shrink-0 place-items-center rounded-[4px] px-1 text-2xs text-ink-faint hover:bg-line hover:text-ink"
        >
          状态
        </Link>
      )}
      {!collapsed && (
        <button
          type="button"
          onClick={onRemove}
          aria-label="从打开列表移除"
          title="从打开列表移除"
          className="mr-1 grid h-6 w-6 shrink-0 place-items-center rounded-[4px] text-ink-faint hover:bg-line hover:text-ink"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  );
}
