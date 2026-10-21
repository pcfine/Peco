// MemoryDocumentList 删除 —— 行内入口 / profile 置灰 / 确认弹层 / 错误分流 / 空页回退

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryDocumentList } from "../MemoryDocumentList";
import { deleteMemoryDocument, listMemoryDocuments } from "@/api/memory";
import type { MemoryDocumentPage } from "@/types/memory";
import { toast } from "sonner";

vi.mock("@/api/memory", () => ({
  getMemoryGraph: vi.fn(),
  listMemoryDocuments: vi.fn(),
  searchMemoryDocuments: vi.fn(),
  getMemoryDocument: vi.fn(),
  deleteMemoryDocument: vi.fn(),
}));

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn(), warning: vi.fn() },
}));

const LIST = vi.mocked(listMemoryDocuments);
const DEL = vi.mocked(deleteMemoryDocument);

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

const TWO_ROWS: MemoryDocumentPage = {
  documents: [
    {
      id: "doc-semantic",
      title: "喜欢喝茶",
      source: "ppa_semantic",
      file_type: "txt",
    },
    {
      id: "doc-profile",
      title: "画像条目",
      source: "ppa_profile",
      file_type: null,
    },
  ],
  offset: 0,
  limit: 20,
  has_more: false,
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

function rows(): NodeListOf<HTMLTableRowElement> {
  return container.querySelectorAll("tbody tr");
}

function deleteButton(id: string): HTMLButtonElement {
  const el = container.querySelector(`[data-testid="memory-doc-delete-${id}"]`);
  if (!el) throw new Error(`delete button not found: ${id}`);
  return el as HTMLButtonElement;
}

function buttonByText(text: string): HTMLButtonElement {
  const btn = Array.from(container.querySelectorAll("button")).find((b) =>
    b.textContent?.includes(text),
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn as HTMLButtonElement;
}

function dialog(): HTMLElement | null {
  return document.body.querySelector(
    '[data-testid="memory-doc-delete-dialog"]',
  );
}

function confirmButton(): HTMLButtonElement {
  const el = document.body.querySelector(
    '[data-testid="memory-doc-delete-dialog-confirm"]',
  );
  if (!el) throw new Error("confirm button not found");
  return el as HTMLButtonElement;
}

function cancelButton(): HTMLButtonElement {
  const el = dialog();
  if (!el) throw new Error("dialog not found");
  const btn = Array.from(el.querySelectorAll("button")).find(
    (b) => b.textContent === "取消",
  );
  if (!btn) throw new Error("cancel button not found");
  return btn as HTMLButtonElement;
}

/** 后端 `ApiError` 形状的 axios 拒绝值。 */
function apiError(status: number, details: string) {
  return Object.assign(new Error("request failed"), {
    isAxiosError: true,
    response: { status, data: { error: "x", details } },
  });
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;

  LIST.mockReset();
  DEL.mockReset();
  vi.mocked(toast.error).mockReset();
  vi.mocked(toast.success).mockReset();
  vi.mocked(toast.warning).mockReset();

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("MemoryDocumentList 删除", () => {
  it("J1 每行都有入口，profile 行真禁用（点了不开弹层）并给出原因", async () => {
    LIST.mockResolvedValue(TWO_ROWS);

    await renderList();

    const profileBtn = deleteButton("doc-profile");
    const semanticBtn = deleteButton("doc-semantic");
    expect(profileBtn).toBeDisabled();
    expect(profileBtn.getAttribute("title")).toContain("画像记忆");
    expect(semanticBtn).not.toBeDisabled();

    // 断言原生属性而非仅可访问性标记：aria-disabled 拦不住点击。
    expect(profileBtn.disabled).toBe(true);

    await act(async () => {
      profileBtn.click();
    });
    await flush();

    expect(dialog()).toBeNull();
    expect(DEL).not.toHaveBeenCalled();
    expect(vi.mocked(toast.success)).not.toHaveBeenCalled();
    expect(vi.mocked(toast.error)).not.toHaveBeenCalled();
    expect(vi.mocked(toast.warning)).not.toHaveBeenCalled();
  });

  it("J1b 行内删除按钮不触发行点击（详情抽屉不打开）", async () => {
    LIST.mockResolvedValue(TWO_ROWS);

    await renderList();
    await act(async () => {
      deleteButton("doc-semantic").click();
    });
    await flush();

    expect(
      document.body.querySelector('[data-testid="memory-document-detail"]'),
    ).toBeNull();
    expect(dialog()).not.toBeNull();
  });

  it("J2 确认后按 doc.id 调用并重拉列表", async () => {
    LIST.mockResolvedValueOnce(TWO_ROWS).mockResolvedValueOnce({
      documents: [TWO_ROWS.documents[1]],
      offset: 0,
      limit: 20,
      has_more: false,
    });
    DEL.mockResolvedValue({
      doc_id: "doc-semantic",
      removed_chunks: 2,
      audit_id: 7,
    });

    await renderList();
    expect(rows()).toHaveLength(2);

    await act(async () => {
      deleteButton("doc-semantic").click();
    });
    await flush();
    expect(dialog()?.textContent).toContain("喜欢喝茶");

    await act(async () => {
      confirmButton().click();
    });
    await flush();

    expect(DEL).toHaveBeenCalledTimes(1);
    expect(DEL).toHaveBeenCalledWith("doc-semantic");
    expect(vi.mocked(toast.success)).toHaveBeenCalledWith("已删除");
    expect(rows()).toHaveLength(1);
    expect(dialog()).toBeNull();
  });

  it("J3 提交中确认按钮与取消按钮都禁用", async () => {
    LIST.mockResolvedValue(TWO_ROWS);
    DEL.mockReturnValue(new Promise<never>(() => {}));

    await renderList();
    await act(async () => {
      deleteButton("doc-semantic").click();
    });
    await flush();
    await act(async () => {
      confirmButton().click();
    });
    await flush();

    expect(confirmButton().textContent).toContain("删除中…");
    expect(confirmButton()).toBeDisabled();
    expect(cancelButton()).toBeDisabled();
  });

  it("J4 404 → warning toast + 关层 + 重拉", async () => {
    LIST.mockResolvedValue(TWO_ROWS);
    DEL.mockRejectedValue(apiError(404, "文档不存在或已被删除"));

    await renderList();
    await act(async () => {
      deleteButton("doc-semantic").click();
    });
    await flush();
    await act(async () => {
      confirmButton().click();
    });
    await flush();

    expect(vi.mocked(toast.warning)).toHaveBeenCalledWith("该文档已被删除");
    expect(dialog()).toBeNull();
    expect(LIST).toHaveBeenCalledTimes(2);
  });

  it("J5 500 → 弹层保留、层内报错、确认按钮可重试", async () => {
    LIST.mockResolvedValue(TWO_ROWS);
    DEL.mockRejectedValue(
      apiError(500, "failed to delete memory document: boom"),
    );

    await renderList();
    await act(async () => {
      deleteButton("doc-semantic").click();
    });
    await flush();
    await act(async () => {
      confirmButton().click();
    });
    await flush();

    expect(dialog()).not.toBeNull();
    expect(dialog()?.textContent).toContain("boom");
    expect(confirmButton()).not.toBeDisabled();
  });

  it("J6 删空末页时回退到上一页", async () => {
    LIST.mockResolvedValueOnce({
      documents: [TWO_ROWS.documents[0]],
      offset: 0,
      limit: 20,
      has_more: true,
    })
      .mockResolvedValueOnce({
        documents: [
          {
            id: "doc-page2",
            title: "第二页条目",
            source: "ppa_semantic",
            file_type: "txt",
          },
        ],
        offset: 20,
        limit: 20,
        has_more: true,
      })
      .mockResolvedValueOnce({
        documents: [],
        offset: 20,
        limit: 20,
        has_more: false,
      })
      .mockResolvedValueOnce({
        documents: [TWO_ROWS.documents[0]],
        offset: 0,
        limit: 20,
        has_more: true,
      });
    DEL.mockResolvedValue({
      doc_id: "doc-page2",
      removed_chunks: 1,
      audit_id: 9,
    });

    await renderList();
    await act(async () => {
      buttonByText("下一页").click();
    });
    await flush();
    expect(LIST).toHaveBeenLastCalledWith(20, 20);

    await act(async () => {
      deleteButton("doc-page2").click();
    });
    await flush();
    await act(async () => {
      confirmButton().click();
    });
    await flush();

    // 末页删空 → offset 回退到 0 并再拉一次，用户不会停在空页
    expect(LIST.mock.calls.map((c) => c[0])).toEqual([0, 20, 20, 0]);
  });
});
