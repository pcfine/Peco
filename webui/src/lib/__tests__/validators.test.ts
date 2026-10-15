import { describe, expect, it } from "vitest";
import { knowledgeBaseSchema } from "../validators";

describe("knowledgeBaseSchema 名称校验", () => {
  it.each(["my-kb", "kb_01", "a.b-c", "A1"])("接受合法名称 %s", (name) => {
    expect(knowledgeBaseSchema.safeParse({ name }).success).toBe(true);
  });

  it.each([
    "",
    "技术文档",
    "@x",
    "a b",
    ".hidden",
    "-x",
    "x@y",
    "a.",
    "a-",
    "a_",
    "a._-",
  ])("拒绝非法名称 %s", (name) => {
    expect(knowledgeBaseSchema.safeParse({ name }).success).toBe(false);
  });

  it("名称超过 64 个字符时给出长度提示", () => {
    const result = knowledgeBaseSchema.safeParse({ name: "a".repeat(65) });

    expect(result.success).toBe(false);
    if (!result.success) {
      expect(result.error.issues[0]?.message).toContain("64");
    }
  });
});
