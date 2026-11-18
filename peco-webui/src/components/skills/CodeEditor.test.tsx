// CodeEditor 的高亮层是叠在 textarea 下的透明底片：类名要对，去标签后的文本
// 更要与输入逐字节一致，差一个字符整篇就会错位。

import { createRef } from "react";
import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { CodeEditor } from "@/components/skills/CodeEditor";

const WITH_FRONTMATTER = [
  "---",
  "name: db-migration",
  'allowed-tools: ["shell"]',
  "---",
  "",
  "# SQLite 迁移铁律",
  "",
  "先备份再迁移。",
  "",
].join("\n");

function renderHighlight(value: string, language: string): HTMLElement {
  const textareaRef = createRef<HTMLTextAreaElement>();
  const { container } = render(
    <CodeEditor
      value={value}
      onChange={() => {}}
      language={language}
      ariaLabel="内容"
      textareaRef={textareaRef}
    />,
  );
  const code = container.querySelector<HTMLElement>("pre code");
  if (!code) throw new Error("highlight layer not found");
  return code;
}

describe("CodeEditor 高亮层", () => {
  it("markdown 的 frontmatter 按 YAML 高亮，正文仍按 Markdown 高亮", () => {
    const code = renderHighlight(WITH_FRONTMATTER, "markdown");

    const meta = Array.from(code.querySelectorAll(".hljs-meta"));
    expect(meta.map((el) => el.textContent)).toEqual(["---", "---"]);
    expect(
      Array.from(code.querySelectorAll(".hljs-attr")).map(
        (el) => el.textContent,
      ),
    ).toEqual(["name:", "allowed-tools:"]);

    const section = Array.from(code.querySelectorAll(".hljs-section"));
    expect(section.map((el) => el.textContent)).toEqual(["# SQLite 迁移铁律"]);
    // 属性行不得再被当成正文吞进 section 标题
    expect(
      section.some((el) => el.textContent?.includes("allowed-tools")),
    ).toBe(false);
  });

  it("分段高亮拼接后去标签文本与原文逐字节相同", () => {
    const code = renderHighlight(WITH_FRONTMATTER, "markdown");
    // 末尾的换行是组件补齐行盒用的，不算进输入文本
    expect(code.textContent).toBe(`${WITH_FRONTMATTER}\n`);
  });

  it("CRLF 与含 HTML 特殊字符的 frontmatter 也不丢字符", () => {
    const value =
      "---\r\ndescription: 交付契约 — <可机检> & 不省略\r\n---\r\n\r\n# 标题\r\n";
    const code = renderHighlight(value, "markdown");

    // 带 frontmatter 的 CRLF 在高亮前并成 LF：换行数不变，行才对得齐。
    expect(code.textContent).toBe(`${value.replace(/\r\n/g, "\n")}\n`);
    expect(code.querySelectorAll(".hljs-meta").length).toBe(2);
    expect(code.querySelector(".hljs-attr")?.textContent).toBe("description:");
  });

  it("没有 frontmatter 的 markdown 不冒 meta / attr 类", () => {
    const code = renderHighlight("# 标题\n\n正文\n", "markdown");

    expect(code.querySelector(".hljs-meta")).toBeNull();
    expect(code.querySelector(".hljs-attr")).toBeNull();
    expect(
      Array.from(code.querySelectorAll(".hljs-section")).map(
        (el) => el.textContent,
      ),
    ).toEqual(["# 标题"]);
    expect(code.textContent).toBe("# 标题\n\n正文\n\n");
  });

  it("非 markdown 语言即使首行是 --- 也不按 frontmatter 切分", () => {
    const code = renderHighlight(WITH_FRONTMATTER, "sql");

    expect(code.querySelector(".hljs-attr")).toBeNull();
    expect(code.querySelector(".hljs-meta")).toBeNull();
    expect(code.textContent).toBe(`${WITH_FRONTMATTER}\n`);
  });

  it("超过 120k 的 markdown 不做高亮，原样转义", () => {
    const value = `---\nname: x\n---\n\n${"a".repeat(120_001)}`;
    const code = renderHighlight(value, "markdown");

    expect(code.innerHTML).not.toContain("hljs-");
    expect(code.textContent).toBe(`${value}\n`);
  });
});
