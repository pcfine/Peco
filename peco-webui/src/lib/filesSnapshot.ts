// 目录列表的「上一份」指纹与差异：文件面板据此判断哪些行是刚出现或刚被改过的。
//
// 指纹只取 (kind, modified_at, size_bytes)：逐字节比内容得把每个文件读一遍，
// 一个只读浏览面板不值得为高亮付这个代价。

import type { SpaceFileEntry } from "@/types/daemon";

export type FileEntryMark = "new" | "updated";

/** 一次目录列举的指纹：名字 → 该行的大小与时刻。 */
export type FileDirSnapshot = ReadonlyMap<string, string>;

/** 高亮留多久：够看清是哪一行，又不至于让面板长期看着「还在动」。 */
export const FILE_HIGHLIGHT_MS = 8_000;

function signature(entry: SpaceFileEntry): string {
  return `${entry.kind}|${entry.modified_at ?? ""}|${entry.size_bytes ?? ""}`;
}

export function snapshotEntries(
  entries: readonly SpaceFileEntry[],
): FileDirSnapshot {
  return new Map(entries.map((entry) => [entry.name, signature(entry)]));
}

/**
 * 本次列表相对上一份快照的变化，按名字索引。
 *
 * 没有上一份快照（第一次看这个目录）时返回空：整屏都是基准，标成「新增」
 * 只会把真正的变化淹掉。
 */
export function diffEntries(
  previous: FileDirSnapshot | undefined,
  entries: readonly SpaceFileEntry[],
): Map<string, FileEntryMark> {
  const marks = new Map<string, FileEntryMark>();
  if (!previous) return marks;
  for (const entry of entries) {
    const before = previous.get(entry.name);
    if (before === undefined) marks.set(entry.name, "new");
    else if (before !== signature(entry)) marks.set(entry.name, "updated");
  }
  return marks;
}
