// CodeEditor 的高亮层是叠在 textarea 下的透明底片：类名要对，去标签后的文本
// 更要与输入逐字节一致，差一个字符整篇就会错位。

import { createRef } from "react";
import { describe, expect, it, vi } from "vitest";
import { createEvent, fireEvent, render } from "@testing-library/react";
import { CodeEditor, scrollbarInsets } from "@/components/skills/CodeEditor";

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

describe("scrollbarInsets", () => {
  it("按 border box 与 client box 的差算出滑条占位", () => {
    expect(
      scrollbarInsets({
        offsetWidth: 570,
        clientWidth: 560,
        offsetHeight: 820,
        clientHeight: 810,
      }),
    ).toEqual({ w: 10, h: 10 });
  });

  it("没有滑条时不收边", () => {
    expect(
      scrollbarInsets({
        offsetWidth: 570,
        clientWidth: 570,
        offsetHeight: 820,
        clientHeight: 820,
      }),
    ).toEqual({ w: 0, h: 0 });
  });

  it("布局未就绪读到负值也不返回负的裁剪量", () => {
    expect(
      scrollbarInsets({
        offsetWidth: 570,
        clientWidth: 580,
        offsetHeight: 820,
        clientHeight: 840,
      }),
    ).toEqual({ w: 0, h: 0 });
  });
});

describe("CodeEditor 裁剪层", () => {
  it("高亮层被裁到 textarea 的 client box 内，位移仍只发生在 pre 上", () => {
    const textareaRef = createRef<HTMLTextAreaElement>();
    const { container } = render(
      <CodeEditor
        value={"# 很长的一行 ".repeat(40)}
        onChange={() => {}}
        language="markdown"
        ariaLabel="内容"
        textareaRef={textareaRef}
      />,
    );

    const pres = container.querySelectorAll<HTMLElement>("pre");
    expect(pres).toHaveLength(1);
    const pre = pres[0];
    const clip = pre.parentElement;
    if (!clip) throw new Error("clip layer not found");

    expect(clip.className).toContain("overflow-hidden");
    expect(clip.className).toContain("pointer-events-none");
    // 裁剪量绑在右/下两边：jsdom 没有布局，量到的滑条宽度是 0
    expect(clip.style.right).toBe("0px");
    expect(clip.style.bottom).toBe("0px");
    // 内边距必须留在 pre 上：挪到裁剪层就会让高亮与 textarea 的文字错位
    expect(pre.className).toContain("px-5");
    expect(pre.className).toContain("py-4");
    expect(clip.className).not.toContain("px-5");
    // 跟随滚动的 transform 只写 pre，裁剪框不能跟着移动
    expect(pre.style.transform).toContain("translate");
    expect(clip.style.transform).toBe("");
  });
});

function renderForKeys() {
  const textareaRef = createRef<HTMLTextAreaElement>();
  const onChange = vi.fn();
  const { container } = render(
    <CodeEditor
      value="abc"
      onChange={onChange}
      language="markdown"
      ariaLabel="内容"
      textareaRef={textareaRef}
    />,
  );
  const textarea = container.querySelector("textarea");
  if (!textarea) throw new Error("textarea not found");
  return { textarea, onChange };
}

/** jsdom 里 Tab 不会真的搬动焦点，只能靠 onChange 与 defaultPrevented 判断谁接住了这个键。 */
function press(el: HTMLElement, key: string) {
  const event = createEvent.keyDown(el, { key });
  fireEvent(el, event);
  return event;
}

describe("CodeEditor 的 Tab 出口", () => {
  it("Tab 默认用来插入缩进并拦下事件", () => {
    const { textarea, onChange } = renderForKeys();

    const event = press(textarea, "Tab");

    expect(onChange).toHaveBeenCalledWith("  abc");
    expect(event.defaultPrevented).toBe(true);
  });

  it("Escape 之后的 Tab 放给浏览器，不插入也不拦截", () => {
    const { textarea, onChange } = renderForKeys();

    const escape = press(textarea, "Escape");
    expect(escape.defaultPrevented).toBe(false);
    expect(onChange).not.toHaveBeenCalled();

    const tab = press(textarea, "Tab");
    expect(onChange).not.toHaveBeenCalled();
    expect(tab.defaultPrevented).toBe(false);
  });

  it("出口只用一次：放行之后的下一个 Tab 又回到缩进", () => {
    const { textarea, onChange } = renderForKeys();
    press(textarea, "Escape");
    press(textarea, "Tab");

    const again = press(textarea, "Tab");

    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenCalledWith("  abc");
    expect(again.defaultPrevented).toBe(true);
  });

  it("blur 清掉放行状态，重新聚焦后 Tab 仍是缩进", () => {
    const { textarea, onChange } = renderForKeys();
    press(textarea, "Escape");
    fireEvent.blur(textarea);

    const tab = press(textarea, "Tab");

    expect(onChange).toHaveBeenCalledTimes(1);
    expect(tab.defaultPrevented).toBe(true);
  });
});
