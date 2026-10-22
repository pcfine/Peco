import { describe, expect, it } from "vitest";
import { formatCreatedAt, parseServerTimestamp } from "../format";

describe("parseServerTimestamp", () => {
  // 变异点：去掉补上的 "Z" → 字符串按本地时区解析，在 TZ=Asia/Shanghai 下
  // 得到 04:00Z（toISOString 差 8 小时）→ 断言失败。
  it("把 DB 的 UTC 无标记时间串按 UTC 解析", () => {
    expect(parseServerTimestamp("2026-10-20 12:00:00").toISOString()).toBe(
      "2026-10-20T12:00:00.000Z",
    );
  });

  it("跨日边界同样按 UTC 解析", () => {
    expect(parseServerTimestamp("2026-01-01 23:59:59").toISOString()).toBe(
      "2026-01-01T23:59:59.000Z",
    );
  });
});

describe("formatCreatedAt", () => {
  it("有效时间串渲染出含年份的本地时间", () => {
    const text = formatCreatedAt("2026-10-20 12:00:00");
    expect(text).not.toBe("");
    expect(text).toContain("2026");
  });

  // 变异点：去掉 NaN 判断 → 渲染出 "NaN-NaN-NaN NaN:NaN"。
  it("无效时间串返回空串", () => {
    expect(formatCreatedAt("")).toBe("");
    expect(formatCreatedAt("不是时间")).toBe("");
  });
});
