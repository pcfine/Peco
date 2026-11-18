import type { Space } from "@/types/daemon";

type SpaceNames = Pick<Space, "name" | "label" | "work_dir">;

/**
 * 空间显示名：`name`（marker 的人读名）→ `label`（库里的逻辑名）→ 目录末段。
 *
 * 不显示 ws_id：同名 `label` 可以有多个，目录才是身份。空串与纯空白视同缺失
 *（后端会过滤，缺字段的响应与 mock 不会）。
 */
export function spaceLabel(space: SpaceNames): string {
  const name = space.name?.trim();
  if (name) return name;
  const label = space.label?.trim();
  if (label) return label;
  const parts = space.work_dir.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? space.work_dir;
}

/**
 * 收起态的单字标记：取显示名的第一个「字」。
 *
 * 用码点而不是下标切：emoji 之类是代理对，按 UTF-16 切会留下半个字。
 */
export function spaceInitial(space: SpaceNames): string {
  return Array.from(spaceLabel(space).trim())[0] ?? "·";
}
