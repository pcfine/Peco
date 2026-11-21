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
// 闸门在壳之前问一次会话状态：这一层测的是路由与面板，统一按「已登录」放行。
vi.mock("@/api/auth", () => ({
  getAuthStatus: async () => ({
    registered: true,
    authenticated: true,
    username: "pcfine",
  }),
  login: vi.fn(),
  register: vi.fn(),
  logout: vi.fn(),
  changePassword: vi.fn(),
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

const mocks = vi.hoisted(() => ({
  getMessages: vi.fn(async () => []),
  ensure: vi.fn(async (_wsId: string) => ({
    conversation_id: "conv-space",
    created: false,
  })),
}));

const pecoSpace = {
  ws_id: "ws_peco",
  work_dir: "/tmp/peco",
  team_id: "peco",
  label: "Peco",
  name: null,
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
  ensureSpaceConversation: mocks.ensure,
  closeSpace: async () => ({ stopped: true }),
  // 冒烟用的空间本来就是 open：自动打开不会发这一枪，但入口得在。
  openSpace: async () => pecoSpace,
  createSpace: async () => ({ ws_id: "ws_new" }),
  // 列表页挂着回收站页签：删掉的是「移入回收站」那条，不是永久销毁。
  listTrashSpaces: async () => [],
  trashSpace: async () => ({ trashed: true as const }),
}));

vi.mock("@/api/teams", () => ({
  listTeams: async () => [],
  createTeam: vi.fn(),
  updateTeam: vi.fn(),
  deleteTeam: vi.fn(),
  // `/teams/peco` 是 Peco 的配置页：团队详情照样要能取到。
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

// 定时任务页仍然只读一次列表：空清单就是它的空态，冒烟这一层不测增删改。
vi.mock("@/api/schedules", () => ({
  listSchedules: async () => [],
  listScheduleRuns: async () => [],
  previewCron: async () => ({ next: [] }),
  createSchedule: vi.fn(),
  updateSchedule: vi.fn(),
  deleteSchedule: vi.fn(),
  runSchedule: vi.fn(),
}));

vi.mock("@/api/conversations", () => ({
  getMessages: mocks.getMessages,
  conversationStreamUrl: (agentId: string, convId: string) =>
    `/api/chat/${agentId}/conversations/${convId}/stream`,
  conversationLiveStreamUrl: (agentId: string, convId: string) =>
    `/api/chat/${agentId}/conversations/${convId}/stream/live`,
  cancelConversationTurn: async () => "accepted" as const,
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

/** 顶栏带 `data-slot`：页面自己也有 `header`（分组标题、空间页头），不能靠标签挑。 */
function topBar(): HTMLElement {
  const header = document.querySelector<HTMLElement>('[data-slot="topbar"]');
  if (!header) throw new Error("没有渲染顶栏");
  return header;
}

/** 顶栏是「位置指示」的 12px `p`，不是 h1；h1 归内容区，每页恰好一个。 */
function expectOnePageH1() {
  expect(document.querySelectorAll("h1")).toHaveLength(1);
  expect(topBar().querySelector("h1")).toBeNull();
  expect(topBar().querySelector("p")).toHaveClass("text-caption");
}

function expectTopBarIndicator(text: string) {
  expect(within(topBar()).getByText(text)).toHaveClass("text-caption");
}

beforeEach(() => {
  localStorage.clear();
  mocks.getMessages.mockClear();
  mocks.ensure.mockClear();
  // dock 的对话壳挂载即订阅在途轮：本文件不测 attach，统一回「没有在途轮」。
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(null, { status: 204 })),
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
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
    expectTopBarIndicator("Peco 聊天");
    expectOnePageH1();
  });

  it("/spaces 是列表页，空态给一句事实", async () => {
    renderAt("/spaces");
    expect(
      await screen.findByRole("heading", { level: 1, name: "工作室" }),
    ).toBeInTheDocument();
    expect(await screen.findByText(/已打开/)).toBeInTheDocument();
    expectOnePageH1();
  });

  it("/results 空台账给一句事实", async () => {
    renderAt("/results");
    expect(await screen.findByText(/台账里没有记录/)).toBeInTheDocument();
  });

  it("/spaces/new 两块同时可见，缺目录时主按钮禁用", async () => {
    renderAt("/spaces/new");
    // 顶栏只是位置指示，页面标题才是 h1：「新建空间」在全页只该有一个标题节点。
    expect(
      await screen.findByRole("heading", { level: 1, name: "新建空间" }),
    ).toBeInTheDocument();
    expectOnePageH1();
    expect(screen.getByText("目录")).toBeInTheDocument();
    expect(screen.getByText("团队")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "创建并打开" })).toBeDisabled();
  });

  it("/schedules 空清单时给一句语义说明，不是占位页", async () => {
    renderAt("/schedules");
    expect(
      await screen.findByText(
        /定时任务按 cron 把一段话投进指定空间的 @master 会话/,
      ),
    ).toBeInTheDocument();
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

  // 每页的 h1 由内容区提供，顶栏不再参与大纲：这里是全站口径的复测。
  it.each([
    ["/peco", "Peco"],
    ["/spaces", "工作室"],
    ["/spaces/ws_peco", "Peco"],
    ["/spaces/ws_peco/status", "Peco"],
    ["/spaces/new", "新建空间"],
    ["/teams", "团队"],
    ["/teams/peco", "Peco"],
    ["/results", "成果"],
    ["/settings", "设置"],
    ["/schedules", "定时任务"],
  ])("%s 的内容区恰好 1 个 h1「%s」", async (path, title) => {
    renderAt(path);
    expect(
      await screen.findByRole("heading", { level: 1, name: title }),
    ).toBeInTheDocument();
    expectOnePageH1();
  });

  it("未注册的路径给 404 文案，不是营销页", async () => {
    renderAt("/workflows/demo");
    expect(await screen.findByText("页面不存在")).toBeInTheDocument();
  });

  it("顶栏只在 <1100 存在：宽屏没有那条标题栏，h1 仍只有内容区那一个", async () => {
    const restore = useWideViewport();
    try {
      setViewport(1440);
      const wide = renderAt("/spaces");
      expect(
        await screen.findByRole("heading", { level: 1, name: "工作室" }),
      ).toBeInTheDocument();
      expect(document.querySelector('[data-slot="topbar"]')).toBeNull();
      expect(document.querySelectorAll("h1")).toHaveLength(1);
      wide.unmount();

      setViewport(1024);
      const narrow = renderAt("/spaces");
      expect(
        await screen.findByRole("heading", { level: 1, name: "工作室" }),
      ).toBeInTheDocument();
      expect(topBar()).toBeInTheDocument();
      narrow.unmount();
    } finally {
      setViewport(1024);
      restore();
    }
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
      expect(expanded.container.querySelector("aside")).toHaveClass("w-52");
      expect(
        screen.getByRole("button", { name: "折叠左栏" }),
      ).toBeInTheDocument();
      expanded.unmount();

      useUiStore.setState({ railOpen: false });
      const collapsed = renderAt("/");
      expect(await screen.findByTestId("panel-today")).toBeInTheDocument();
      expect(collapsed.container.querySelector("aside")).toHaveClass("w-16");
      collapsed.unmount();

      // 900–1099 放不下 208：宽度压成图标轨，切换按钮不进 DOM（改由顶栏开整栏 Sheet）。
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

  it("rail 收起态不显示设置与主题，底栏只留折叠", async () => {
    const restore = useWideViewport();
    try {
      setViewport(1440);
      useUiStore.setState({ railOpen: false });
      const view = renderAt("/");
      // 先等壳的数据落地，收尾时就不会留下测试之外的异步 setState。
      expect(await screen.findByTestId("panel-today")).toBeInTheDocument();
      expect(screen.queryByRole("link", { name: "设置" })).toBeNull();
      expect(screen.queryByRole("button", { name: /切到/ })).toBeNull();
      expect(
        screen.getByRole("button", { name: "展开左栏" }),
      ).toBeInTheDocument();
      view.unmount();
    } finally {
      useUiStore.setState({ railOpen: true });
      restore();
    }
  });

  it("dock 打开时对话首屏不白屏：ChatView 的 Tooltip 在根部 provider 里", async () => {
    const restore = useWideViewport();
    useDockStore.setState({ open: true });
    // `#peco-dock` 只在非 Peco 页存在：`/` 会重定向到 `/peco`。
    const view = renderAt("/spaces");
    try {
      // 缺 TooltipProvider 时 Radix 渲染期抛错、整棵树被卸载，消息区不会出现。
      await waitFor(() =>
        expect(
          document.querySelector("[data-slot='message-scroller']"),
        ).not.toBeNull(),
      );
      // 抽屉接的是空间会话（ensure 出来的那条），不再自己建一条。
      await waitFor(() =>
        expect(mocks.getMessages).toHaveBeenCalledWith(
          "@master",
          "conv-space",
          expect.objectContaining({ wsId: "ws_peco" }),
        ),
      );
    } finally {
      // Tooltip 内部有延迟计时器：挂着等它到点会以 act() 警告收尾。
      view.unmount();
      useDockStore.setState({ open: false });
      restore();
    }
  });
});

describe("Peco 抽屉", () => {
  afterEach(() => {
    setViewport(1024);
  });

  it("在 /peco 时不渲染：没有 #peco-dock，开关也归 false", async () => {
    const restore = useWideViewport();
    try {
      setViewport(1440);
      useUiStore.setState({ railOpen: true });
      useDockStore.setState({ open: true });
      const view = renderAt("/peco");
      // Peco 页自己就是这条会话的入口：抽屉连壳都不该留。这里不断言 ensure 的
      // 次数 —— 页面壳自己也会调它，抽屉侧的「不建会话」由 DockShell.test.tsx 管。
      await waitFor(() => expect(useDockStore.getState().open).toBe(false));
      expect(view.container.querySelector("#peco-dock")).toBeNull();
      expect(screen.queryByRole("button", { name: "Peco 对话" })).toBeNull();
      view.unmount();
    } finally {
      useDockStore.setState({ open: false });
      useUiStore.setState({ railOpen: true });
      restore();
    }
  });

  it("在 Peco 空间页不渲染：那一页的主体就是同一条会话", async () => {
    const restore = useWideViewport();
    try {
      setViewport(1440);
      useUiStore.setState({ railOpen: true });
      useDockStore.setState({ open: true });
      const view = renderAt("/spaces/ws_peco");
      // 等页面自己的聊天壳落地，再看抽屉有没有跟着铺第二份历史。
      expect(await screen.findByTestId("space-chat-shell")).toBeInTheDocument();
      await waitFor(() => expect(useDockStore.getState().open).toBe(false));
      expect(view.container.querySelector("#peco-dock")).toBeNull();
      expect(screen.queryByRole("button", { name: "Peco 对话" })).toBeNull();
      view.unmount();
    } finally {
      useDockStore.setState({ open: false });
      useUiStore.setState({ railOpen: true });
      restore();
    }
  });

  it("关着：面板常驻但内容不渲染、键盘与点击都进不去，旧的两个 id 不存在", async () => {
    const restore = useWideViewport();
    try {
      setViewport(1440);
      useUiStore.setState({ railOpen: true });
      useDockStore.setState({ open: false });
      const view = renderAt("/spaces");
      const panel = await waitFor(() => {
        const node = view.container.querySelector("#peco-dock");
        expect(node).not.toBeNull();
        return node as HTMLElement;
      });
      // 从没打开过：不建对话、不挂消息区（页面自己的聊天壳不算）。
      expect(panel.querySelector("[data-slot='message-scroller']")).toBeNull();
      expect(panel).toHaveAttribute("inert");
      expect(panel).toHaveAttribute("aria-hidden", "true");
      expect(panel).toHaveClass("-translate-x-full");
      expect(panel).not.toHaveClass("translate-x-0");
      // 三形态的两个旧 id 不该再有落点。
      expect(view.container.querySelector("#peco-dock-bottom")).toBeNull();
      expect(view.container.querySelector("#peco-dock-sheet")).toBeNull();
      view.unmount();
    } finally {
      useDockStore.setState({ open: false });
      useUiStore.setState({ railOpen: true });
      restore();
    }
  });

  it("点左栏品牌按钮开抽屉：aria-expanded 翻面、贴 rail、宽 400、对话才挂上", async () => {
    const restore = useWideViewport();
    try {
      setViewport(1440);
      useUiStore.setState({ railOpen: true });
      useDockStore.setState({ open: false });
      const view = renderAt("/spaces");
      const panel = await waitFor(() => {
        const node = view.container.querySelector("#peco-dock");
        expect(node).not.toBeNull();
        return node as HTMLElement;
      });
      const trigger = screen.getByRole("button", { name: "Peco 对话" });
      expect(trigger).toHaveAttribute("aria-expanded", "false");

      fireEvent.click(trigger);

      await waitFor(() => expect(useDockStore.getState().open).toBe(true));
      expect(trigger).toHaveAttribute("aria-expanded", "true");
      // 抽屉的会话就是 Peco 空间那一条，不自己另建。
      await waitFor(() => expect(mocks.ensure).toHaveBeenCalledWith("ws_peco"));
      expect(panel).toHaveClass("translate-x-0");
      expect(panel).not.toHaveAttribute("inert");
      // 几何：抽屉起点 = rail 宽，宽度上限 400（1440 时 92vw 用不上）。
      expect(panel.style.left).toBe("208px");
      expect(panel.style.width).toBe("min(92vw, 400px)");
      await waitFor(() =>
        expect(
          panel.querySelector("[data-slot='message-scroller']"),
        ).not.toBeNull(),
      );
      expect(
        within(panel).getByRole("button", { name: "关闭 Peco 对话" }),
      ).toBeInTheDocument();

      // 再点同一枚按钮收回。
      fireEvent.click(trigger);
      await waitFor(() => expect(useDockStore.getState().open).toBe(false));
      expect(trigger).toHaveAttribute("aria-expanded", "false");
      expect(panel).toHaveClass("-translate-x-full");
      expect(panel).toHaveAttribute("inert");
      view.unmount();
    } finally {
      useDockStore.setState({ open: false });
      useUiStore.setState({ railOpen: true });
      restore();
    }
  });

  it("开合不重挂内容：关掉后消息区还是同一个 DOM 节点", async () => {
    const restore = useWideViewport();
    try {
      setViewport(1440);
      useUiStore.setState({ railOpen: true });
      useDockStore.setState({ open: true });
      const view = renderAt("/spaces");
      const before = await waitFor(() => {
        const node = view.container
          .querySelector("#peco-dock")
          ?.querySelector("[data-slot='message-scroller']");
        expect(node).not.toBeNull();
        return node as HTMLElement;
      });

      fireEvent.click(screen.getByRole("button", { name: "Peco 对话" }));

      await waitFor(() => expect(useDockStore.getState().open).toBe(false));
      expect(
        view.container
          .querySelector("#peco-dock")
          ?.querySelector("[data-slot='message-scroller']"),
      ).toBe(before);
      view.unmount();
    } finally {
      useDockStore.setState({ open: false });
      useUiStore.setState({ railOpen: true });
      restore();
    }
  });
});
