// 空间总线的一行：活动条 / 名称 / 团队 / 在飞。整行是按钮 —— 切换空间不是路由跳转。

import { cn } from "@/lib/utils";
import type { Space } from "@/types/daemon";
import { spaceLabel } from "@/components/layout/spaceLabel";

export interface SpaceBusModel {
  wsId: string;
  name: string;
  teamId: string;
  inflight: number;
}

/** 总线行的数据装配：名称与在飞数都从同一个 space 行取，不另算一份。 */
export function spaceBusModel(space: Space): SpaceBusModel {
  return {
    wsId: space.ws_id,
    name: spaceLabel(space),
    teamId: space.team_id,
    inflight: space.inflight_dispatch,
  };
}

interface SpaceBusRowProps {
  model: SpaceBusModel;
  /** 收缩态（rail 64）只留首字，团队与计数收起。 */
  collapsed?: boolean;
  active: boolean;
  onSelect: () => void;
}

export function SpaceBusRow({
  model,
  collapsed = false,
  active,
  onSelect,
}: SpaceBusRowProps) {
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-current={active ? "true" : undefined}
      aria-label={`${model.name}（团队 ${model.teamId}）`}
      data-ws-id={model.wsId}
      data-space-row=""
      className={cn(
        "relative flex w-full items-center transition-colors duration-[120ms]",
        collapsed ? "h-8 justify-center" : "h-8 gap-2 pl-3 pr-2",
        active ? "bg-line/60 text-ink" : "text-ink-muted hover:bg-line/40",
      )}
    >
      {active && (
        <span
          data-testid="rail-active-bar"
          className="absolute inset-y-1 left-0 w-[3px] rounded-full bg-brand transition-transform duration-[120ms]"
        />
      )}
      {collapsed ? (
        <span className="text-sm">{model.name.slice(0, 1)}</span>
      ) : (
        <>
          <span className="truncate text-sm">{model.name}</span>
          <span className="ml-auto shrink-0 font-mono text-2xs text-ink-faint">
            {model.teamId}
          </span>
          <span className="w-5 shrink-0 text-right tabular-nums text-2xs text-ink-faint">
            {model.inflight > 0 ? model.inflight : "—"}
          </span>
        </>
      )}
    </button>
  );
}
