import { describe, expect, it } from "vitest";
import { parseServerTimestamp } from "@/lib/format";
import { relativeUntil } from "@/lib/relative";

// 基准时刻也用同一个解析器取：服务端串是 UTC 无时区文本，断言的档位不能随
// 跑测机器的时区变。
const NOW = parseServerTimestamp("2026-11-05 00:00:00").getTime();

describe("未来时刻的人读", () => {
  it("按距离分档：分钟后 / 小时后", () => {
    expect(relativeUntil("2026-11-05 00:05:00", NOW)).toBe("5 分钟后");
    expect(relativeUntil("2026-11-05 02:00:00", NOW)).toBe("2 小时后");
  });

  it("已经到点或过点都说「即将运行」，不显示负数", () => {
    expect(relativeUntil("2026-11-05 00:00:00", NOW)).toBe("即将运行");
    expect(relativeUntil("2026-11-04 00:00:00", NOW)).toBe("即将运行");
  });

  it("一周以后退回到本地日历日", () => {
    const raw = "2026-11-20 00:00:00";
    const date = parseServerTimestamp(raw);
    expect(relativeUntil(raw, NOW)).toBe(
      `${date.getMonth() + 1}月${date.getDate()}日`,
    );
  });

  it("没有时刻给破折号", () => {
    expect(relativeUntil(null, NOW)).toBe("—");
  });
});
