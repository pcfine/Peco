import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { SpaceFilesTab } from "@/components/spaces/SpaceFilesTab";
import { SPACE_FILES_POLL_MS } from "@/queries/spaceDetail";
import { FILE_HIGHLIGHT_MS } from "@/lib/filesSnapshot";
import type { SpaceFileEntry, SpaceFilesResponse } from "@/types/daemon";

// 文件面板的自动刷新：可见时轮询、隐藏时停、新条目高亮到点自己消退。
// 数据层换成可控的假后端，因为要断言的正是「重取之后界面变了什么」。

const mocks = vi.hoisted(() => ({
  files:
    vi.fn<
      (wsId: string, options: { path?: string }) => Promise<SpaceFilesResponse>
    >(),
}));

vi.mock("@/api/spaces", () => ({
  spaceFiles: (wsId: string, options: { path?: string }) =>
    mocks.files(wsId, options),
  getSpace: async () => {
    throw new Error("文件面板用不到空间详情");
  },
  spaceArtifacts: async () => ({ items: [], total: 0 }),
  spaceTasks: async () => [],
  spaceLiveTasks: async () => [],
  spaceMemoryDocuments: async () => ({ total: 0, items: [] }),
  spaceMemoryDocument: async () => {
    throw new Error("文件面板用不到记忆全文");
  },
}));

function entry(overrides: Partial<SpaceFileEntry> = {}): SpaceFileEntry {
  return {
    name: "a.txt",
    kind: "file",
    size_bytes: 10,
    modified_at: "2026-11-05T10:00:00Z",
    ...overrides,
  };
}

function listing(entries: SpaceFileEntry[]): SpaceFilesResponse {
  return { path: "", entries, truncated: false };
}

/**
 * 假定时器下 react-query 的落定还要一次 0ms 定时器，而 `findBy`/`waitFor` 自己
 * 推不动被接管的时钟 —— 这里手动把这一拍放掉。
 */
async function flush(): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
}

function renderTab(props: { active?: boolean } = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const element = (active: boolean) => (
    <QueryClientProvider client={client}>
      <SpaceFilesTab wsId="ws_a" active={active} />
    </QueryClientProvider>
  );
  const view = render(element(props.active ?? true));
  return {
    client,
    rerender: (next: { active?: boolean }) =>
      view.rerender(element(next.active ?? true)),
  };
}

describe("SpaceFilesTab 自动刷新", () => {
  beforeEach(() => {
    mocks.files.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("首次看一个目录不产生高亮", async () => {
    mocks.files.mockResolvedValue(listing([entry({ name: "a.txt" })]));
    renderTab();

    expect(await screen.findByText("a.txt")).toBeInTheDocument();
    expect(document.querySelector("[data-highlight]")).toBeNull();
    expect(screen.queryByTitle("新增")).toBeNull();
    expect(screen.queryByTitle("已更新")).toBeNull();
  });

  it("第一次进子目录也不高亮（换目录会先挂着上一个目录的列表）", async () => {
    mocks.files.mockImplementation(async (_wsId, options) =>
      options.path === "sub"
        ? listing([entry({ name: "b.txt" }), entry({ name: "c.txt" })])
        : listing([
            entry({ name: "sub", kind: "dir" }),
            entry({ name: "a.txt" }),
          ]),
    );
    renderTab();

    expect(await screen.findByText("a.txt")).toBeInTheDocument();
    fireEvent.click(screen.getByText("sub"));

    expect(await screen.findByText("b.txt")).toBeInTheDocument();
    expect(document.querySelector("[data-highlight]")).toBeNull();
  });

  it("刷新后新增的条目高亮，8 秒后消退", async () => {
    vi.useFakeTimers();
    mocks.files.mockResolvedValueOnce(listing([entry({ name: "a.txt" })]));
    const { client } = renderTab();
    await flush();

    expect(screen.getByText("a.txt")).toBeInTheDocument();
    expect(document.querySelector("[data-highlight]")).toBeNull();

    mocks.files.mockResolvedValue(
      listing([
        entry({ name: "a.txt" }),
        entry({ name: "b.txt", size_bytes: 3 }),
      ]),
    );
    await act(async () => {
      await client.invalidateQueries({ queryKey: ["space-files"] });
    });
    await flush();

    const marked = screen.getByTitle("新增");
    expect(marked).toHaveAttribute("data-highlight", "new");
    expect(marked).toHaveTextContent("b.txt");
    // 颜色之外还有一条可读信号：屏幕阅读器读得到，字形也看得见。
    expect(within(marked).getByText("新增")).toHaveClass("sr-only");
    expect(within(marked).getByText("+")).toBeInTheDocument();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(FILE_HIGHLIGHT_MS);
    });
    expect(screen.queryByTitle("新增")).toBeNull();
    expect(document.querySelector("[data-highlight]")).toBeNull();
  });

  it("同名文件大小变了标成已更新", async () => {
    mocks.files.mockResolvedValueOnce(
      listing([entry({ name: "a.txt", size_bytes: 10 })]),
    );
    const { client } = renderTab();

    expect(await screen.findByText("a.txt")).toBeInTheDocument();
    expect(document.querySelector("[data-highlight]")).toBeNull();

    mocks.files.mockResolvedValue(
      listing([entry({ name: "a.txt", size_bytes: 20 })]),
    );
    await act(async () => {
      await client.invalidateQueries({ queryKey: ["space-files"] });
    });

    const marked = await screen.findByTitle("已更新");
    expect(marked).toHaveAttribute("data-highlight", "updated");
    expect(within(marked).getByText("~")).toBeInTheDocument();
  });

  it("active=false 不轮询，转成可见后才按 7 秒重取", async () => {
    vi.useFakeTimers();
    mocks.files.mockResolvedValue(listing([entry({ name: "a.txt" })]));
    const { rerender } = renderTab({ active: false });
    await flush();

    expect(mocks.files).toHaveBeenCalledTimes(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(SPACE_FILES_POLL_MS * 2);
    });
    expect(mocks.files).toHaveBeenCalledTimes(1);

    rerender({ active: true });
    await flush();
    const beforeInterval = mocks.files.mock.calls.length;

    await act(async () => {
      await vi.advanceTimersByTimeAsync(SPACE_FILES_POLL_MS + 500);
    });
    expect(mocks.files).toHaveBeenCalledTimes(beforeInterval + 1);
  });
});
