import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { App } from "@/App";
import { useDockStore } from "@/stores/dockStore";
import { useUiStore } from "@/stores/uiStore";

// 壳的挂载冒烟：只验证路由与面板都渲染出来。数据来源全换成固定值 ——
// 这一层测的不是取数，真接口由 daemon 的集成测试覆盖。
vi.mock("@/api/peco", () => ({
  getPeco: async () => ({
    ws_id: "ws_peco",
    work_dir: "/tmp/peco",
    team_id: "peco",
    reserved: true,
    from_marker: true,
  }),
}));

const pecoSpace = {
  ws_id: "ws_peco",
  work_dir: "/tmp/peco",
  team_id: "peco",
  label: "Peco",
  exit_policy: "inherit",
  effective_exit_policy: "inherit",
  status: "open" as const,
  closed_at: null,
  last_opened_at: null,
  reserved: true,
  inflight_dispatch: 0,
  inflight_foreign: 0,
  marker: null,
};

vi.mock("@/api/spaces", () => ({
  listSpaces: async () => [pecoSpace],
  getSpace: async () => pecoSpace,
  spaceTasks: async () => [],
  spaceLiveTasks: async () => [],
  cancelSpaceTask: async () => ({ cancelled: true }),
  spaceArtifacts: async () => ({ items: [], total: 0 }),
  spaceFiles: async () => ({ path: "", entries: [], truncated: false }),
  ensureSpaceConversation: async () => ({
    conversation_id: "conv-space",
    created: false,
  }),
  closeSpace: async () => ({ stopped: true }),
  createSpace: async () => ({ ws_id: "ws_new" }),
  deleteSpace: async () => ({ purged: true }),
}));

vi.mock("@/api/teams", () => ({
  listTeams: async () => [],
  // Peco 翻转面常驻挂着，`/peco` 一开始就会读它的团队详情。
  getTeam: async (teamId: string) => ({
    id: teamId,
    display_name: "Peco",
    template_version: 1,
    root: "/tmp/peco/team",
    has_master: true,
    agents: [],
    skills: [],
    mcp_servers: [],
    knowledge_bases: [],
    spaces: [],
  }),
}));

vi.mock("@/api/agents", () => ({
  listAgents: async () => [],
  getAgent: async () => {
    throw new Error("unused in smoke test");
  },
}));

vi.mock("@/api/tasks", () => ({
  listTasks: async () => [],
}));

vi.mock("@/api/conversations", () => ({
  listConversations: async () => [{ id: "conv-dock", title: "Peco" }],
  getMessages: async () => [],
  createConversation: async () => ({ id: "conv-dock" }),
  conversationStreamUrl: () =>
    "/api/chat/@master/conversations/conv-dock/stream",
  uploadConversationImage: async () => ({ id: "img", url: "/img" }),
}));

vi.mock("@/lib/events", () => ({
  openTaskStream: () => () => {},
}));

/**
 * jsdom 的 `matchMedia` 一律返回 false（见 test/setup.ts），顶栏这类按媒体查询排版的
 * 组件要显式打开。壳的栅格分档读的是 `window.innerWidth`，见 `setViewport`。
 * 返回的函数用于复原。
 */
function useWideViewport(): () => void {
  const original = window.matchMedia;
  window.matchMedia = ((query: string) => ({
    matches: /min-width: (900|1100|1200)px/.test(query),
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
  return () => {
    window.matchMedia = original;
  };
}

/**
 * 壳的分档读 `window.innerWidth`（`useViewportWidth`）：改完补一次 resize，
 * 挂载中的壳才会跟着重排。
 */
function setViewport(width: number) {
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    writable: true,
    value: width,
  });
  act(() => {
    window.dispatchEvent(new Event("resize"));
  });
}

/** MemoryRouter 不暴露地址栏：重定向要靠一个读 location 的探针来断言。 */
function PathProbe() {
  const { pathname } = useLocation();
  return <span data-testid="path">{pathname}</span>;
}

function renderAt(path: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        <App />
        <PathProbe />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  localStorage.clear();
});

