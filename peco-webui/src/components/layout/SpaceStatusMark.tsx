// 生命周期用字面符号编码，不用色块：符号 + aria-label 双写，读屏也读得懂。
// 打开态用一块同宽槽位：最近打开的那个点蓝点，其余留空 —— 行文字始终与栏目图标对齐。

import type { Space } from "@/types/daemon";

export type SpaceGroup = "running" | "dormant" | "closed";

/**
 * 三组的分组依据是数据属性，不是用户可拖的分组。
 *
 * `/api/spaces` 只回 `status`（是否在 Hub 里）与 `closed_at`：`closed` 且没被
 * 显式关闭过 = 库里有行、本次没开，正是「有记录但未打开」。
 */
export function spaceGroupOf(
  space: Pick<Space, "status" | "closed_at">,
): SpaceGroup {
  if (space.status === "open") return "running";
  if (space.closed_at) return "closed";
  return "dormant";
}

const MARKS: Record<
  Exclude<SpaceGroup, "running">,
  { glyph: string; label: string; className: string }
> = {
  dormant: { glyph: "◐", label: "未打开", className: "text-ink-muted" },
  closed: { glyph: "⨯", label: "已关闭", className: "text-ink-faint" },
};

/**
 * 已打开（running）只在最近打开的那一行点蓝点；其余留同宽空槽，文字不挪位。
 *
 * 有任务在飞时换成 `◉` 并压过 `recent`：`●` 说的是「最近/当前」，不兼任「在跑」，
 * 两者挤进同一个符号就没人分得清这一行是在忙还是刚点过。
 */
export function SpaceStatusMark({
  group,
  recent = false,
  inflight = 0,
}: {
  group: SpaceGroup;
  recent?: boolean;
  /** 在飞任务数（含外来）；0 不显示。 */
  inflight?: number;
}) {
  if (group === "running") {
    if (inflight > 0) {
      const label = `在运行 · ${inflight} 个任务在飞`;
      return (
        <span
          role="img"
          aria-label={label}
          title={label}
          className="w-4 shrink-0 text-center text-xs text-brand"
        >
          ◉
        </span>
      );
    }
    return (
      <span className="grid w-4 shrink-0 place-items-center">
        {recent && (
          <span
            role="img"
            aria-label="最近打开"
            title="最近打开"
            className="text-xs text-brand"
          >
            ●
          </span>
        )}
      </span>
    );
  }
  const mark = MARKS[group];
  return (
    <span
      role="img"
      aria-label={mark.label}
      title={mark.label}
      className={`w-4 shrink-0 text-center text-xs ${mark.className}`}
    >
      {mark.glyph}
    </span>
  );
}

export const SPACE_GROUP_LABELS: Record<SpaceGroup, string> = {
  running: "已打开",
  dormant: "未打开",
  closed: "已关闭",
};
