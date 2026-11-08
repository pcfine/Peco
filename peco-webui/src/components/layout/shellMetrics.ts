// 壳的分档口径：dock 何时占右列、main 内容宽（work）还剩多少。
//
// 与 `AppShell` 的 `@min-[1184px]/shell` 栅格、`grid-cols-[minmax(0,1fr)_360px]`
// 是同一组数：改这里必须同时改那两处，否则抽屉与内联的切换点会跟栅格错开。

import { railWidth } from "@/stores/uiStore";

/** dock 占右列时的宽度，与 `AppShell` 第二列的 `360px` 同值。 */
export const DOCK_COLUMN_WIDTH = 360;

/** work 的下限：320（右栏）+ 480（聊天列可用宽）+ 24（列 gutter）。 */
export const WORK_INLINE_MIN = 824;

export type DockForm = "column" | "bottom" | "sheet";

/**
 * dock 形态只由「视口 − rail」判定：容器宽已经把 dock 自己算进去，用它判定会自指。
 */
export function dockFormOf(viewport: number, railOpen: boolean): DockForm {
  if (viewport < 900) return "sheet";
  return viewport - railWidth(viewport, railOpen) >= 1184 ? "column" : "bottom";
}

/**
 * main 内容宽 = 视口 − rail − 占右列的 dock。
 *
 * 工作台页的 main 没有内边距（列 gutter 在各列内部），所以这个值就是
 * `@container/work` 的宽度，右栏内联不下时据此改抽屉。
 */
export function workWidthOf(viewport: number, railOpen: boolean): number {
  const dockColumn = dockFormOf(viewport, railOpen) === "column";
  return (
    viewport -
    railWidth(viewport, railOpen) -
    (dockColumn ? DOCK_COLUMN_WIDTH : 0)
  );
}

/** 当前形态下 dock 的元素 id：TopBar 的 `aria-controls` 必须指向真实存在的那一个。 */
export function dockElementId(viewport: number, railOpen: boolean): string {
  const form = dockFormOf(viewport, railOpen);
  if (form === "column") return "peco-dock";
  if (form === "bottom") return "peco-dock-bottom";
  return "peco-dock-sheet";
}
