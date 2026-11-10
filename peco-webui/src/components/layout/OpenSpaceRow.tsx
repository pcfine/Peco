// 打开集的一行：主点击进聊天，「×」只移出打开集、不关闭空间。
// 状态页从工作室列表进入；左栏承担「打开集 + 切换」。

import { X } from "lucide-react";
import {
  SpaceStatusMark,
  spaceGroupOf,
} from "@/components/layout/SpaceStatusMark";
import { spaceInitial, spaceLabel } from "@/components/layout/spaceLabel";
import { cn } from "@/lib/utils";
import type { Space } from "@/types/daemon";

interface OpenSpaceRowProps {
  space: Space;
  /** 收起态（rail 64）只留名字首字：文字没了，`title` 与可见文本等价。 */
  collapsed?: boolean;
  /** 当前路由是否停在这个空间上：决定边框，不决定圆点。 */
  active: boolean;
  /** 最近打开的那一个：蓝点只点在这一行。 */
  recent: boolean;
  onSelect: () => void;
  onRemove: () => void;
}

export function OpenSpaceRow({
  space,
  collapsed = false,
  active,
  recent,
  onSelect,
  onRemove,
}: OpenSpaceRowProps) {
  const name = spaceLabel(space);
  const inflight = space.inflight_dispatch + space.inflight_foreign;

  return (
    // 圆角矩形落在行容器上：主点击区与「×」同属一行，框只画一次。
    // `group` 是给「×」用的：删除只在指针进入这一行（或键盘焦点落进来）时出现。
    <div
      className={cn(
        "group mx-2 flex items-center rounded-[6px] border transition-colors duration-[120ms]",
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
        {collapsed ? (
          <span className="grid w-4 shrink-0 place-items-center text-caption font-medium">
            {spaceInitial(space)}
          </span>
        ) : (
          <SpaceStatusMark group={spaceGroupOf(space)} recent={recent} />
        )}
        {!collapsed && (
          <>
            <span className="min-w-0 truncate text-body">{name}</span>
            {inflight > 0 && (
              <span
                aria-label={`${inflight} 个任务在飞`}
                title={`${inflight} 个任务在飞`}
                className="ml-auto inline-flex h-4 min-w-4 shrink-0 items-center justify-center rounded-full bg-brand/15 px-1 tabular-nums text-2xs font-semibold text-primary-ink"
              >
                {inflight}
              </span>
            )}
          </>
        )}
      </button>
      {!collapsed && (
        <button
          type="button"
          onClick={onRemove}
          aria-label="从打开列表移除"
          title="从打开列表移除"
          className="mr-1 grid h-6 w-6 shrink-0 place-items-center rounded-[4px] text-ink-faint opacity-0 transition-opacity duration-[120ms] hover:bg-line hover:text-ink group-hover:opacity-100 group-focus-within:opacity-100 focus-visible:opacity-100"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  );
}
