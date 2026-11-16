import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TooltipProvider } from "@/components/ui/tooltip";
import { DockShell } from "@/components/dock/DockShell";
import { useDockStore } from "@/stores/dockStore";
import { useSpaceActivityStore } from "@/stores/spaceActivityStore";
import { useUiStore } from "@/stores/uiStore";

// 只挂抽屉自己的依赖：这几条测的是「哪一页该有它」以及取不到会话时的退路。
// 整棵树里页面壳会 ensure 同一条会话（同一个 key、还会去重），计数就分不出是谁打的。
const mocks = vi.hoisted(() => ({
  ensure: vi.fn(async (_wsId: string) => ({
    conversation_id: "conv-space",
    created: false,
  })),
  artifacts: vi.fn(async (): Promise<unknown[]> => []),
}));

vi.mock("@/api/peco", () => ({
  getPeco: async () => ({
    ws_id: "ws_peco",
    work_dir: "/tmp/peco",
    team_id: "peco",
    reserved: true,
    from_marker: true,
  }),
}));

vi.mock("@/api/spaces", () => ({
  ensureSpaceConversation: mocks.ensure,
  spaceArtifacts: async () => ({ items: [], total: 0 }),
}));

vi.mock("@/api/conversations", () => ({
  getMessages: async () => [],
  conversationStreamUrl: () => "/stream",
  conversationLiveStreamUrl: () => "/stream/live",
  cancelConversationTurn: async () => "accepted",
  uploadConversationImage: async () => ({ id: "img", url: "/img" }),
  listConversationArtifacts: mocks.artifacts,
}));

function renderDock(path: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const view = render(
    <QueryClientProvider client={client}>
      <TooltipProvider>
        <MemoryRouter initialEntries={[path]}>
          <DockShell />
        </MemoryRouter>
      </TooltipProvider>
    </QueryClientProvider>,
  );
  return { client, ...view };
}

const dockPanel = (container: HTMLElement) =>
  container.querySelector("#peco-dock");

beforeEach(() => {
  localStorage.clear();
  mocks.ensure.mockReset();
  mocks.ensure.mockImplementation(async (_wsId: string) => ({
    conversation_id: "conv-space",
    created: false,
  }));
  mocks.artifacts.mockReset();
  mocks.artifacts.mockImplementation(async () => []);
  useDockStore.setState({ open: true, tab: "chat" });
  useSpaceActivityStore.setState({ running: {} });
  useUiStore.setState({ railOpen: true });
  // 聊天壳挂载即订阅在途轮：本文件不测 attach，统一回「没有在途轮」。
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(null, { status: 204 })),
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
  useDockStore.setState({ open: false, tab: "chat" });
  useSpaceActivityStore.setState({ running: {} });
  useUiStore.setState({ railOpen: true });
});

describe("抽屉的存在范围", () => {
  it("在 /peco 上不渲染，也不替这一页建会话", async () => {
    const { client, container, unmount } = renderDock("/peco");
    // 身份到手才轮到判断「这一页要不要抽屉」，等它落定再断言。
    await waitFor(() =>
      expect(client.getQueryState(["peco"])?.status).toBe("success"),
    );
    // 抽屉若还在，这一刻紧接着就会打一次 ensure：给足一拍再看。
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(mocks.ensure).not.toHaveBeenCalled();
    expect(dockPanel(container)).toBeNull();
    unmount();
  });

  it("非 Peco 页照常取回 Peco 空间的会话", async () => {
    const { container, unmount } = renderDock("/spaces");
    await waitFor(() => expect(mocks.ensure).toHaveBeenCalledWith("ws_peco"));

    expect(dockPanel(container)).not.toBeNull();
    unmount();
  });
});

