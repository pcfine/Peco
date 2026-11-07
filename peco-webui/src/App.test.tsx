import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
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
 * jsdom 的 `matchMedia` 一律返回 false（见 test/setup.ts），壳的宽屏分支要显式打开，
 * 否则 rail 只会出现在 Sheet 里、dock 也不按右列渲染。返回的函数用于复原。
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
  it("rail 宽度只由折叠态决定：展开 256、收起 64", async () => {
    const restore = useWideViewport();
    try {
      useUiStore.setState({ railOpen: true });
      const expanded = renderAt("/");
      expect(await screen.findByTestId("panel-today")).toBeInTheDocument();
      expect(expanded.container.querySelector("aside")).toHaveClass("w-64");
      expanded.unmount();

      useUiStore.setState({ railOpen: false });
      const collapsed = renderAt("/");
      expect(await screen.findByTestId("panel-today")).toBeInTheDocument();
      expect(collapsed.container.querySelector("aside")).toHaveClass("w-16");
      collapsed.unmount();
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
