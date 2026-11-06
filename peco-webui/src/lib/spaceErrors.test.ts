import { describe, expect, it } from "vitest";
import { ApiError } from "@/lib/api";
import { spaceErrorText } from "@/lib/spaceErrors";

describe("新建向导的错误文案", () => {
  it("每个已知 code 都翻成可操作的一句话，而不是 code 本身", () => {
    const text = spaceErrorText(
      new ApiError("work_dir '/x' is not an absolute path", {
        status: 400,
        code: "work_dir_not_absolute",
      }),
    );
    expect(text).toContain("绝对路径");
    expect(text).not.toContain("work_dir_not_absolute");
  });

  it("team_missing 指向「换一个团队或先建团队」", () => {
    const text = spaceErrorText(
      new ApiError("team 'nope' has no directory", {
        status: 409,
        code: "team_missing",
      }),
    );
    expect(text).toContain("团队");
  });

  it("未知 code 与无 code 都退回服务端 message", () => {
    expect(
      spaceErrorText(new ApiError("boom", { status: 500, code: "nope" })),
    ).toBe("boom");
    expect(spaceErrorText(new Error("连不上"))).toBe("连不上");
  });
});
