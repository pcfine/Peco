import { describe, expect, it } from "vitest";
import {
  agentDescriptionDetail,
  agentDisplayName,
  agentRoleName,
  firstGrapheme,
} from "@/lib/agentName";

describe("agentDisplayName", () => {
  it("取前 12 个字里最早出现的分隔符前的文字", () => {
    expect(
      agentDisplayName("记忆管理 Agent — 在私人知识库中检索", "@memory"),
    ).toBe("记忆管理 Agent");
    expect(
      agentDisplayName("Peco 架构师 — 把模糊需求变成设计", "architect"),
    ).toBe("Peco 架构师");
    expect(
      agentDisplayName("调研 - Peco 代码考古与事实核查", "researcher"),
    ).toBe("调研");
    expect(agentDisplayName("写手 – 写文案 - 兼校对", "writer")).toBe("写手");
  });

  it("前 12 个字里没有分隔符就回退 id", () => {
    expect(
      agentDisplayName("Peco 研发团队总管 — 把需求拆成工程任务", "@master"),
    ).toBe("@master");
    expect(
      agentDisplayName("Peco Rust 后端开发 — 按冻结设计实现", "backend-dev"),
    ).toBe("backend-dev");
    expect(agentDisplayName("Peco 数据与迁移专员 — 负责 schema", "dba")).toBe(
      "dba",
    );
  });

  it("分隔符正好落在第 12 个字时仍算命中", () => {
    expect(agentDisplayName("Peco 对抗式评审 — 对照验收标准", "reviewer")).toBe(
      "Peco 对抗式评审",
    );
    expect(
      agentDisplayName("Peco 端到端验证 — 用隔离 PECO_HOME", "verifier"),
    ).toBe("Peco 端到端验证");
    expect(agentDisplayName("Peco 前端开发 — 按冻结设计实现", "web-dev")).toBe(
      "Peco 前端开发",
    );
  });

  it("超过 12 个字才出现的分隔符不参与标题", () => {
    expect(
      agentDisplayName(
        "Peco 数据与迁移专员 — 负责 SQLite schema 与迁移",
        "dba",
      ),
    ).toBe("dba");
    expect(agentDisplayName("一二三四五六七八九十十一 — 说明", "x")).toBe("x");
  });

  it("标题按码点开窗，不会切坏 emoji", () => {
    // 码点窗口比 UTF-16 窗口多装一个字：切在码元上就找不到这个连字符。
    expect(agentDisplayName("🚀一二三四五六七八九 - 说明", "x")).toBe(
      "🚀一二三四五六七八九",
    );
  });

  it("空描述、分隔符在开头都回退 id", () => {
    expect(agentDisplayName("", "reviewer")).toBe("reviewer");
    expect(agentDisplayName("   ", "reviewer")).toBe("reviewer");
    expect(agentDisplayName("— 没有名称的说明", "reviewer")).toBe("reviewer");
    expect(agentDisplayName("- —–", "reviewer")).toBe("reviewer");
  });
});

describe("agentRoleName", () => {
  it("取整条描述里分隔符前的文字，不受 12 字窗口限制", () => {
    expect(agentRoleName("Peco 数据与迁移专员 — 负责 SQLite schema")).toBe(
      "Peco 数据与迁移专员",
    );
    expect(agentRoleName(" 调研 - Peco 代码考古与事实核查 — 读源码 ")).toBe(
      "调研",
    );
  });

  it("没有分隔符或分隔符在开头时为空", () => {
    expect(agentRoleName("干活的")).toBe("");
    expect(agentRoleName("")).toBe("");
    expect(agentRoleName("   ")).toBe("");
    expect(agentRoleName("— 没有名称的说明")).toBe("");
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
