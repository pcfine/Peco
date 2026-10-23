// 气泡时间戳分档 — 纯函数用例，now 显式注入以免「昨天」随运行时刻漂移

import { describe, expect, it } from "vitest";
import { formatDuration, formatMessageTime } from "../time";

/** 本地构造：`Date.parse("...Z")` 在不同时区会落到不同的本地日历日。 */
const at = (y: number, m: number, d: number, h = 10, min = 0) =>
  new Date(y, m, d, h, min).getTime();

const NOW = at(2026, 2, 1);

describe("formatMessageTime 四档", () => {
  it("同日只显示时分", () => {
    expect(formatMessageTime(at(2026, 2, 1, 14, 32), NOW)?.label).toBe("14:32");
  });

  it("昨天带日期前缀", () => {
    expect(formatMessageTime(at(2026, 1, 28, 14, 32), NOW)?.label).toBe(
      "昨天 14:32",
    );
  });

  it("同年更早显示月日与时分", () => {
    expect(formatMessageTime(at(2026, 0, 5, 9, 5), NOW)?.label).toBe(
      "01-05 09:05",
    );
  });

  it("跨年显示完整年月日", () => {
    expect(formatMessageTime(at(2025, 9, 21, 14, 32), NOW)?.label).toBe(
      "2025-10-21 14:32",
    );
  });
});

describe("formatMessageTime 昨天边界", () => {
  it("昨天跨月仍算昨天，不退化成月日档", () => {
    expect(
      formatMessageTime(at(2026, 1, 28, 10, 0), at(2026, 2, 1, 10, 0))?.label,
    ).toBe("昨天 10:00");
  });

  it("昨天跨年仍算昨天，不退化成完整日期档", () => {
    expect(
      formatMessageTime(at(2025, 11, 31, 10, 0), at(2026, 0, 1, 10, 0))?.label,
    ).toBe("昨天 10:00");
  });
});

describe("formatMessageTime 非法输入", () => {
  it.each([
    ["undefined", undefined],
    ["null", null],
    ["NaN", Number.NaN],
    ["0", 0],
    ["负数", -1],
    ["Infinity", Number.POSITIVE_INFINITY],
    ["-Infinity", Number.NEGATIVE_INFINITY],
  ])("%s 返回 null", (_name, ms) => {
    expect(formatMessageTime(ms as number | null | undefined, NOW)).toBeNull();
  });
});

describe("formatDuration 分档", () => {
  it("不足 1 秒统一收敛为「不足 1 秒」", () => {
    expect(formatDuration(0)).toBe("不足 1 秒");
    expect(formatDuration(999)).toBe("不足 1 秒");
  });

  it("1 分钟内保留 1 位小数", () => {
    expect(formatDuration(1000)).toBe("1.0 秒");
    expect(formatDuration(12_340)).toBe("12.3 秒");
    expect(formatDuration(59_940)).toBe("59.9 秒");
  });

  it("超过 1 分钟按分/秒分档，整分不带秒", () => {
    expect(formatDuration(60_000)).toBe("1 分");
    expect(formatDuration(83_000)).toBe("1 分 23 秒");
    expect(formatDuration(3_599_000)).toBe("59 分 59 秒");
  });

  it("超过 1 小时按时/分分档", () => {
    expect(formatDuration(3_600_000)).toBe("1 小时");
    expect(formatDuration(3_840_000)).toBe("1 小时 4 分");
  });
});

describe("formatDuration 非法输入", () => {
  it.each([
    ["undefined", undefined],
    ["null", null],
    ["NaN", Number.NaN],
    ["负数", -1],
    ["Infinity", Number.POSITIVE_INFINITY],
  ])("%s 返回 null", (_name, ms) => {
    expect(formatDuration(ms as number | null | undefined)).toBeNull();
  });
});

describe("formatMessageTime iso", () => {
  it("iso 与入参同一时刻（可反解回 ms）", () => {
    // 带秒：证明 iso 保留完整精度，而不只是分档用的两位时分
    const ms = new Date(2025, 9, 21, 14, 32, 15).getTime();
    const parsed = formatMessageTime(ms, NOW);
    expect(parsed).not.toBeNull();
    expect(new Date(parsed!.iso).getTime()).toBe(ms);
    // 正向保护：iso 与 label 是对同一时刻的两种表述
    expect(parsed!.label).toBe("2025-10-21 14:32");
  });
});
