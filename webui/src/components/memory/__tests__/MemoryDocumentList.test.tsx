// MemoryDocumentList 组件测试 —— 翻页 / 空态 / 错误重试 / 列渲染（design §5.3 F10）

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, createRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  MemoryDocumentList,
  type MemoryDocumentListHandle,
} from "../MemoryDocumentList";
import {
  getMemoryDocument,
  listMemoryDocuments,
  searchMemoryDocuments,
} from "@/api/memory";
import type { MemoryDocumentPage, MemorySearchHit } from "@/types/memory";

vi.mock("@/api/memory", () => ({
  getMemoryGraph: vi.fn(),
  listMemoryDocuments: vi.fn(),
  searchMemoryDocuments: vi.fn(),
  getMemoryDocument: vi.fn(),
}));

const DOCS = vi.mocked(listMemoryDocuments);
const SEARCH = vi.mocked(searchMemoryDocuments);
const DETAIL = vi.mocked(getMemoryDocument);

// jsdom 未实现 ResizeObserver（radix ScrollArea）；Select 亦需指针捕获 API。
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

const PAGE_TWO_ROWS: MemoryDocumentPage = {
  documents: [
    {
      id: "c6cd2285ca896c7d",
      title: "memory_1789552607485_0",
      source: "ppa_episodic",
      file_type: "txt",
    },
    {
      id: "aabbccddeeff0011",
      title: "memory_1789552607485_1",
      source: "ppa_profile",
      file_type: null,
    },
  ],
  offset: 0,
  limit: 2,
  has_more: true,
};

let container: HTMLDivElement;
let root: Root;

