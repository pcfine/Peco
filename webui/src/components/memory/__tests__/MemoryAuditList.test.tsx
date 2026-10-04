// MemoryAuditList 组件测试 —— 被取代行渲染 / 回滚确认与失败提示 / 追加分页 / 空态
// （design §11 历史 tab；裸数组分页）

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, createRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  MemoryAuditList,
  type MemoryAuditListHandle,
} from "../MemoryAuditList";
import { listMemoryAudit, restoreMemoryAudit } from "@/api/memory";
import type { MemoryAuditItem } from "@/types/memory";
import { toast } from "sonner";

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
  toast: { error: vi.fn(), success: vi.fn() },
}));

const LIST = vi.mocked(listMemoryAudit);
const RESTORE = vi.mocked(restoreMemoryAudit);

const ROW: MemoryAuditItem = {
  id: 7,
  kb_name: "@private_memory",
  doc_id: "c6cd2285ca896c7d",
  title: "喜欢喝茶",
  content: "…",
  source: "ppa_semantic",
  reason: "superseded",
  deleted_by: "memory",
  status: "done",
  deleted_at: "2026-09-20T10:30:00Z",
  topic_key: "semantic:喜欢喝茶",
  successor_doc_id: "aabbccddeeff0011",
  successor_title: "喜欢喝绿茶",
  retention_days_remaining: 12,
};

let container: HTMLDivElement;
let root: Root;

async function renderList(ref?: React.Ref<MemoryAuditListHandle>) {
  await act(async () => {
    root.render(<MemoryAuditList ref={ref} />);
  });
  await flush();
}

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function buttons(): HTMLButtonElement[] {
  return Array.from(container.querySelectorAll("button"));
}

function buttonByText(text: string): HTMLButtonElement {
  const btn = buttons().find((b) => b.textContent?.includes(text));
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn;
}

function rows(): NodeListOf<HTMLTableRowElement> {
  return container.querySelectorAll("tbody tr");
}

/** 回滚确认弹层挂在 document.body（portal），不在 container 内。 */
function dialogEl(): HTMLElement {
  const el = document.body.querySelector('[role="dialog"]');
  if (!el) throw new Error("dialog not found");
  return el as HTMLElement;
}

