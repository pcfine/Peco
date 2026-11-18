import { describe, expect, it } from "vitest";
import { hasFrontmatter, splitFrontmatter } from "@/lib/frontmatter";

describe("splitFrontmatter", () => {
  it("切出首尾 --- 行，两段长度之和等于原文", () => {
    const content = [
      "---",
      "name: delivery-contract",
      'allowed-tools: ["shell"]',
      "---",
      "",
      "# 交付契约",
      "",
      "正文",
      "",
    ].join("\n");

    const { frontmatter, body } = splitFrontmatter(content);

    expect(frontmatter).toBe(
      [
        "---",
        "name: delivery-contract",
        'allowed-tools: ["shell"]',
        "---",
      ].join("\n"),
    );
    expect(body).toBe("\n\n# 交付契约\n\n正文\n");
    expect(frontmatter!.length + body.length).toBe(content.length);
    expect(frontmatter! + body).toBe(content);
    expect(hasFrontmatter(content)).toBe(true);
  });

  it("没有 frontmatter 时整体算 body", () => {
    const content = "# 标题\n\n正文\n";

    expect(splitFrontmatter(content)).toEqual({
      frontmatter: null,
      body: content,
    });
    expect(hasFrontmatter(content)).toBe(false);
  });

  it("开头有 --- 但没有闭合行时整体算 body", () => {
    const content = "---\nname: x\n\n# 标题\n";

    expect(splitFrontmatter(content)).toEqual({
      frontmatter: null,
      body: content,
    });
    expect(splitFrontmatter("---\n")).toEqual({
      frontmatter: null,
      body: "---\n",
    });
    expect(splitFrontmatter("---")).toEqual({
      frontmatter: null,
      body: "---",
    });
  });

  it("正文中间（非首行）的 --- 不当作 frontmatter", () => {
    const content = "# 标题\n\n---\n\n后续\n";
    expect(splitFrontmatter(content).frontmatter).toBeNull();

    // 首行不是 --- ，即便后面成对出现也不处理
    const paired = "正文\n---\nname: x\n---\n";
    expect(splitFrontmatter(paired)).toEqual({
      frontmatter: null,
      body: paired,
    });
  });

  it("CRLF 版本同样能切分", () => {
    const content = "---\r\nname: x\r\n---\r\n\r\n# 标题\r\n";

    const { frontmatter, body } = splitFrontmatter(content);

    expect(frontmatter).toBe("---\r\nname: x\r\n---");
    expect(body).toBe("\r\n\r\n# 标题\r\n");
    expect(frontmatter!.length + body.length).toBe(content.length);
  });

  it("空 frontmatter 切出首尾两行", () => {
    const content = "---\n---\n正文";

    const { frontmatter, body } = splitFrontmatter(content);

    expect(frontmatter).toBe("---\n---");
    expect(body).toBe("\n正文");
    expect(frontmatter!.length + body.length).toBe(content.length);
  });

  it("UTF-8 BOM 计入 frontmatter，长度仍然守恒", () => {
    const content = "\uFEFF---\r\nname: x\r\n---\r\n正文";

    const { frontmatter, body } = splitFrontmatter(content);

    expect(frontmatter).toBe("\uFEFF---\r\nname: x\r\n---");
    expect(body).toBe("\r\n正文");
    expect(frontmatter!.length + body.length).toBe(content.length);
  });

  it("闭合 --- 后面跟的不是换行（如 ----）不算闭合", () => {
    const content = "---\nname: x\n----\nbody\n";
    expect(splitFrontmatter(content).frontmatter).toBeNull();
  });
});