describe("壳与路由", () => {
  it("/ 重定向到 /peco，Peco 页照常渲染", async () => {
    renderAt("/");

    await waitFor(() =>
      expect(screen.getByTestId("path")).toHaveTextContent("/peco"),
    );
  });

  it("/peco 是聊天页：右栏默认总览，四块面板都在", async () => {
    renderAt("/peco");
    expect(await screen.findByTestId("space-chat-shell")).toBeInTheDocument();
    expect(screen.getByTestId("peco-panel")).toHaveAttribute(
      "data-mode",
      "overview",
    );
    expect(await screen.findByTestId("panel-today")).toBeInTheDocument();
    expect(screen.getByTestId("panel-spaces")).toBeInTheDocument();
    expect(screen.getByTestId("panel-teams")).toBeInTheDocument();
    expect(screen.getByTestId("panel-results")).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Peco 聊天" }),
    ).toBeInTheDocument();
  });

  it("/peco?face=config 是能力配置面：顶栏跟着换标题", async () => {
    renderAt("/peco?face=config");
    expect(
      await screen.findByRole("heading", { name: "Peco 能力配置" }),
    ).toBeInTheDocument();
    expect(await screen.findByTestId("peco-flip")).toHaveAttribute(
      "data-face",
      "config",
    );
  });

  it("/spaces 是列表页，空态给一句事实", async () => {
    renderAt("/spaces");
    expect(
      await screen.findByRole("heading", { name: "工作室" }),
    ).toBeInTheDocument();
    expect(await screen.findByText(/进行中/)).toBeInTheDocument();
  });

  it("/results 空台账给一句事实", async () => {
    renderAt("/results");
    expect(await screen.findByText(/台账里没有记录/)).toBeInTheDocument();
  });

  it("/spaces/new 两块同时可见，缺目录时主按钮禁用", async () => {
    renderAt("/spaces/new");
    // topbar 与页面各有一个 h1「新建空间」：这里只验两块表单同屏可见。
    expect(
      (await screen.findAllByRole("heading", { name: "新建空间" })).length,
    ).toBeGreaterThan(0);
    expect(screen.getByText("目录")).toBeInTheDocument();
    expect(screen.getByText("团队")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "创建并打开" })).toBeDisabled();
  });

  it("/schedules 是占位页，文案说清未实现", async () => {
    renderAt("/schedules");
    expect(await screen.findByText(/本期尚未实现/)).toBeInTheDocument();
  });

  it("/spaces/:wsId 是聊天页：单对话壳 + 右栏，/status 才是状态页", async () => {
    const chat = renderAt("/spaces/ws_peco");
    expect(await screen.findByTestId("space-chat-shell")).toBeInTheDocument();
    expect(screen.getByTestId("space-panel")).toHaveAttribute(
      "data-mode",
      "files",
    );
    expect(screen.getByTestId("path")).toHaveTextContent("/spaces/ws_peco");
    chat.unmount();

    renderAt("/spaces/ws_peco/status");
    expect(
      await screen.findByRole("tab", { name: /任务台账/ }),
    ).toBeInTheDocument();
    // 状态页不承载聊天：它没有单对话壳。
    expect(screen.queryByTestId("space-chat-shell")).toBeNull();
  });

  it("未注册的路径给 404 文案，不是营销页", async () => {
    renderAt("/workflows/demo");
    expect(await screen.findByText("页面不存在")).toBeInTheDocument();
  });
});

describe("白屏与 rail 宽度", () => {
  afterEach(() => {
    setViewport(1024);
  });

  it("rail 宽度：≥1100 跟折叠偏好，900–1099 强制 64", async () => {
    const restore = useWideViewport();
    try {
      setViewport(1440);
      useUiStore.setState({ railOpen: true });
      const expanded = renderAt("/");
      expect(await screen.findByTestId("panel-today")).toBeInTheDocument();
      expect(expanded.container.querySelector("aside")).toHaveClass("w-64");
      expect(
        screen.getByRole("button", { name: "折叠左栏" }),
      ).toBeInTheDocument();
      expanded.unmount();

      useUiStore.setState({ railOpen: false });
      const collapsed = renderAt("/");
      expect(await screen.findByTestId("panel-today")).toBeInTheDocument();
      expect(collapsed.container.querySelector("aside")).toHaveClass("w-16");
      collapsed.unmount();

      // 900–1099 放不下 256：宽度压成图标轨，切换按钮不进 DOM（改由顶栏开整栏 Sheet）。
      setViewport(1024);
      useUiStore.setState({ railOpen: true });
      const forced = renderAt("/");
      expect(await screen.findByTestId("panel-today")).toBeInTheDocument();
      expect(forced.container.querySelector("aside")).toHaveClass("w-16");
      expect(screen.queryByRole("button", { name: "展开左栏" })).toBeNull();
      expect(screen.queryByRole("button", { name: "折叠左栏" })).toBeNull();
      // 强制的只是渲染层：存档里的偏好不许被覆写，视口放宽还要能展开。
      expect(useUiStore.getState().railOpen).toBe(true);
      forced.unmount();
    } finally {
      useUiStore.setState({ railOpen: true });
      restore();
    }
  });

  it("rail 收起态也能进设置页", async () => {
    const restore = useWideViewport();
    try {
      useUiStore.setState({ railOpen: false });
      const view = renderAt("/");
      // 先等壳的数据落地，收尾时就不会留下测试之外的异步 setState。
      expect(await screen.findByTestId("panel-today")).toBeInTheDocument();
      expect(screen.getByRole("link", { name: "设置" })).toHaveAttribute(
        "href",
        "/settings",
      );
      view.unmount();
    } finally {
      useUiStore.setState({ railOpen: true });
      restore();
    }
  });

  it("dock 打开时对话首屏不白屏：ChatView 的 Tooltip 在根部 provider 里", async () => {
    const restore = useWideViewport();
    useDockStore.setState({ open: true, conversationByWs: {} });
    const view = renderAt("/");
    try {
      // 缺 TooltipProvider 时 Radix 渲染期抛错、整棵树被卸载，消息区不会出现。
      await waitFor(() =>
        expect(
          document.querySelector("[data-slot='message-scroller']"),
        ).not.toBeNull(),
      );
    } finally {
      // Tooltip 内部有延迟计时器：挂着等它到点会以 act() 警告收尾。
      view.unmount();
      useDockStore.setState({ open: false });
      restore();
    }
  });
});

