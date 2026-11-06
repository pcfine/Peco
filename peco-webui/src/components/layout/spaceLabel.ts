import type { Space } from "@/types/daemon";

/**
 * 空间显示名：优先 `label`，缺省退回目录末段。
 *
 * 目录才是空间的身份（同名 label 可以有多个），所以缺 label 时也不显示 ws_id。
 */
export function spaceLabel(space: Pick<Space, "label" | "work_dir">): string {
  if (space.label) return space.label;
  const parts = space.work_dir.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? space.work_dir;
}
