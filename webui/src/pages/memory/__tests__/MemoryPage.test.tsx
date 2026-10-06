// MemoryPage 测试 —— 历史 tab 置顶 / 默认选中列表 / 待处理取代徽章 / 刷新接线
// （展示分层 + TASK-S3b-UI Q1）

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { toast } from "sonner";
import { MemoryPage } from "../MemoryPage";
import {
  getSupersedeHealth,
  listMemoryAudit,
  listMemoryDocuments,
  triggerSupersedeReconcile,
} from "@/api/memory";
import type { SupersedeHealth } from "@/types/memory";

vi.mock("@/api/memory", () => ({
  getMemoryGraph: vi.fn(),
  listMemoryDocuments: vi.fn(),
  searchMemoryDocuments: vi.fn(),
  getMemoryDocument: vi.fn(),
  listMemoryAudit: vi.fn(),
  restoreMemoryAudit: vi.fn(),
  getSupersedeHealth: vi.fn(),
  triggerSupersedeReconcile: vi.fn(),
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), warning: vi.fn() },
}));

// jsdom 未实现 ResizeObserver（radix ScrollArea）；Select 亦需指针捕获 API。
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

const HEALTH = vi.mocked(getSupersedeHealth);
const AUDIT = vi.mocked(listMemoryAudit);
const DOCS = vi.mocked(listMemoryDocuments);
const RECON = vi.mocked(triggerSupersedeReconcile);
const TOAST_ERROR = vi.mocked(toast.error);
const TOAST_WARNING = vi.mocked(toast.warning);

const ZERO_HEALTH: SupersedeHealth = {
  pending: 0,
  processing: 0,
  failed: 0,
  degraded: 0,
  last_converged_at: null,
};

let container: HTMLDivElement;
let root: Root;

async function renderPage() {
  await act(async () => {
    root.render(<MemoryPage />);
  });
  await flush();
}

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function tabTriggers(): HTMLButtonElement[] {
  return Array.from(
    container.querySelectorAll<HTMLButtonElement>('[role="tab"]'),
  );
}

function tabByText(text: string): HTMLButtonElement {
  const tab = tabTriggers().find((t) => t.textContent?.includes(text));
  if (!tab) throw new Error(`tab not found: ${text}`);
  return tab;
}

/** radix TabsTrigger 在 mousedown（button=0、无 ctrl）上激活，click 不切 tab。 */
async function activateTab(tab: HTMLButtonElement) {
  await act(async () => {
    tab.dispatchEvent(
      new MouseEvent("mousedown", { bubbles: true, button: 0 }),
    );
  });
  await flush();
}

function buttonByText(text: string): HTMLButtonElement {
  const btn = Array.from(container.querySelectorAll("button")).find((b) =>
    b.textContent?.includes(text),
  );
  if (!btn) throw new Error(`button not found: ${text}`);
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

  HEALTH.mockReset().mockResolvedValue(ZERO_HEALTH);
  RECON.mockReset().mockResolvedValue(ZERO_HEALTH);
  TOAST_ERROR.mockClear();
  TOAST_WARNING.mockClear();
  AUDIT.mockReset().mockResolvedValue([]);
  DOCS.mockReset().mockResolvedValue({
    documents: [],
    offset: 0,
    limit: 20,
    has_more: false,
  });

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("MemoryPage", () => {
  it("tab 顺序为 历史 / 图谱 / 列表，默认选中「列表」", async () => {
    await renderPage();

    expect(tabTriggers().map((t) => t.textContent)).toEqual([
      "历史",
      "图谱",
      "列表",
    ]);
    expect(tabByText("列表").getAttribute("aria-selected")).toBe("true");
    // 默认落在列表：文档列表已挂载，历史内容未挂载（radix 非激活面板卸载）
    expect(container.textContent).toContain("还没有记忆文档");
    expect(AUDIT).not.toHaveBeenCalled();
  });

  it("health pending+failed>0 时历史触发钮显示徽章", async () => {
    HEALTH.mockResolvedValue({
      pending: 2,
      processing: 1,
      failed: 3,
      degraded: 0,
      last_converged_at: null,
    });

    await renderPage();

    const badge = tabByText("历史").querySelector('[data-slot="badge"]');
    expect(badge).toBeTruthy();
    // 徽章口径 = pending + failed（processing 不计）
    expect(badge?.textContent).toBe("5");
    expect(badge?.getAttribute("title")).toBe("待处理取代");
  });

  it("health 全零时不显示徽章", async () => {
    await renderPage();

    expect(tabByText("历史").querySelector('[data-slot="badge"]')).toBeNull();
  });

  it("health 拉取失败静默降级：页面正常、无徽章", async () => {
    HEALTH.mockRejectedValue(new Error("boom"));

    await renderPage();

    // 不阻断页面：默认列表照常渲染，无错误横幅
    expect(container.textContent).toContain("还没有记忆文档");
    expect(container.textContent).not.toContain("boom");
    expect(tabByText("历史").querySelector('[data-slot="badge"]')).toBeNull();
  });

  it("切到历史 tab 挂载审计列表，刷新按钮触发其重拉", async () => {
    await renderPage();
    expect(AUDIT).not.toHaveBeenCalled();

    await activateTab(tabByText("历史"));
    expect(AUDIT).toHaveBeenCalledTimes(1);
    expect(AUDIT).toHaveBeenCalledWith(0, 20, "superseded");

    await act(async () => {
      buttonByText("刷新").click();
    });
    await flush();

    expect(AUDIT).toHaveBeenCalledTimes(2);
    expect(AUDIT).toHaveBeenLastCalledWith(0, 20, "superseded");
  });

  it("列表 tab 激活时刷新按钮只刷文档列表，不碰审计接口", async () => {
    await renderPage();

    await act(async () => {
      buttonByText("刷新").click();
    });
    await flush();

    expect(DOCS.mock.calls.length).toBeGreaterThan(1);
    expect(AUDIT).not.toHaveBeenCalled();
  });

  it("历史 tab 提供「立即对账」：触发 reconcile 并用返回计数刷新徽章与列表", async () => {
    RECON.mockResolvedValue({
      pending: 1,
      processing: 0,
      failed: 2,
      degraded: 0,
      last_converged_at: null,
    });
    await renderPage();
    await activateTab(tabByText("历史"));
    expect(AUDIT).toHaveBeenCalledTimes(1);
    expect(tabByText("历史").querySelector('[data-slot="badge"]')).toBeNull();

    await act(async () => {
      buttonByText("立即对账").click();
    });
    await flush();
    await flush();

    expect(RECON).toHaveBeenCalledTimes(1);
    expect(AUDIT).toHaveBeenCalledTimes(2);
    // 徽章 = pending + failed = 3，直接由 reconcile 返回值更新（不再 GET health）
    expect(
      tabByText("历史").querySelector('[data-slot="badge"]')?.textContent,
    ).toBe("3");
    expect(TOAST_WARNING).toHaveBeenCalled();
  });

  it("对账失败：弹出错误提示、不重拉列表", async () => {
    RECON.mockRejectedValue(new Error("boom"));
    await renderPage();
    await activateTab(tabByText("历史"));

    await act(async () => {
      buttonByText("立即对账").click();
    });
    await flush();
    await flush();

    expect(RECON).toHaveBeenCalledTimes(1);
    expect(AUDIT).toHaveBeenCalledTimes(1); // 失败不刷新列表
    expect(TOAST_ERROR).toHaveBeenCalled(); // 有可见错误提示
    // 页面仍在，按钮回到可再试态
    expect(buttonByText("立即对账")).toBeTruthy();
  });
});