describe("dock 三形态", () => {
  afterEach(() => {
    setViewport(1024);
  });

  it("形态跟着 shell 宽走：1440 右列、1280 底部条、820 覆盖 Sheet", async () => {
    const restore = useWideViewport();
    try {
      // shell = 1440 − 256 = 1184：右列的下限。
      setViewport(1440);
      useUiStore.setState({ railOpen: true });
      useDockStore.setState({ open: true, conversationByWs: {} });
      const wide = renderAt("/");
      await waitFor(() =>
        expect(wide.container.querySelector("#peco-dock")).not.toBeNull(),
      );
      expect(wide.container.querySelector("#peco-dock-bottom")).toBeNull();
      expect(wide.container.querySelector("#peco-dock-sheet")).toBeNull();
      wide.unmount();

      // shell = 1280 − 256 = 1024：退出右列，收起成 40px 的底部条。
      setViewport(1280);
      useDockStore.setState({ open: false, conversationByWs: {} });
      const bottom = renderAt("/");
      const bar = await waitFor(() => {
        const node = bottom.container.querySelector("#peco-dock-bottom");
        expect(node).not.toBeNull();
        return node as HTMLElement;
      });
      expect(bottom.container.querySelector("#peco-dock")).toBeNull();
      expect(bar).toHaveClass("h-10");
      expect(
        bottom.container.querySelector("[data-slot='message-scroller']"),
      ).toBeNull();

      // 收起态点整条展开：对话才挂上，头部给 ⌄ 收起。
      fireEvent.click(within(bar).getByRole("button"));
      expect(useDockStore.getState().open).toBe(true);
      await waitFor(() =>
        expect(
          bottom.container.querySelector("button[aria-label='收起 Peco 对话']"),
        ).not.toBeNull(),
      );
      expect(
        bottom.container.querySelector("[data-slot='message-scroller']"),
      ).not.toBeNull();
      bottom.unmount();

      // 视口 <900：覆盖式 Sheet，语义与 radix 的弹层一致。
      setViewport(820);
      useDockStore.setState({ open: true, conversationByWs: {} });
      const small = renderAt("/");
      const sheet = await waitFor(() => {
        const node = small.container.querySelector("#peco-dock-sheet");
        expect(node).not.toBeNull();
        return node as HTMLElement;
      });
      expect(sheet).toHaveAttribute("role", "dialog");
      expect(sheet).toHaveAttribute("aria-modal", "true");
      expect(sheet).toHaveAttribute("aria-label", "Peco 对话");
      expect(small.container.querySelector("#peco-dock")).toBeNull();
      expect(small.container.querySelector("#peco-dock-bottom")).toBeNull();
      small.unmount();
    } finally {
      useDockStore.setState({ open: false });
      useUiStore.setState({ railOpen: true });
      restore();
    }
  });

  it("换形态不重挂子树：aside 与消息区是同一批 DOM 节点", async () => {
    const restore = useWideViewport();
    try {
      setViewport(1440);
      useUiStore.setState({ railOpen: true });
      useDockStore.setState({ open: true, conversationByWs: {} });
      const view = renderAt("/");
      const before = await waitFor(() => {
        const dock = view.container.querySelector("#peco-dock");
        const scroller = dock?.querySelector("[data-slot='message-scroller']");
        expect(scroller).toBeTruthy();
        return { dock: dock as HTMLElement, scroller: scroller as HTMLElement };
      });

      setViewport(1280);
      const after = await waitFor(() => {
        const dock = view.container.querySelector("#peco-dock-bottom");
        expect(dock).not.toBeNull();
        return dock as HTMLElement;
      });

      expect(after).toBe(before.dock);
      expect(after.querySelector("[data-slot='message-scroller']")).toBe(
        before.scroller,
      );
      view.unmount();
    } finally {
      useDockStore.setState({ open: false });
      useUiStore.setState({ railOpen: true });
      restore();
    }
  });
});
