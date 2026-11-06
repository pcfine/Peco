import { describe, expect, it } from "vitest";
import {
  DEFAULT_BOARD,
  movePanel,
  normalizeBoard,
  togglePanel,
  visiblePanels,
} from "@/lib/board";

describe("normalizeBoard", () => {
  it("丢弃未知 id 并把缺失面板追加到末尾", () => {
    const config = normalizeBoard({ order: ["nope", "today"], hidden: [] });
    expect(config.order).toEqual(["today", "spaces", "teams", "results"]);
    expect(visiblePanels(config)).toHaveLength(4);
  });

  it("隐藏项按 order 过滤：未知/重复的 hidden 不会多算一格", () => {
    const config = normalizeBoard({
      order: ["results", "today", "spaces", "teams"],
      hidden: ["results", "results", "ghost"],
    });
    expect(config.hidden).toEqual(["results"]);
    expect(visiblePanels(config)).toEqual(["today", "spaces", "teams"]);
  });

  it("hidden 命中全部面板时保住第一块", () => {
    const config = normalizeBoard({
      order: ["today", "spaces", "teams", "results"],
      hidden: ["today", "spaces", "teams", "results"],
    });
    expect(visiblePanels(config)).toEqual(["today"]);
  });
});

describe("togglePanel", () => {
  it("关掉一块后它从可见列表消失", () => {
    const config = togglePanel(DEFAULT_BOARD, "results");
    expect(config.hidden).toEqual(["results"]);
    expect(visiblePanels(config)).toEqual(["today", "spaces", "teams"]);
  });

  it("只剩一块可见时拒绝再关", () => {
    let config = DEFAULT_BOARD;
    for (const id of ["results", "teams", "spaces"] as const) {
      config = togglePanel(config, id);
    }
    expect(visiblePanels(config)).toEqual(["today"]);
    expect(togglePanel(config, "today")).toBe(config);
  });
});

describe("movePanel", () => {
  it("与相邻面板换位", () => {
    const config = movePanel(DEFAULT_BOARD, "teams", -1);
    expect(config.order).toEqual(["today", "teams", "spaces", "results"]);
  });

  it("首行上移与末行下移都原样返回", () => {
    expect(movePanel(DEFAULT_BOARD, "today", -1)).toBe(DEFAULT_BOARD);
    expect(movePanel(DEFAULT_BOARD, "results", 1)).toBe(DEFAULT_BOARD);
  });
});
