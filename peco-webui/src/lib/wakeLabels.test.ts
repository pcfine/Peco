import { describe, expect, it } from "vitest";
import {
  wakeDescription,
  wakeSkipReasonLabel,
  wakeStateLabel,
} from "@/lib/wakeLabels";

describe("唤醒态文案", () => {
  it.each([
    ["awaiting", "等待完成"],
    ["pending", "待投递"],
    ["delivering", "投递中"],
    ["delivered", "已投递"],
    ["skipped", "已跳过"],
    ["failed", "投递失败"],
  ])("%s → %s", (wakeState, label) => {
    expect(wakeStateLabel(wakeState)).toBe(label);
  });

  it("delivered 只说已投递，不暗示任务处理完成", () => {
    expect(wakeStateLabel("delivered")).not.toMatch(/完成|处理|已读/);
  });

  it("awaiting 是还没终态的等待，不拼出成因", () => {
    expect(wakeDescription("awaiting", null)).toBe("等待完成");
    expect(wakeDescription("awaiting", "space_closed")).toBe("等待完成");
  });

  it("没见过的唤醒态给兜底文案并带上原值", () => {
    expect(wakeStateLabel("exploded")).toBe("未知唤醒态（exploded）");
    expect(wakeStateLabel("")).toBe("未知唤醒态（空）");
  });
});

describe("跳过原因文案", () => {
  it.each([
    ["not_peco_origin", "非 Peco 来源"],
    ["peco_identity_missing", "观察者缺少 Peco 身份"],
    ["orphaned", "运行期失联"],
    ["space_closed", "目标空间已关闭"],
    ["attempts_exhausted", "重试次数用尽"],
  ])("%s → %s", (reason, label) => {
    expect(wakeSkipReasonLabel(reason)).toBe(label);
  });

  it("历史行没有记录原因时不猜一种", () => {
    expect(wakeSkipReasonLabel(null)).toBe("原因未记录");
  });

  it("没见过的原因给兜底文案并带上原值", () => {
    expect(wakeSkipReasonLabel("who_knows")).toBe("未知原因（who_knows）");
  });
});

describe("完整唤醒描述", () => {
  it("skipped 带原因", () => {
    expect(wakeDescription("skipped", "space_closed")).toBe(
      "已跳过 · 目标空间已关闭",
    );
  });

  it("skipped 且原因缺失时写明原因未记录", () => {
    expect(wakeDescription("skipped", null)).toBe("已跳过 · 原因未记录");
  });

  it("非 skipped 不拼原因：那些状态没有成因可言", () => {
    expect(wakeDescription("failed", "space_closed")).toBe("投递失败");
    expect(wakeDescription("delivered", null)).toBe("已投递");
  });
});
