import { describe, expect, it } from "vitest";
import {
  agentDescriptionDetail,
  agentDisplayName,
  firstGrapheme,
} from "@/lib/agentName";

describe("agentDisplayName", () => {
  it("取最早出现的分隔符前的文字", () => {
    expect(agentDisplayName("代码审查员 — 看代码", "reviewer")).toBe(
      "代码审查员",
    );
    expect(agentDisplayName("团队总管 — 派发任务", "@master")).toBe("团队总管");
    expect(agentDisplayName("reviewer - looks things up", "r")).toBe(
      "reviewer",
    );
    expect(agentDisplayName("写手 – 写文案 - 兼校对", "writer")).toBe("写手");
  });

  it("没有分隔符时取前 3 个字符", () => {
    expect(agentDisplayName("记忆管理 Agent", "memory")).toBe("记忆管");
    expect(agentDisplayName("ab", "fallback")).toBe("ab");
  });

  it("分隔符前超过 8 个字符时截断，不留下半截英文单词", () => {
    expect(agentDisplayName("一二三四五六七八九十 — 说明", "x")).toBe(
      "一二三四五六七八",
    );
    expect(
      agentDisplayName("记忆管理 Agent — 在私人知识库中检索", "memory"),
    ).toBe("记忆管理");
  });

  it("空描述与分隔符前为空都回退", () => {
    expect(agentDisplayName("", "reviewer")).toBe("reviewer");
    expect(agentDisplayName("   ", "reviewer")).toBe("reviewer");
    expect(agentDisplayName("— 没有名称的说明", "reviewer")).toBe("没有名");
    expect(agentDisplayName("- —–", "reviewer")).toBe("reviewer");
  });
});

describe("agentDescriptionDetail", () => {
  it("分隔符之后才是说明，无分隔符时原样返回", () => {
    expect(agentDescriptionDetail("代码审查员 — 看代码")).toBe("看代码");
    expect(agentDescriptionDetail("researcher - looks things up")).toBe(
      "looks things up",
    );
    expect(agentDescriptionDetail("干活的")).toBe("干活的");
    expect(agentDescriptionDetail("  ")).toBe("");
  });
});

describe("firstGrapheme", () => {
  it("取首个字符，组合 emoji 不被截断", () => {
    expect(firstGrapheme("代码审查员")).toBe("代");
    expect(firstGrapheme("  @master")).toBe("@");
    expect(firstGrapheme("👨‍👩‍👧 小组")).toBe("👨‍👩‍👧");
    expect(firstGrapheme("")).toBe("");
  });
});
