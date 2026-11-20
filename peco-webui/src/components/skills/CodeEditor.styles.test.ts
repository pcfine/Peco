// jsdom 不跑真实级联，只能直读样式源码，断言那条未分层覆盖仍在。
/// <reference types="node" />

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const SELECTOR = "[data-code-editor] pre code.hljs";

// vitest 把 CSS 导入替换成空模块，只能按路径直读。
const css = readFileSync(resolve(process.cwd(), "src/index.css"), "utf-8")
  .replace(/\/\*[\s\S]*?\*\//g, "")
  .replace(/\s+/g, " ");

/** 去掉整块 `@layer`，只留未分层样式。 */
function unlayered(source: string): string {
  let kept = "";
  let cursor = 0;
  for (;;) {
    const start = source.indexOf("@layer", cursor);
    if (start === -1) return kept + source.slice(cursor);
    kept += source.slice(cursor, start);
    const open = source.indexOf("{", start);
    if (open === -1) return kept;
    let depth = 0;
    let end = open;
    for (; end < source.length; end += 1) {
      if (source[end] === "{") depth += 1;
      else if (source[end] === "}") {
        depth -= 1;
        if (depth === 0) break;
      }
    }
    cursor = end + 1;
  }
}

/** 选择器完全匹配的规则的声明块，没有则返回 null。 */
function declarationsFor(source: string, selector: string): string | null {
  for (const [, rawSelector, declarations] of source.matchAll(
    /([^{}]+)\{([^{}]*)\}/g,
  )) {
    const selectors = rawSelector.split(",").map((part) => part.trim());
    if (selectors.includes(selector)) return declarations.trim();
  }
  return null;
}

describe("CodeEditor 高亮层的样式覆盖", () => {
  it("未分层地清掉 hljs 的内边距、嵌套滚动容器与背景色", () => {
    const declarations = declarationsFor(unlayered(css), SELECTOR);

    expect(declarations).not.toBeNull();
    expect(declarations).toContain("padding: 0");
    expect(declarations).toContain("overflow: visible");
    expect(declarations).toContain("background: transparent");
  });
});