function dialogButtonByText(text: string): HTMLButtonElement {
  const btn = Array.from(dialogEl().querySelectorAll("button")).find((b) =>
    b.textContent?.includes(text),
  );
  if (!btn) throw new Error(`dialog button not found: ${text}`);
  return btn as HTMLButtonElement;
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;

  LIST.mockReset();
  RESTORE.mockReset();
  vi.mocked(toast.success).mockReset();
  vi.mocked(toast.error).mockReset();

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("MemoryAuditList", () => {
  it("首屏以 reason=superseded 拉取并渲染后继 / 天数 / 状态", async () => {
    LIST.mockResolvedValue([ROW]);

    await renderList();

    expect(LIST).toHaveBeenCalledWith(0, 20, "superseded");
    expect(rows()).toHaveLength(1);
    const text = rows()[0].textContent ?? "";
    expect(text).toContain("喜欢喝茶");
    expect(text).toContain("喜欢喝绿茶");
    expect(text).toContain("剩余 12 天");
    expect(text).toContain("done");
    // 变更时间为本地化展示
    expect(text).toContain("2026");
  });

  it("successor_title 缺失 / 空串、天数缺失时渲染 —", async () => {
    LIST.mockResolvedValue([
      {
        ...ROW,
        id: 8,
        successor_title: undefined,
        retention_days_remaining: undefined,
      },
      { ...ROW, id: 9, successor_title: "" },
    ]);

    await renderList();

    expect(rows()).toHaveLength(2);
    // 第一行：后继与天数都缺省 ⇒ 两个 —；第二行：空串后继 ⇒ —
    expect(rows()[0].textContent).toContain("—");
    expect(rows()[0].textContent).not.toContain("剩余");
    expect(rows()[1].textContent).toContain("—");
  });

  it("已回滚行显示「已回滚」且禁用回滚按钮", async () => {
    LIST.mockResolvedValue([{ ...ROW, restored_at: "2026-09-21T08:00:00Z" }]);

    await renderList();

    expect(rows()[0].textContent).toContain("已回滚");
    expect(buttonByText("回滚")).toBeDisabled();
  });

  it("点回滚 → 确认弹层 → 调 restore 接口 → toast 成功并刷新列表", async () => {
    LIST.mockResolvedValueOnce([ROW]).mockResolvedValueOnce([
      { ...ROW, restored_at: "2026-09-21T08:00:00Z" },
    ]);
    RESTORE.mockResolvedValue({
      success: true,
      doc_id: "c6cd2285ca896c7d",
      restored_at: "2026-09-21T08:00:00Z",
    });

    await renderList();

    await act(async () => {
      buttonByText("回滚").click();
    });
    await flush();
    expect(dialogEl()).toBeTruthy();
    expect(dialogEl().textContent).toContain("喜欢喝茶");

    await act(async () => {
      dialogButtonByText("回滚").click();
    });
    await flush();

    expect(RESTORE).toHaveBeenCalledWith(7);
    expect(vi.mocked(toast.success)).toHaveBeenCalledTimes(1);
    // 成功后重拉第一页，行显示已回滚
    expect(LIST).toHaveBeenCalledTimes(2);
    expect(rows()[0].textContent).toContain("已回滚");
    // 弹层关闭
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
  });

  it("回滚失败时弹层内展示后端错误且不刷新列表", async () => {
    LIST.mockResolvedValue([ROW]);
    RESTORE.mockRejectedValue(
      Object.assign(new Error("request failed"), {
        isAxiosError: true,
        response: {
          status: 409,
          data: { message: "审计行 #7 状态为 'pending'，仅 done 记录可回滚" },
        },
      }),
    );

    await renderList();

    await act(async () => {
      buttonByText("回滚").click();
    });
    await flush();

    await act(async () => {
      dialogButtonByText("回滚").click();
    });
    await flush();

    expect(RESTORE).toHaveBeenCalledWith(7);
    expect(dialogEl().textContent).toContain("仅 done 记录可回滚");
    expect(vi.mocked(toast.success)).not.toHaveBeenCalled();
    expect(LIST).toHaveBeenCalledTimes(1);

    // 取消后弹层关闭
    await act(async () => {
      dialogButtonByText("取消").click();
    });
    await flush();
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
  });

  it("满页出现「加载更多」，追加式拉取；不足一页则到底", async () => {
    const pageOne = Array.from({ length: 20 }, (_, i) => ({
      ...ROW,
      id: i + 1,
      title: `条目 ${i + 1}`,
    }));
    LIST.mockResolvedValueOnce(pageOne).mockResolvedValueOnce([
      { ...ROW, id: 21, title: "条目 21" },
    ]);

    await renderList();

    expect(rows()).toHaveLength(20);
    const more = buttonByText("加载更多");

    await act(async () => {
      more.click();
    });
    await flush();

    // 追加而非替换：offset += limit，第二页 1 条 < limit ⇒ 按钮消失
    expect(LIST).toHaveBeenLastCalledWith(20, 20, "superseded");
    expect(rows()).toHaveLength(21);
    expect(buttons().some((b) => b.textContent?.includes("加载更多"))).toBe(
      false,
    );
  });

  it("refresh 句柄重置回第一页重拉", async () => {
    const ref = createRef<MemoryAuditListHandle>();
    LIST.mockResolvedValue([ROW]);

    await renderList(ref);
    expect(LIST).toHaveBeenCalledTimes(1);

    await act(async () => {
      ref.current?.refresh();
    });
    await flush();

    expect(LIST).toHaveBeenCalledTimes(2);
    expect(LIST).toHaveBeenLastCalledWith(0, 20, "superseded");
  });

  it("空数据展示引导文案", async () => {
    LIST.mockResolvedValue([]);

    await renderList();

    expect(container.textContent).toContain("暂无被取代的记忆条目");
    expect(container.textContent).toContain("取代执行开启后");
    expect(buttons().some((b) => b.textContent?.includes("加载更多"))).toBe(
      false,
    );
  });

  it("首屏 API 失败显示错误与重试，点击后重拉", async () => {
    LIST.mockRejectedValueOnce(new Error("boom")).mockResolvedValue([ROW]);

    await renderList();

    expect(container.textContent).toContain("boom");

    await act(async () => {
      buttonByText("重试").click();
    });
    await flush();

    expect(LIST).toHaveBeenCalledTimes(2);
    expect(rows()).toHaveLength(1);
  });
});
