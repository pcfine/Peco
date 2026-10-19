// MarkdownRenderer 测试 —— className 合并 / 表格横向滚动容器

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MarkdownRenderer } from "../MarkdownRenderer";

let container: HTMLDivElement;
let root: Root;

async function render(content: string, className?: string) {
  await act(async () => {
    root.render(<MarkdownRenderer content={content} className={className} />);
  });
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("MarkdownRenderer", () => {
  it("表格包进横向滚动容器，避免撑宽宿主容器", async () => {
    await render("| a | b |\n| --- | --- |\n| 1 | 2 |");
    const table = container.querySelector("table");
    expect(table).not.toBeNull();
    expect(table?.parentElement?.className).toContain("overflow-x-auto");
  });

  it("不传 className 时保持既有根类名", async () => {
    await render("正文");
    const rootClass = container.firstElementChild?.className ?? "";
    expect(rootClass).toContain("break-words");
    expect(rootClass).toContain("[&_table]:border-collapse");
  });

  it("传入 className 与既有类合并而非覆盖", async () => {
    await render("正文", "max-w-[560px] text-sm");
    const rootClass = container.firstElementChild?.className ?? "";
    expect(rootClass).toContain("max-w-[560px]");
    expect(rootClass).toContain("break-words");
  });
});
