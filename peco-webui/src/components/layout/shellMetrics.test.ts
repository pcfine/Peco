import { describe, expect, it } from "vitest";
import {
  DOCK_DRAWER_WIDTH,
  WORK_INLINE_MIN,
  workWidthOf,
} from "@/components/layout/shellMetrics";

// 两个常量都是推出来的：824 = 412(双开每块) × 2，400 是抽屉宽度上限。
// 这里把它们钉住，改栅格或改抽屉宽度时两处一起动的约束才有人守着。

describe("壳的分档", () => {
  it("抽屉宽 400，与 DockShell 的 min(92vw, 400px) 同值", () => {
    expect(DOCK_DRAWER_WIDTH).toBe(400);
  });

  it("work = 视口 − rail：抽屉是覆盖层，不再扣一列", () => {
    // 1440 − 208 = 1232：互斥阈值（824）不跟着 rail 走，于是富余 408。
    expect(workWidthOf(1440, true)).toBe(1232);
    expect(workWidthOf(1440, true) - WORK_INLINE_MIN).toBe(408);
    // rail 收起后只剩 64：work 跟着变宽，双开照样成立。
    expect(workWidthOf(1440, false)).toBe(1376);
    expect(workWidthOf(1280, true)).toBe(1072);
    // 900 是 e2e 的窄档：rail 收到 64，两列还放得下（836 ≥ 824）。
    expect(workWidthOf(900, false)).toBe(836);
    // 820 时 rail 离屏，work 就等于视口：差 4px 进抽屉。
    expect(workWidthOf(820, false)).toBe(820);
  });
});
