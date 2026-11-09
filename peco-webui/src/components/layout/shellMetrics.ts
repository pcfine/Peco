// 壳的分档口径：Peco 对话抽屉的宽度上限、main 内容宽（work）还剩多少。

import { railWidth } from "@/stores/uiStore";

/** 抽屉宽度上限，与 `DockShell` 的 `min(92vw, 400px)` 是同一组数。 */
export const DOCK_DRAWER_WIDTH = 400;

/** work 的下限：双开均分后每块 ≥412（824 = 412 × 2）。 */
export const WORK_INLINE_MIN = 824;

/**
 * main 内容宽 = 视口 − rail。
 *
 * 工作台页的 main 没有内边距（列 gutter 在各列内部），所以这个值就是
 * `@container/work` 的宽度，右栏内联不下时据此改抽屉。Peco 抽屉是覆盖层，不占布局。
 */
export function workWidthOf(viewport: number, railOpen: boolean): number {
  return Math.max(0, viewport - railWidth(viewport, railOpen));
}
