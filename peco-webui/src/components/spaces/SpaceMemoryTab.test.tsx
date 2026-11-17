import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { SpaceMemoryTab } from "@/components/spaces/SpaceMemoryTab";
import { ApiError } from "@/lib/api";
import type { MemoryDocumentQuery } from "@/api/spaces";
import type {
  MemoryDocumentDetail,
  MemoryDocumentList,
  MemoryDocumentSummary,
  Space,
} from "@/types/daemon";

// 记忆面板的两条数据路：按空间过滤的清单与单篇全文。
// 清单的 409 与真正的失败必须分开 —— 前者要给「打开空间」，后者只给重试。

const mocks = vi.hoisted(() => ({
  list: vi.fn<
    (wsId: string, options: MemoryDocumentQuery) => Promise<MemoryDocumentList>
  >(),
  detail:
    vi.fn<(wsId: string, docId: string) => Promise<MemoryDocumentDetail>>(),
  getSpace: vi.fn<(wsId: string) => Promise<Space>>(),
}));

vi.mock("@/api/spaces", () => ({
  spaceMemoryDocuments: (wsId: string, options: MemoryDocumentQuery) =>
    mocks.list(wsId, options),
  spaceMemoryDocument: (wsId: string, docId: string) =>
    mocks.detail(wsId, docId),
  getSpace: (wsId: string) => mocks.getSpace(wsId),
}));

function space(overrides: Partial<Space> = {}): Space {
  return {
    ws_id: "ws_a",
    work_dir: "/tmp/spaces/ws_a/work",
    team_id: "research",
    label: "空间 A",
    exit_policy: "inherit",
    effective_exit_policy: "keep",
    status: "open",
    closed_at: null,
    last_opened_at: "2026-11-05 10:00:00",
    reserved: false,
    inflight_dispatch: 0,
    inflight_foreign: 0,
    marker: null,
    ...overrides,
  };
}

function summary(
  overrides: Partial<MemoryDocumentSummary> = {},
): MemoryDocumentSummary {
  return {
    id: "doc_1",
    title: "回复偏好",
    source: "memory/preferences.md",
    category: "profile",
    created_at: "2026-11-05T10:00:00Z",
    snippet: "用户希望所有回复都用中文。",
    ...overrides,
  };
}

function page(items: MemoryDocumentSummary[], total = items.length) {
  return { total, items };
}

interface MemoryTabProps {
  open?: boolean;
}

function renderTab(props: MemoryTabProps = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const element = (next: MemoryTabProps) => (
    <QueryClientProvider client={client}>
      <SpaceMemoryTab wsId="ws_a" open={next.open ?? true} />
    </QueryClientProvider>
  );
  const view = render(element(props));
  return {
    ...view,
    rerender: (next: MemoryTabProps) => view.rerender(element(next)),
  };
}

