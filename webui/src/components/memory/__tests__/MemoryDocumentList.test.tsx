// MemoryDocumentList 组件测试 —— 翻页 / 空态 / 错误重试 / 列渲染（design §5.3 F10）

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryDocumentList } from "../MemoryDocumentList";
import { listMemoryDocuments } from "@/api/memory";
import type { MemoryDocumentPage } from "@/types/memory";

vi.mock("@/api/memory", () => ({
  getMemoryGraph: vi.fn(),
  listMemoryDocuments: vi.fn(),
}));

const DOCS = vi.mocked(listMemoryDocuments);

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

async function renderList() {
  await act(async () => {
    root.render(<MemoryDocumentList />);
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

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;

  DOCS.mockReset();

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
});
