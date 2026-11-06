import { describe, expect, it } from "vitest";
import {
  MAX_RETRIES,
  retryDelay,
  sseDotClass,
  sseLabel,
  type SseState,
} from "@/lib/sseState";

describe("SSE 可见状态机", () => {
  it("退避序列是 1s→2s→4s→8s→15s→15s，超出上限仍取最后一档", () => {
    expect([1, 2, 3, 4, 5, 6].map(retryDelay)).toEqual([
      1000, 2000, 4000, 8000, 15000, 15000,
    ]);
    expect(retryDelay(0)).toBe(1000);
    expect(retryDelay(99)).toBe(15000);
    expect(MAX_RETRIES).toBe(6);
  });

  it("重连文案带上「第 n / 6 次」——用户要知道还能等多久", () => {
    const state: SseState = { kind: "reconnecting", attempt: 1 };
    expect(sseLabel(state)).toBe("连接中断，第 1 次重试（1/6）");
    expect(sseDotClass(state)).toBe("bg-warn");
  });

  it("五个状态各有文案与点色，没有一条是空白", () => {
    const states: SseState[] = [
      { kind: "idle" },
      { kind: "open" },
      { kind: "running" },
      { kind: "reconnecting", attempt: 3 },
      { kind: "failed" },
    ];
    for (const state of states) {
      expect(sseLabel(state).length).toBeGreaterThan(0);
      expect(sseDotClass(state)).toMatch(/^bg-/);
    }
  });
});
