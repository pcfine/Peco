// 打开集的一行：主点击进聊天，状态图标进状态页，「×」只移出打开集 —— 不关闭空间。
// 三个动作都是原生 button/link，键盘与中键新开标签都天然可用。

import { Link } from "react-router-dom";
import { Activity, X } from "lucide-react";
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
    // 圆角矩形落在行容器上：主点击区、状态、「×」同属一行，框只画一次。
    <div
      className={cn(
        "mx-2 flex items-center rounded-[6px] border transition-colors duration-[120ms]",
        active
          ? "border-brand/40 bg-brand/10"
          : "border-transparent hover:bg-line/50",
      )}
    >
      <button
        type="button"
        onClick={onSelect}
        aria-current={active ? "true" : undefined}
        aria-label={`${name}（团队 ${space.team_id}）`}
        title={name}
        data-ws-id={space.ws_id}
        data-space-row=""
        className={cn(
          "flex h-8 min-w-0 flex-1 items-center gap-2 rounded-[6px] font-normal",
          collapsed ? "justify-center px-1" : "px-2",
          active ? "text-ink" : "text-ink-muted hover:text-ink",
        )}
      >
        <SpaceStatusMark group={spaceGroupOf(space)} />
        {!collapsed && (
          <>
            <span className="min-w-0 truncate text-body">{name}</span>
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
          title={`${name} 状态`}
          className="grid h-6 w-6 shrink-0 place-items-center rounded-[4px] text-ink-faint hover:bg-line hover:text-ink"
        >
          <Activity className="h-3.5 w-3.5" />
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