async function renderList(ref?: React.Ref<MemoryDocumentListHandle>) {
  await act(async () => {
    root.render(<MemoryDocumentList ref={ref} />);
  });
  await flush();
}

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function buttonByText(text: string): HTMLButtonElement {
  const btn = Array.from(container.querySelectorAll("button")).find((b) =>
    b.textContent?.includes(text),
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn as HTMLButtonElement;
}

function rows(): NodeListOf<HTMLTableRowElement> {
  return container.querySelectorAll("tbody tr");
}

function inputEl(): HTMLInputElement {
  const el = container.querySelector("input");
  if (!el) throw new Error("search input not found");
  return el as HTMLInputElement;
}

/** 绕过 React 的 value 追踪，触发一次真实的 onChange。 */
function typeInto(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value",
  )?.set;
  setter?.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

/** 打开来源 Select（portal 挂在 document.body）并点选给定选项。 */
async function chooseSource(label: string) {
  const trigger = container.querySelector(
    '[data-slot="select-trigger"]',
  ) as HTMLElement;
  await act(async () => {
    trigger.dispatchEvent(
      new MouseEvent("pointerdown", {
        bubbles: true,
        button: 0,
        ctrlKey: false,
      }),
    );
    trigger.click();
  });
  await flush();

  const opt = Array.from(
    document.body.querySelectorAll('[role="option"]'),
  ).find((o) => o.textContent?.trim() === label) as HTMLElement | undefined;
  if (!opt) throw new Error(`option not found: ${label}`);

  await act(async () => {
    opt.dispatchEvent(
      new MouseEvent("pointermove", { bubbles: true, button: 0 }),
    );
    opt.dispatchEvent(
      new MouseEvent("pointerdown", { bubbles: true, button: 0 }),
    );
    opt.dispatchEvent(
      new MouseEvent("pointerup", { bubbles: true, button: 0 }),
    );
    opt.click();
  });
  await flush();
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;
  (
    HTMLElement.prototype as unknown as { hasPointerCapture: () => boolean }
  ).hasPointerCapture = () => false;
  (
    HTMLElement.prototype as unknown as { setPointerCapture: () => void }
  ).setPointerCapture = () => {};
  (
    HTMLElement.prototype as unknown as { releasePointerCapture: () => void }
  ).releasePointerCapture = () => {};
  (
    HTMLElement.prototype as unknown as { scrollIntoView: () => void }
  ).scrollIntoView = () => {};

  DOCS.mockReset();
  SEARCH.mockReset();
  DETAIL.mockReset();
  DETAIL.mockResolvedValue({
    id: "x",
    title: "t",
    source: "s",
    file_type: null,
    created_at: null,
    content: "",
  });

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("MemoryDocumentList", () => {
  it("有下一页时点击以 offset=limit 再次拉取", async () => {
    DOCS.mockResolvedValueOnce(PAGE_TWO_ROWS).mockResolvedValueOnce({
      documents: [],
      offset: 2,
      limit: 2,
      has_more: false,
    });

    await renderList();

    expect(DOCS).toHaveBeenCalledTimes(1);
    expect(rows()).toHaveLength(2);
    expect(rows()[0].textContent).toContain("memory_1789552607485_0");

    const next = buttonByText("下一页");
    expect(next).not.toBeDisabled();

    await act(async () => {
      next.click();
    });
    await flush();

    expect(DOCS).toHaveBeenCalledTimes(2);
    expect(DOCS.mock.calls[1][0]).toBe(2);
  });

  it("has_more 为 false 时「下一页」禁用", async () => {
    DOCS.mockResolvedValue({ ...PAGE_TWO_ROWS, has_more: false });

    await renderList();

    expect(buttonByText("下一页")).toBeDisabled();
    expect(buttonByText("上一页")).toBeDisabled();
  });

  it("首屏空为「还没有记忆文档」，翻页后空为「本页无数据」", async () => {
    DOCS.mockResolvedValueOnce({
      documents: [],
      offset: 0,
      limit: 2,
      has_more: false,
    });
    await renderList();
    expect(container.textContent).toContain("还没有记忆文档");

    // 第二例：offset>0 且空
    act(() => root.unmount());
    DOCS.mockReset();
    DOCS.mockResolvedValueOnce(PAGE_TWO_ROWS).mockResolvedValueOnce({
      documents: [],
      offset: 2,
      limit: 2,
      has_more: false,
    });
    root = createRoot(container);
    await renderList();

    await act(async () => {
      buttonByText("下一页").click();
    });
    await flush();

    expect(container.textContent).toContain("本页无数据");
  });

  it("API 失败时显示错误与重试，点击后重新拉取", async () => {
    DOCS.mockRejectedValue(new Error("boom"));

    await renderList();

    expect(container.textContent).toContain("boom");
    expect(buttonByText("重试")).toBeTruthy();
    expect(DOCS).toHaveBeenCalledTimes(1);

    await act(async () => {
      buttonByText("重试").click();
    });
    await flush();

    expect(DOCS).toHaveBeenCalledTimes(2);
  });

  it("source 渲染为 Badge，file_type 为 null 时渲染 -", async () => {
    DOCS.mockResolvedValue(PAGE_TWO_ROWS);

    await renderList();

    const badges = container.querySelectorAll('[data-slot="badge"]');
    expect(badges).toHaveLength(2);
    expect(badges[0].textContent).toBe("ppa_episodic");
    expect(badges[1].textContent).toBe("ppa_profile");
    // 第二行 file_type 为 null ⇒ "-"
    expect(rows()[1].textContent).toContain("-");
  });

  // ── v2：来源筛选 / 检索 / 详情抽屉 ──────────────────────────────────────

  it("来源筛选把 offset 归零并以 source 重新拉取", async () => {
    DOCS.mockResolvedValue(PAGE_TWO_ROWS);

    await renderList();

    // 先翻到第二页（step = 本页 limit = 2）
    await act(async () => {
      buttonByText("下一页").click();
    });
    await flush();
    expect(DOCS.mock.calls[DOCS.mock.calls.length - 1][0]).toBe(2);

    await chooseSource("ppa_profile");

    expect(DOCS).toHaveBeenLastCalledWith(0, 20, "ppa_profile");
  });

  const HIT: MemorySearchHit = {
    id: "hit-1",
    title: "命中标题",
    source: "ppa_semantic",
    file_type: "txt",
    snippet: "…书名叫《春天的爱情故事》…",
  };

  it("输入检索词 + Enter → 走检索接口、渲染片段、隐藏分页、下拉可用", async () => {
    DOCS.mockResolvedValue(PAGE_TWO_ROWS);
    SEARCH.mockResolvedValue({ hits: [HIT] });

    await renderList();
    expect(container.textContent).toContain("下一页");

    await act(async () => {
      typeInto(inputEl(), "小C");
      inputEl().dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
      );
    });
    await flush();

    expect(SEARCH).toHaveBeenCalledWith("小C", 20);
    expect(container.textContent).toContain("命中 1 条");
    expect(container.textContent).toContain("春天的爱情故事");
    expect(container.textContent).not.toContain("下一页");

    const trigger = container.querySelector(
      '[data-slot="select-trigger"]',
    ) as HTMLButtonElement;
    expect(trigger).not.toBeDisabled();
  });

  it("选中来源后检索把 source 一并透传", async () => {
    DOCS.mockResolvedValue(PAGE_TWO_ROWS);
    SEARCH.mockResolvedValue({ hits: [HIT] });

    await renderList();
    await chooseSource("ppa_semantic");

    await act(async () => {
      typeInto(inputEl(), "小C");
      buttonByText("搜索").click();
    });
    await flush();

    expect(SEARCH).toHaveBeenCalledWith("小C", 20, "ppa_semantic");
  });

  it("检索态下 refresh 重跑检索且不回列表", async () => {
    const ref = createRef<MemoryDocumentListHandle>();
    DOCS.mockResolvedValue(PAGE_TWO_ROWS);
    SEARCH.mockResolvedValue({ hits: [HIT] });

    await renderList(ref);

    await act(async () => {
      typeInto(inputEl(), "小C");
      buttonByText("搜索").click();
    });
    await flush();

    expect(SEARCH).toHaveBeenCalledTimes(1);
    const docsCallsBefore = DOCS.mock.calls.length;

    await act(async () => {
      ref.current?.refresh();
    });
    await flush();

    expect(SEARCH).toHaveBeenCalledTimes(2);
    expect(SEARCH).toHaveBeenLastCalledWith("小C", 20);
    expect(DOCS.mock.calls.length).toBe(docsCallsBefore);
  });

  it("检索无命中显示空态，点清除筛选回到列表态", async () => {
    DOCS.mockResolvedValue(PAGE_TWO_ROWS);
    SEARCH.mockResolvedValue({ hits: [] });

    await renderList();
    const docsCallsBefore = DOCS.mock.calls.length;

    await act(async () => {
      typeInto(inputEl(), "不存在");
      buttonByText("搜索").click();
    });
    await flush();

    expect(container.textContent).toContain("没有匹配的记忆文档");
    const clear = Array.from(container.querySelectorAll("button")).find(
      (b) => b.textContent?.trim() === "清除筛选",
    );
    expect(clear).toBeTruthy();

    await act(async () => {
      clear!.click();
    });
    await flush();

    // 回到列表态 ⇒ 重新拉列表
    expect(DOCS.mock.calls.length).toBeGreaterThan(docsCallsBefore);
    expect(container.textContent).toContain("下一页");
  });

  it("点击行 / 行上按 Enter 打开详情抽屉（调用详情接口）", async () => {
    DOCS.mockResolvedValue(PAGE_TWO_ROWS);
    await renderList();

    await act(async () => {
      rows()[0].click();
    });
    await flush();

    expect(DETAIL).toHaveBeenCalledWith("c6cd2285ca896c7d");

    // 关闭后，第二行按 Enter 同样开抽屉
    await act(async () => {
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      );
    });
    await flush();

    await act(async () => {
      rows()[1].dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
      );
    });
    await flush();

    expect(DETAIL).toHaveBeenCalledWith("aabbccddeeff0011");
  });
});
