import { describe, expect, it } from "vitest";
import {
  dockFormOf,
  WORK_INLINE_MIN,
  workWidthOf,
} from "@/components/layout/shellMetrics";

// 三档取值都是推出来的常量：右栏内联的切换点 824 = 320(右栏) + 480(聊天列) + 24(gutter)，
// 这里把它们钉住，改栅格时两处一起动的约束才有人守着。

describe("壳的分档", () => {
  it("dock 形态按「视口 − rail」判：1440 右列、1280 底部、820 覆盖 Sheet", () => {
    // 1440 − 208 = 1232 是右列的下限。
    expect(dockFormOf(1440, true)).toBe("column");
    expect(dockFormOf(1440, false)).toBe("column");
    expect(dockFormOf(1280, true)).toBe("bottom");
    expect(dockFormOf(820, false)).toBe("sheet");
  });

  it("work 扣掉 rail 与占右列的 dock", () => {
    // 1440 − 208 − 360 = 872：右栏阈值（824）不跟着 rail 收窄，于是富余 48。
    expect(workWidthOf(1440, true)).toBe(872);
    expect(workWidthOf(1440, true) - WORK_INLINE_MIN).toBe(48);
    // rail 收起后 shell = 1376 仍在 1184 以上：dock 继续占右列，只是 rail 变窄。
    expect(workWidthOf(1440, false)).toBe(1016);
    // 1280 时 dock 已退出右列，只剩 rail。
    expect(workWidthOf(1280, true)).toBe(1072);
    // 900 是 e2e 的窄档：rail 收到 64，右栏仍内联。
    expect(workWidthOf(900, false)).toBe(836);
    // 820 时 rail 离屏，work 就等于视口：差 4px 进抽屉。
    expect(workWidthOf(820, false)).toBe(820);
  });
});