describe("抽屉的活动上报", () => {
  it("切到「产物」卸掉聊天时把自己那一笔撤掉，头部不留「生成中」", async () => {
    const { container, unmount } = renderDock("/spaces");
    await waitFor(() =>
      expect(
        dockPanel(container)?.querySelector("[data-slot='message-scroller']"),
      ).not.toBeNull(),
    );

    // 模拟这一轮正在跑（真实来路上由 ChatView 上报）。
    act(() => {
      useSpaceActivityStore.getState().setStreaming("ws_peco", "dock", true);
    });
    fireEvent.click(screen.getByRole("button", { name: "产物" }));

    // 聊天卸掉后没有任何回调会再上报，只能靠卸载复位。
    await waitFor(() =>
      expect(
        useSpaceActivityStore.getState().running["ws_peco"],
      ).toBeUndefined(),
    );
    unmount();
  });
});

describe("抽屉的错误态", () => {
  it("取不到会话时给可重试的错误条，不是一句死文案", async () => {
    mocks.ensure.mockRejectedValue(new Error("空间不存在"));
    const { unmount } = renderDock("/spaces");

    // 空间不存在是 404，与「没打开」无关：文案照实说，并给一条出路。
    expect(await screen.findByText("空间不存在")).toBeInTheDocument();
    await waitFor(() => expect(mocks.ensure).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole("button", { name: "重试" }));

    // 查询带 retryOnMount: false，重新挂载不会重取，出路只有这枚按钮。
    await waitFor(() => expect(mocks.ensure).toHaveBeenCalledTimes(2));
    unmount();
  });
});

describe("抽屉的布局", () => {
  const chatReady = (panel: HTMLElement) =>
    waitFor(() =>
      expect(
        panel.querySelector("[data-slot='message-scroller']"),
      ).not.toBeNull(),
    );

  it("聊天面板不自带标题行：tab 行已经是这块面板的头", async () => {
    const { container, unmount } = renderDock("/spaces");
    const panel = dockPanel(container) as HTMLElement;
    await chatReady(panel);

    // 再给 ChatView 传一条 headerTitle 就会把同一块 chrome 叠两遍。
    const titles = [...panel.querySelectorAll("h2")].map((h) => h.textContent);
    expect(titles).not.toContain("Peco");
    unmount();
  });

  it("gutter 只有 content 容器这一处来源", async () => {
    const { container, unmount } = renderDock("/spaces");
    const panel = dockPanel(container) as HTMLElement;
    await chatReady(panel);

    // 面板是「tab 行 + content」两段；gutter 若回落到各分支自带的外边距，
    // 这个唯一的容器就不会带上它。
    const content = panel.lastElementChild as HTMLElement;
    expect(
      content.querySelector("[data-slot='message-scroller']"),
    ).not.toBeNull();
    expect(content).toHaveClass("px-4", "pt-3", "pb-3");
    unmount();
  });

  it("tab 行左内边距与内容同轴，关闭按钮与 tab pill 同高", async () => {
    const { container, unmount } = renderDock("/spaces");
    const panel = dockPanel(container) as HTMLElement;
    await chatReady(panel);

    expect(panel.firstElementChild).toHaveClass("px-4");
    // 24px 是命中区域下限，28px 才与 tab pill 等高。
    expect(screen.getByRole("button", { name: "关闭 Peco 对话" })).toHaveClass(
      "h-7",
      "w-7",
    );
    unmount();
  });

  it("产物行不再自带横内边距：16px 内容边距上不再叠 12px", async () => {
    mocks.artifacts.mockResolvedValue([
      {
        id: "art-1",
        conversation_id: "conv-space",
        kind: "document",
        mime: "text/markdown",
        title: "报告.md",
        size_bytes: 1024,
        url: "/api/artifacts/art-1/content",
        download_url: "/api/artifacts/art-1/download",
        created_at: "2026-01-01 00:00:00",
        available: true,
      },
    ]);
    const { unmount } = renderDock("/spaces");
    fireEvent.click(screen.getByRole("button", { name: "产物" }));

    const link = await screen.findByRole("link", { name: "报告.md" });
    const row = link.closest("li") as HTMLElement;
    expect(row).not.toBeNull();
    expect(row.className).not.toContain("px-3");
    unmount();
  });
});