/** 放掉一轮宏任务：让「本该不发的请求」有机会暴露自己。 */
async function settle(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

beforeEach(() => {
  mocks.list.mockReset();
  mocks.detail.mockReset();
  mocks.getSpace.mockReset();
  mocks.list.mockResolvedValue(page([]));
  mocks.getSpace.mockResolvedValue(space());
  mocks.detail.mockResolvedValue({
    id: "doc_1",
    title: "回复偏好",
    content: "用户希望所有回复都用中文。",
    source: "memory/preferences.md",
    category: "profile",
    created_at: "2026-11-05T10:00:00Z",
    metadata: { ws_id: "ws_a" },
  });
});

describe("空间记忆面板", () => {
  it("列表给标题、类别、时间、摘要与总数，请求不带空过滤条件", async () => {
    mocks.list.mockResolvedValue(
      page([
        summary(),
        summary({
          id: "doc_2",
          title: "合同期限",
          source: "memory/contract.md",
          category: "semantic",
          snippet: "合同到期日是 2027-03-31。",
        }),
      ]),
    );
    renderTab();

    expect(await screen.findByText("回复偏好")).toBeInTheDocument();
    expect(screen.getByText("合同期限")).toBeInTheDocument();
    // 类别标签是中文，不用后端那串 profile / semantic。
    expect(screen.getAllByText("偏好").length).toBeGreaterThan(0);
    expect(screen.getAllByText("事实").length).toBeGreaterThan(0);
    expect(screen.getByText("用户希望所有回复都用中文。")).toBeInTheDocument();
    expect(screen.getByTestId("memory-count")).toHaveTextContent("2 条");
    // 时间按本地时区分档，只验形状：断言具体小时数会让用例依赖运行机器时区。
    expect(screen.getAllByText(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/)).toHaveLength(
      2,
    );

    expect(mocks.list).toHaveBeenCalledWith("ws_a", {
      query: undefined,
      category: undefined,
      limit: 50,
    });
  });

  it("类别过滤把 category 传给后端，按钮的 aria-pressed 跟着走", async () => {
    renderTab();
    await screen.findByText("本空间还没有记忆。");

    fireEvent.click(screen.getByRole("button", { name: "偏好" }));

    await waitFor(() =>
      expect(mocks.list).toHaveBeenLastCalledWith(
        "ws_a",
        expect.objectContaining({ category: "profile" }),
      ),
    );
    expect(screen.getByRole("button", { name: "偏好" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByRole("button", { name: "全部" })).toHaveAttribute(
      "aria-pressed",
      "false",
    );
  });

  it("搜索停顿 300ms 后才带上 query，防抖期间不发请求", async () => {
    renderTab();
    await waitFor(() => expect(mocks.list).toHaveBeenCalledTimes(1));

    fireEvent.change(screen.getByRole("searchbox", { name: "搜索记忆" }), {
      target: { value: "合同" },
    });
    // 刚敲完这一帧还没到 300ms：此时多打一次请求就是把列表刷成噪声。
    expect(mocks.list).toHaveBeenCalledTimes(1);

    await waitFor(
      () =>
        expect(mocks.list).toHaveBeenLastCalledWith(
          "ws_a",
          expect.objectContaining({ query: "合同" }),
        ),
      { timeout: 2000 },
    );
  });

  it("空态写明本空间没有记忆；筛选后换成无匹配并给清除入口", async () => {
    renderTab();
    expect(await screen.findByText("本空间还没有记忆。")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "事项" }));
    expect(await screen.findByText("没有匹配的记忆。")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "清除筛选" }));
    expect(await screen.findByText("本空间还没有记忆。")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "全部" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  it("其他错误给重试，重试后按正常路径渲染", async () => {
    mocks.list
      .mockRejectedValueOnce(new ApiError("连不上 daemon", { status: 500 }))
      .mockResolvedValue(page([summary()]));
    renderTab();

    expect(await screen.findByText("连不上 daemon")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));

    expect(await screen.findByText("回复偏好")).toBeInTheDocument();
    expect(mocks.list).toHaveBeenCalledTimes(2);
  });

  it("空间未打开：不发清单请求，摆打开入口而不是空记忆", async () => {
    mocks.getSpace.mockResolvedValue(space({ status: "closed" }));
    renderTab();

    expect(
      await screen.findByRole("button", { name: "打开空间" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/空间还没打开/)).toBeInTheDocument();
    expect(screen.queryByText("本空间还没有记忆。")).toBeNull();
    // 没实例就没有记忆可读：一个必然 409 的请求不该发出去。
    expect(mocks.list).not.toHaveBeenCalled();
  });

  it("状态说开着但清单 409：以请求为准，给打开入口与重试都不是空记忆", async () => {
    mocks.list.mockRejectedValue(
      new ApiError("空间还没打开", { status: 409, code: "space_not_open" }),
    );
    renderTab();

    expect(
      await screen.findByRole("button", { name: "打开空间" }),
    ).toBeInTheDocument();
    expect(screen.queryByText("本空间还没有记忆。")).toBeNull();
    expect(screen.queryByRole("button", { name: "重试" })).toBeNull();
  });

  it("open=false：一个清单请求都不发，只摆中性说明", async () => {
    // 详情里的状态说「开着」也不放行：15s 前的快照说不了当下有没有实例。
    renderTab({ open: false });

    expect(
      await screen.findByText("空间未打开，暂时看不到记忆。"),
    ).toBeInTheDocument();
    await settle();
    expect(mocks.list).not.toHaveBeenCalled();
    expect(screen.queryByText("本空间还没有记忆。")).toBeNull();
  });

  it("翻转成已打开后清单自己接上，说明退场", async () => {
    mocks.list.mockResolvedValue(page([summary()]));
    const { rerender } = renderTab({ open: false });

    expect(
      await screen.findByText("空间未打开，暂时看不到记忆。"),
    ).toBeInTheDocument();
    await settle();
    expect(mocks.list).not.toHaveBeenCalled();

    rerender({ open: true });

    expect(await screen.findByText("回复偏好")).toBeInTheDocument();
    expect(screen.queryByText("空间未打开，暂时看不到记忆。")).toBeNull();
    expect(mocks.list).toHaveBeenCalledTimes(1);
  });

  it("点列表项按 docId 取全文并渲染正文、来源，再点收起", async () => {
    mocks.list.mockResolvedValue(page([summary()]));
    renderTab();

    const row = await screen.findByRole("button", { name: /回复偏好/ });
    expect(row).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(row);

    await waitFor(() =>
      expect(mocks.detail).toHaveBeenCalledWith("ws_a", "doc_1"),
    );
    expect(
      await screen.findByText("用户希望所有回复都用中文。"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/来源 memory\/preferences\.md/),
    ).toBeInTheDocument();
    expect(row).toHaveAttribute("aria-expanded", "true");

    fireEvent.click(row);
    expect(row).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText(/来源 memory\/preferences\.md/)).toBeNull();
  });

  it("详情失败给重试，不让列表跟着变成错误", async () => {
    mocks.list.mockResolvedValue(page([summary()]));
    mocks.detail.mockRejectedValueOnce(
      new ApiError("本空间没有这篇记忆", { status: 404 }),
    );
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: /回复偏好/ }));
    expect(await screen.findByText("本空间没有这篇记忆")).toBeInTheDocument();
    // 列表本身还在：详情取不到不等于这份清单失效。
    expect(screen.getByText("回复偏好")).toBeInTheDocument();
  });
});
