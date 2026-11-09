// 生命周期用字面符号编码，不用色块：符号 + aria-label 双写，读屏也读得懂。

import type { Space } from "@/types/daemon";

export type SpaceGroup = "running" | "dormant" | "closed";

/**
 * 三组的分组依据是数据属性，不是用户可拖的分组。
 *
 * `/api/spaces` 只回 `status`（是否在 Hub 里）与 `closed_at`：`closed` 且没被
 * 显式关闭过 = 库里有行、本次没开，正是「已存在」。
 */
export function spaceGroupOf(
  space: Pick<Space, "status" | "closed_at">,
): SpaceGroup {
  if (space.status === "open") return "running";
  if (space.closed_at) return "closed";
  return "dormant";
}

const MARKS: Record<
  SpaceGroup,
  { glyph: string; label: string; className: string }
> = {
  running: { glyph: "●", label: "进行中", className: "text-brand" },
  dormant: { glyph: "◐", label: "已存在", className: "text-ink-muted" },
  closed: { glyph: "⨯", label: "已关闭", className: "text-ink-faint" },
};

export function SpaceStatusMark({ group }: { group: SpaceGroup }) {
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
  running: "进行中",
  dormant: "已存在",
  closed: "已关闭",
};
