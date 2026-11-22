import { describe, expect, it } from "vitest";
import { isPecoSurface } from "@/lib/pecoSurface";

describe("Peco 会话本体所在的页面", () => {
  it("Peco 页与 Peco 空间详情页都算，尾斜杠不影响判定", () => {
    expect(isPecoSurface("/peco", "ws_peco")).toBe(true);
    expect(isPecoSurface("/peco/", "ws_peco")).toBe(true);
    expect(isPecoSurface("/spaces/ws_peco", "ws_peco")).toBe(true);
    expect(isPecoSurface("/spaces/ws_peco/", "ws_peco")).toBe(true);
  });

  it("别的空间页与工作室列表都不是 Peco 会话", () => {
    expect(isPecoSurface("/spaces/ws_a", "ws_peco")).toBe(false);
    expect(isPecoSurface("/spaces", "ws_peco")).toBe(false);
    expect(isPecoSurface("/spaces/ws_peco/foo", "ws_peco")).toBe(false);
  });

  it("Peco 空间 id 未到手时只认 /peco", () => {
    expect(isPecoSurface("/peco", null)).toBe(true);
    expect(isPecoSurface("/spaces/ws_peco", undefined)).toBe(false);
  });
});
