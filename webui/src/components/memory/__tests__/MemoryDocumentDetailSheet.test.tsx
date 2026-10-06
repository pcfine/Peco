// MemoryDocumentDetailSheet 组件测试 —— 打开即请求 / 四态 / 关闭不影响列表

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryDocumentDetailSheet } from "../MemoryDocumentDetailSheet";
import { getMemoryDocument } from "@/api/memory";
import type { MemoryDocumentDetail } from "@/types/memory";

vi.mock("@/api/memory", () => ({
  getMemoryGraph: vi.fn(),
  listMemoryDocuments: vi.fn(),
  searchMemoryDocuments: vi.fn(),
  getMemoryDocument: vi.fn(),
}));

const DETAIL = vi.mocked(getMemoryDocument);

// jsdom 未实现 ResizeObserver（radix ScrollArea）；Dialog 亦需指针捕获 API。
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

const DOC: MemoryDocumentDetail = {
  id: "ef5c3ede5ae751ce",
  title: "chen 的朋友小C及其作品",
  source: "ppa_semantic",
  file_type: "txt",
  created_at: "2026-09-16T09:57:58.915195367+00:00",
  content: "hello",
};

let container: HTMLDivElement;
let root: Root;

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

/** 受控 harness：关闭时把 docId 置空，模拟父组件行为。 */
function Harness({
  initialId,
  onClose,
}: {
  initialId: string | null;
  onClose: () => void;
}) {
  const [docId, setDocId] = useState<string | null>(initialId);
  return (
    <MemoryDocumentDetailSheet
      docId={docId}
      onClose={() => {
        setDocId(null);
        onClose();
      }}
    />
  );
}

async function renderSheet(id: string | null, onClose: () => void = () => {}) {
  await act(async () => {
    root.render(<Harness initialId={id} onClose={onClose} />);
  });
  await flush();
}

function closeButton(): HTMLButtonElement {
  const btn = Array.from(document.body.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Close"),
  );
  if (!btn) throw new Error("close button not found");
  return btn as HTMLButtonElement;
}

function retryButton(): HTMLButtonElement {
  const btn = Array.from(document.body.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === "重试",
  );
  if (!btn) throw new Error("retry button not found");
  return btn as HTMLButtonElement;
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

  DETAIL.mockReset();

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("MemoryDocumentDetailSheet", () => {
  it("打开即请求，成功渲染标题 / 元数据 / 正文", async () => {
    DETAIL.mockResolvedValue(DOC);

    await renderSheet("ef5c3ede5ae751ce");

    expect(DETAIL).toHaveBeenCalledTimes(1);
    expect(DETAIL).toHaveBeenCalledWith("ef5c3ede5ae751ce");

    const text = document.body.textContent ?? "";
    expect(text).toContain("chen 的朋友小C及其作品");
    expect(text).toContain("ppa_semantic");
    expect(text).toContain("txt");
    expect(text).toContain("2026");
    expect(text).toContain("约 5 字");
    expect(text).toContain("hello");
  });

  it("空正文显示占位文案，不渲染滚动区", async () => {
    DETAIL.mockResolvedValue({ ...DOC, content: "" });

    await renderSheet("ef5c3ede5ae751ce");

    expect(document.body.textContent).toContain("该文档没有正文内容");
    expect(document.body.querySelector('[data-slot="scroll-area"]')).toBeNull();
  });

  it("404 显示「文档不存在或已被删除」+ 关闭", async () => {
    DETAIL.mockRejectedValue(
      Object.assign(new Error("not found"), {
        isAxiosError: true,
        response: { status: 404 },
      }),
    );

    await renderSheet("missing");

    const text = document.body.textContent ?? "";
    expect(text).toContain("文档不存在或已被删除");
    expect(text).toContain("关闭");
  });

  it("非 404 错误显示 ErrorBanner + 重试，点击重试再请求一次", async () => {
    DETAIL.mockRejectedValue(new Error("boom"));

    await renderSheet("ef5c3ede5ae751ce");

    expect(document.body.textContent).toContain("boom");
    expect(DETAIL).toHaveBeenCalledTimes(1);

    await act(async () => {
      retryButton().click();
    });
    await flush();

    expect(DETAIL).toHaveBeenCalledTimes(2);
  });

  it("关闭触发 onClose，且不会再次请求详情", async () => {
    DETAIL.mockResolvedValue(DOC);
    const onClose = vi.fn();

    await renderSheet("ef5c3ede5ae751ce", onClose);
    expect(DETAIL).toHaveBeenCalledTimes(1);

    await act(async () => {
      closeButton().click();
    });
    await flush();

    expect(onClose).toHaveBeenCalledTimes(1);
    expect(DETAIL).toHaveBeenCalledTimes(1);
  });

  it("created_at 为 null 时不渲染时间字段（不显示 - / Invalid Date）", async () => {
    DETAIL.mockResolvedValue({ ...DOC, created_at: null });

    await renderSheet("ef5c3ede5ae751ce");

    const text = document.body.textContent ?? "";
    expect(text).not.toContain("Invalid Date");
    expect(text).toContain("类型 txt");
    expect(
      document.querySelector('[data-testid="memory-detail-created-at"]'),
    ).toBeNull();
  });

  it("created_at 存在时渲染时间字段", async () => {
    DETAIL.mockResolvedValue(DOC);

    await renderSheet("ef5c3ede5ae751ce");

    expect(
      document.querySelector('[data-testid="memory-detail-created-at"]'),
    ).not.toBeNull();
  });
});
