// 打开集的一行：主点击进聊天，「×」只移出打开集、不关闭空间。
// 关闭与删除都在工作室列表；左栏承担「打开集 + 切换」。

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
  // 收起态没有位置摆数量徽章，展开态的徽章又被按钮自己的 aria-label 盖掉：
  // 两种形态都得把在飞数写进按钮的名字，读屏才拿得到。
  const busy = collapsed && inflight > 0;
  const label = `${name}（团队 ${space.team_id}）${
    inflight > 0 ? ` · ${inflight} 个任务在飞` : ""
  }`;

  return (
    // 圆角矩形落在行容器上：主点击区与「×」同属一行，框只画一次。
    // `group` 是给「×」用的：删除只在指针进入这一行（或键盘焦点落进来）时出现。
    <div
      className={cn(
        "group relative mx-2 flex items-center rounded-lg border transition-colors",
        active
          ? "border-brand/40 bg-brand/10"
          : "border-transparent hover:bg-line/50",
      )}
    >
      {busy && (
        <span
          aria-hidden="true"
          className="pointer-events-none absolute top-1 right-1 h-1.5 w-1.5 rounded-full bg-brand"
        />
      )}
      <button
        type="button"
        onClick={onSelect}
        aria-current={active ? "true" : undefined}
        aria-label={label}
        title={inflight > 0 ? label : name}
        data-ws-id={space.ws_id}
        data-space-row=""
        className={cn(
          "flex h-8 min-w-0 flex-1 items-center gap-2 rounded-lg font-normal",
          collapsed ? "justify-center px-1" : "px-2",
          active ? "text-ink" : "text-ink-muted hover:text-ink",
        )}
      >
        {collapsed ? (
          <span className="grid w-4 shrink-0 place-items-center text-caption font-medium">
            {spaceInitial(space)}
          </span>
        ) : (
          <SpaceStatusMark
            group={spaceGroupOf(space)}
            recent={recent}
            inflight={inflight}
          />
        )}
        {!collapsed && (
          <>
            <span className="min-w-0 truncate text-body">{name}</span>
            {inflight > 0 && (
              <span
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
          className="mr-1 grid h-6 w-6 shrink-0 place-items-center rounded-md text-ink-faint opacity-0 transition-opacity duration-[120ms] hover:bg-line hover:text-ink group-hover:opacity-100 group-focus-within:opacity-100 focus-visible:opacity-100"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  );
}
