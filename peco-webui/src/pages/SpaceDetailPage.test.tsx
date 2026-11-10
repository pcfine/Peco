import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
  renderHook,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import {
  MemoryRouter,
  Route,
  Routes,
  useLocation,
  useNavigate,
} from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TooltipProvider } from "@/components/ui/tooltip";
import { SpaceDetailPage } from "@/pages/SpaceDetailPage";
import { useSpacePanelState } from "@/components/spaces/useSpacePanelState";
import { useOpenSpacesStore } from "@/stores/openSpacesStore";
import { toast } from "sonner";
import { ApiError } from "@/lib/api";
import type { LiveTaskView, Space, Task } from "@/types/daemon";
import type { Message } from "@/types/chat";

// 聊天页的装配面：两块并排、双向折叠、右栏模式与持久化、冲突提示。
// 取数全换成固定值 —— 真接口由 daemon 的集成测试覆盖。

const mocks = vi.hoisted(() => ({
  ensure: vi.fn(async (wsId: string) => ({
    conversation_id: `conv_${wsId}`,
    created: false,
  })),
  tasks: vi.fn(async (_wsId: string): Promise<Task[]> => []),
  /** 默认走「空间未打开」这条路：提示与角标于是回落到台账，与真实漂移一致。 */
  live: vi.fn(async (_wsId: string): Promise<LiveTaskView[]> => {
    throw new ApiError("空间还没打开", {
      status: 409,
      code: "space_not_open",
    });
  }),
  getMessages: vi.fn(async (..._args: unknown[]): Promise<Message[]> => []),
  clearSpaceConversation: vi.fn(async (..._args: unknown[]) => ({
    success: true,
  })),
}));

/** 「打开」走的是幂等的 POST /api/spaces：目录与团队原样提交。 */
const createSpace = vi.fn(
  async (_body: { work_dir: string; team_id: string }) => ({
    ws_id: "ws_closed",
  }),
);

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn() },
}));

vi.mock("@/api/spaces", () => {
  const spaces: Record<string, Space> = {
    ws_a: {
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
    },
    ws_b: {
      ws_id: "ws_b",
      work_dir: "/tmp/spaces/ws_b/work",
      team_id: "research",
      label: "空间 B",
      exit_policy: "inherit",
      effective_exit_policy: "keep",
      status: "open",
      closed_at: null,
      last_opened_at: "2026-11-05 11:00:00",
      reserved: false,
      inflight_dispatch: 0,
      inflight_foreign: 0,
      marker: null,
    },
    ws_closed: {
      ws_id: "ws_closed",
      work_dir: "/tmp/spaces/ws_closed/work",
      team_id: "research",
      label: "空间 C",
      exit_policy: "inherit",
      effective_exit_policy: "keep",
      status: "closed",
      closed_at: "2026-11-05 12:00:00",
      last_opened_at: "2026-11-05 09:00:00",
      reserved: false,
      inflight_dispatch: 0,
      inflight_foreign: 0,
      marker: null,
    },
  };
  return {
    getSpace: async (wsId: string) => spaces[wsId] ?? spaces.ws_a,
    ensureSpaceConversation: (wsId: string) => mocks.ensure(wsId),
    clearSpaceConversation: (wsId: string, agentName?: string) =>
      mocks.clearSpaceConversation(wsId, agentName),
    createSpace: (body: { work_dir: string; team_id: string }) =>
      createSpace(body),
    spaceTasks: (wsId: string) => mocks.tasks(wsId),
    spaceLiveTasks: (wsId: string) => mocks.live(wsId),
    cancelSpaceTask: async () => ({ cancelled: true }),
    spaceFiles: async () => ({ path: "", entries: [], truncated: false }),
    // 右栏的「记忆」模式在挂载时就会取清单；这里给空列表，本文件测的是面板装配。
    spaceMemoryDocuments: async () => ({ total: 0, items: [] }),
    spaceArtifacts: async () => ({ items: [], total: 0 }),
    closeSpace: async () => ({ stopped: true }),
  };
});

vi.mock("@/api/conversations", () => ({
  getMessages: (...args: unknown[]) => mocks.getMessages(...args),
  conversationStreamUrl: (agentId: string, convId: string, _message: string) =>
    `/api/chat/${agentId}/conversations/${convId}/stream`,
  uploadConversationImage: async () => ({ id: "img", url: "/img" }),
}));

function taskOf(overrides: Partial<Task>): Task {
  return {
    id: "t_1",
    task_id: "t_1",
    target_ws_id: "ws_a",
    target_work_dir: "/tmp/spaces/ws_a/work",
    target_team_id: "research",
    origin_ws_id: "ws_peco",
    origin_kind: "dispatch",
    agent_name: "coder",
    task_state: "working",
    wake_state: "delivered",
    wake_skip_reason: null,
    created_at: "2026-11-05 10:00:00",
    terminal_at: null,
    result_digest: null,
    ...overrides,
  };
}

function liveTask(overrides: Partial<LiveTaskView> = {}): LiveTaskView {
  return {
    id: "t_live",
    kind: "echo",
    state: "working",
    state_detail: null,
    progress: { phase: null, current: null, total: null },
    output: { tail: [], start_offset: 0, end_offset: 0 },
    outcome: null,
    artifacts: [],
    created_at: "2026-11-05T10:00:00Z",
    updated_at: "2026-11-05T10:00:10Z",
    ...overrides,
  };
}

function NavProbe() {
  const navigate = useNavigate();
  return (
    <button type="button" onClick={() => navigate("/spaces/ws_b")}>
      切到 B
    </button>
  );
}

/** 地址栏在 MemoryRouter 里不可见：查询串靠一个探针来断言。 */
function SearchProbe() {
  const { search } = useLocation();
  return <span data-testid="search">{search}</span>;
}

function renderDetail(
  path = "/spaces/ws_a",
  prime?: (client: QueryClient) => void,
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  prime?.(client);
  return render(
    <QueryClientProvider client={client}>
      <TooltipProvider>
        <MemoryRouter initialEntries={[path]}>
          <main id="main">
            <Routes>
              <Route path="/spaces/:wsId" element={<SpaceDetailPage />} />
            </Routes>
          </main>
          <NavProbe />
          <SearchProbe />
        </MemoryRouter>
      </TooltipProvider>
    </QueryClientProvider>,
  );
}

/** 壳的分档读 `window.innerWidth`（`useViewportWidth`）：挂载前改，挂载后补一次 resize。 */
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

function searchOf(): URLSearchParams {
  return new URLSearchParams(screen.getByTestId("search").textContent ?? "");
}

/**
 * 双开的几何是「两块同权」：都 `flex-1 basis-0 min-w-0`，谁也不再固定宽度。
 * jsdom 量不了像素，这里钉住产生均分的类是哪些；宽度由 e2e 兜。
 */
function expectEvenSplit() {
  const chat = screen.getByTestId("space-chat-column");
  const panel = screen.getByTestId("space-panel");
  for (const block of [chat, panel]) {
    expect(block).toHaveClass("flex-1");
    expect(block).toHaveClass("basis-0");
    expect(block).toHaveClass("min-w-0");
  }
  expect(panel).not.toHaveClass("w-80");
  expect(panel).not.toHaveClass("shrink-0");
}

/** 页面根就是那一条 flex 行：左对话列 + 右栏，两个都可能在。 */
function pageRoot(): HTMLElement {
  const root = document.querySelector("main > div");
  if (!root) throw new Error("页面根不在 main 下");
  return root as HTMLElement;
}

function storedPanel(wsId = "ws_a"): unknown {
  return JSON.parse(
    localStorage.getItem(`peco-webui:space-panel:${wsId}`) ?? "{}",
  );
}

/** 「已打开」有两行：A→B 的保活测试要两个壳同时挂着。 */
function openBothSpaces() {
  useOpenSpacesStore.setState({
    openIds: ["ws_a", "ws_b"],
    activeWsId: "ws_a",
    seeded: true,
  });
}

/**
 * 把聊天流握在测试手里：`fetch` 回一条永不结束的 SSE 流，只有测试能收尾。
 *
 * 这么一来「这一轮在跑」是真的从 ChatView 流过 `onStreamingChange` 报上来的，
 * 不是在 store 或 props 上摆一个恒真的值。
 */
function stubChatStream() {
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const body = new ReadableStream<Uint8Array>({
    start(next) {
      controller = next;
    },
  });
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(body, { status: 200 })),
  );
  return {
    /** 关流 = 这一轮收尾：ChatView 的读循环走完 finally，把 streaming 置回 false。 */
    end: () => controller.close(),
  };
}

/** 在聊天壳的输入框里回车发一条：这是唯一能把 ChatView 推进流式态的用户动作。 */
function sendInShell(shell: HTMLElement) {
  const textbox = within(shell).getByRole("textbox");
  fireEvent.change(textbox, { target: { value: "跑一下" } });
  fireEvent.keyDown(textbox, { key: "Enter" });
}

afterEach(() => {
  setViewport(1024);
  vi.unstubAllGlobals();
});

beforeEach(() => {
  localStorage.clear();
  mocks.ensure.mockClear();
  mocks.tasks.mockClear();
  mocks.getMessages.mockClear();
  mocks.clearSpaceConversation.mockClear();
  mocks.clearSpaceConversation.mockResolvedValue({ success: true });
  vi.mocked(toast.error).mockClear();
  createSpace.mockClear();
  mocks.ensure.mockImplementation(async (wsId: string) => ({
    conversation_id: `conv_${wsId}`,
    created: false,
  }));
  mocks.tasks.mockResolvedValue([]);
  mocks.live.mockReset();
  mocks.live.mockImplementation(async () => {
    throw new ApiError("空间还没打开", {
      status: 409,
      code: "space_not_open",
    });
  });
  mocks.getMessages.mockResolvedValue([]);
  useOpenSpacesStore.setState({
    openIds: ["ws_a"],
    activeWsId: "ws_a",
    seeded: true,
  });
});

describe("空间聊天页", () => {
  it("两块并排：左对话列 + 右栏，全页只有一个 h1", async () => {
    renderDetail();

    const shell = await screen.findByTestId("space-chat-shell");
    expect(shell).toHaveAttribute("data-ws-id", "ws_a");
    expect(shell).toHaveAttribute("data-visible", "true");
    // ChatView 真的挂上了：有 composer。
    expect(await within(shell).findByRole("textbox")).toBeInTheDocument();

    const root = pageRoot();
    expect(root.children).toHaveLength(2);
    expect(root.children[0].tagName).toBe("DIV");
    expect(root.children[1]).toBe(screen.getByTestId("space-panel"));

    // 页头不再有第三块：唯一的 h1 在两块之一的 header 里。
    expect(document.querySelectorAll("h1")).toHaveLength(1);
    expect(document.querySelectorAll("main > header")).toHaveLength(0);
    expect(
      within(root.children[0] as HTMLElement).getByRole("heading", {
        level: 1,
        name: "空间 A",
      }),
    ).toBeInTheDocument();
    // 团队名是小字链接，只带 id，不加「团队」二字。
    expect(screen.getByRole("link", { name: "·research" })).toHaveAttribute(
      "href",
      "/teams/research",
    );

    expect(screen.getByTestId("space-panel")).toHaveAttribute(
      "data-mode",
      "files",
    );
    // 默认双开：两块均分整宽。
    expectEvenSplit();
    // 唯一的 @master 会话：历史按空间的 ws 取。
    expect(mocks.ensure).toHaveBeenCalledWith("ws_a");
  });

  it("历史按空间的 ws 取回，并渲染成聊天消息", async () => {
    mocks.getMessages.mockResolvedValue([
      {
        id: "m_1",
        role: "user",
        content: "合同里的违约条款看下",
        created_at: "2026-11-05 10:00:00",
      },
    ]);
    renderDetail();

    expect(await screen.findByText("合同里的违约条款看下")).toBeInTheDocument();
    expect(mocks.getMessages).toHaveBeenCalledWith("@master", "conv_ws_a", {
      limit: 100,
      wsId: "ws_a",
    });
  });

  it("A→B 换路由参数不卸载聊天壳，只有当前空间可见", async () => {
    openBothSpaces();
    renderDetail("/spaces/ws_a");

    const before = await screen.findAllByTestId("space-chat-shell");
    const shellA = before.find(
      (node) => node.getAttribute("data-ws-id") === "ws_a",
    ) as HTMLElement;
    expect(await within(shellA).findByRole("textbox")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "切到 B" }));

    const shells = await screen.findAllByTestId("space-chat-shell");
    expect(shells).toHaveLength(2);
    const byWs = new Map(
      shells.map((shell) => [shell.getAttribute("data-ws-id"), shell]),
    );
    const afterA = byWs.get("ws_a");
    const afterB = byWs.get("ws_b");
    expect(afterA).toBeDefined();
    expect(afterB).toBeDefined();

    expect(afterA).toHaveAttribute("data-visible", "false");
    expect(afterA).toHaveClass("hidden");
    expect(afterB).toHaveAttribute("data-visible", "true");
    expect(afterB).not.toHaveClass("hidden");

    // 两个壳都还在文档里：切走的那一个没有被卸载，流也不会被打断。
    expect(within(afterA as HTMLElement).getByRole("textbox")).toBeDisabled();
    expect(
      within(afterB as HTMLElement).getByRole("textbox"),
    ).not.toBeDisabled();
    expect(
      screen.getByRole("heading", { level: 1, name: "空间 B" }),
    ).toBeInTheDocument();
  });

  it("收起右栏：右栏整块消失，对话列接着用；点「展开右栏」回来", async () => {
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("button", { name: "收起右栏" }));

    expect(screen.queryByTestId("space-panel")).toBeNull();
    expect(screen.queryByLabelText("空间右栏")).toBeNull();
    // 右栏没了，对话列就成了页面根里唯一的一块。
    expect(pageRoot().children).toHaveLength(1);
    expect(screen.getByTestId("space-chat-shell")).toBeInTheDocument();
    expect(storedPanel()).toEqual({
      mode: "files",
      collapsed: true,
      chatCollapsed: false,
    });

    // 「展开右栏」只在右栏藏着的时候出现，且长在对话列头部。
    const chatColumn = pageRoot().children[0] as HTMLElement;
    fireEvent.click(
      within(chatColumn).getByRole("button", { name: "展开右栏" }),
    );

    expectEvenSplit();
    expect(screen.queryByRole("button", { name: "展开右栏" })).toBeNull();
    expect(storedPanel()).toEqual({
      mode: "files",
      collapsed: false,
      chatCollapsed: false,
    });
  });

  it("折叠入口贴着头两端：收起对话在左，右栏开关在右，收起右栏后不挪位", async () => {
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    const chatHeader = () =>
      screen
        .getByTestId("space-chat-column")
        .querySelector("header") as HTMLElement;

    // 双开时右栏开关在右栏头的最右，聊天头右槽空着但宽度照留。
    expect(chatHeader().children).toHaveLength(3);
    expect(
      within(chatHeader().children[0] as HTMLElement).getByRole("button", {
        name: "收起对话",
      }),
    ).toBeInTheDocument();
    expect(
      within(chatHeader().children[2] as HTMLElement).queryByRole("button"),
    ).toBeNull();
    const panelHeader = screen
      .getByTestId("space-panel")
      .querySelector("header") as HTMLElement;
    expect(
      within(panelHeader).getAllByRole("button").at(-1),
    ).toHaveAccessibleName("收起右栏");

    // 收起右栏：右端原地换成「展开右栏」，左端仍是「收起对话」。
    fireEvent.click(screen.getByRole("button", { name: "收起右栏" }));
    expect(
      within(chatHeader().children[0] as HTMLElement).getByRole("button", {
        name: "收起对话",
      }),
    ).toBeInTheDocument();
    expect(
      within(chatHeader().children[2] as HTMLElement).getByRole("button", {
        name: "展开右栏",
      }),
    ).toBeInTheDocument();
  });

  it("收起对话：右栏接手整行，聊天壳留着不卸载；点「展开对话」回到双开", async () => {
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("button", { name: "收起对话" }));

    // 视觉消失 ≠ 卸载：壳还在文档里，只是它所在的列 display:none ——
    // 卸载会让 ChatView abort 掉在途的那一轮。
    expect(screen.getAllByTestId("space-chat-shell")).toHaveLength(1);
    expect(screen.getByTestId("space-chat-column")).toHaveClass("hidden");
    expect(screen.getByTestId("space-chat-column")).toHaveAttribute(
      "data-collapsed",
      "true",
    );
    // 折叠不改「谁是当前空间」：visible 仍为 true。
    expect(screen.getByTestId("space-chat-shell")).toHaveAttribute(
      "data-visible",
      "true",
    );
    // 右栏成了唯一露面的一块：没有兄弟争宽度，它自然吃满整宽。
    const panel = screen.getByTestId("space-panel");
    expect(pageRoot().children[1]).toBe(panel);
    expect(panel).toHaveClass("flex-1");
    expect(panel).toHaveClass("basis-0");
    expect(panel).not.toHaveClass("w-80");
    // 独占时 tab 内容跟着吃满整行，不再按 880 居中留白。
    expect((panel.lastElementChild as HTMLElement).className).not.toContain(
      "max-w-mid",
    );
    expect(storedPanel()).toEqual({
      mode: "files",
      collapsed: false,
      chatCollapsed: true,
    });

    fireEvent.click(screen.getByRole("button", { name: "展开对话" }));

    expect(await screen.findByTestId("space-chat-shell")).toBeInTheDocument();
    expect(screen.getByTestId("space-chat-column")).toHaveClass("flex");
    expect(screen.getByTestId("space-chat-column")).not.toHaveClass("hidden");
    expectEvenSplit();
    expect(
      (screen.getByTestId("space-panel").lastElementChild as HTMLElement)
        .className,
    ).not.toContain("max-w-mid");
  });

  it("窄屏（work < 824）两块互斥且聊天优先：展开右栏换位，展开对话换回来", async () => {
    setViewport(820);
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    // 放不下两块：默认只留聊天，右栏的 tab 栏也一起不在。
    expect(screen.queryByTestId("space-panel")).toBeNull();
    expect(screen.queryByRole("tablist", { name: "右栏" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "展开右栏" }));

    const panel = screen.getByTestId("space-panel");
    expect(panel).toHaveClass("flex-1");
    expect(screen.getByTestId("space-chat-column")).toHaveClass("hidden");
    expect(within(panel).getByRole("tab", { name: /^任务/ })).toHaveAttribute(
      "aria-selected",
      "false",
    );
    expect(storedPanel()).toEqual({
      mode: "files",
      collapsed: false,
      chatCollapsed: true,
    });

    fireEvent.click(screen.getByRole("button", { name: "展开对话" }));

    expect(await screen.findByTestId("space-chat-shell")).toBeInTheDocument();
    expect(screen.getByTestId("space-chat-column")).not.toHaveClass("hidden");
    expect(screen.queryByTestId("space-panel")).toBeNull();
    expect(storedPanel()).toEqual({
      mode: "files",
      collapsed: true,
      chatCollapsed: false,
    });
  });

  it("双折叠不可达：连点两个折叠按钮，始终有一块可见", async () => {
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("button", { name: "收起对话" }));
    expect(screen.getByTestId("space-panel")).toBeInTheDocument();

    // 右栏此刻是唯一的一块，收起它必须把对话放回来。
    fireEvent.click(screen.getByRole("button", { name: "收起右栏" }));
    expect(screen.queryByTestId("space-panel")).toBeNull();
    expect(await screen.findByTestId("space-chat-shell")).toBeInTheDocument();

    // 再收一次对话：右栏接位，两块不会一起消失。
    fireEvent.click(screen.getByRole("button", { name: "收起对话" }));
    expect(screen.getByTestId("space-panel")).toBeInTheDocument();
    expect(screen.getByTestId("space-chat-column")).toHaveClass("hidden");
  });

  it("刷新后按存档恢复模式与折叠态；旧存档（没有 chatCollapsed）不崩", async () => {
    // 旧版本只写 mode 与 collapsed：缺的 chatCollapsed 当 false。
    localStorage.setItem(
      "peco-webui:space-panel:ws_a",
      JSON.stringify({ mode: "memory", collapsed: true }),
    );
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    expect(screen.queryByTestId("space-panel")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "展开右栏" }));
    expect(screen.getByTestId("space-panel")).toHaveAttribute(
      "data-mode",
      "memory",
    );
    expect(await screen.findByText("本空间还没有记忆。")).toBeInTheDocument();
  });

  it("存档说聊天收起时，右栏按独占恢复", async () => {
    localStorage.setItem(
      "peco-webui:space-panel:ws_a",
      JSON.stringify({ mode: "tasks", collapsed: false, chatCollapsed: true }),
    );
    renderDetail();

    const panel = await screen.findByTestId("space-panel");
    expect(panel).toHaveClass("flex-1");
    expect(within(panel).getByRole("tab", { name: /^任务/ })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByTestId("space-chat-column")).toHaveClass("hidden");
  });

  it("两个折叠按钮只有图标：aria-label 与 Tooltip 同文", async () => {
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    const collapseChat = screen.getByRole("button", { name: "收起对话" });
    const collapsePanel = screen.getByRole("button", { name: "收起右栏" });
    expect(collapseChat.textContent).toBe("");
    expect(collapsePanel.textContent).toBe("");

    fireEvent.focus(collapseChat);
    expect(await screen.findByRole("tooltip")).toHaveTextContent("收起对话");

    fireEvent.blur(collapseChat);
    fireEvent.focus(collapsePanel);
    expect(await screen.findByRole("tooltip")).toHaveTextContent("收起右栏");
  });

  it("任务 tab 先取实时列表，并标注实时", async () => {
    mocks.live.mockResolvedValue([liveTask()]);
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("tab", { name: /^任务/ }));

    const note = await screen.findByTestId("task-fidelity");
    expect(note).toHaveTextContent("实时");
    expect(mocks.live).toHaveBeenCalledWith("ws_a");
    // 台账只用来补 prompt / agent：没有对应行时照样给出「未记录」，不留白。
    expect(screen.getByText("未记录")).toBeInTheDocument();
  });

  it("没有在飞任务时「任务」tab 不带角标", async () => {
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    // 台账与实时都空：tab 的文案就是这两个字，不写「任务 0」。
    expect(
      await screen.findByRole("tab", { name: "任务" }),
    ).toBeInTheDocument();
  });

  it("在飞任务数进「任务」tab 的角标", async () => {
    mocks.live.mockResolvedValue([liveTask()]);
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    expect(
      await screen.findByRole("tab", { name: "任务 1" }),
    ).toBeInTheDocument();
  });

  it("ensure 失败时给错误与重试，聊天壳不被卸载", async () => {
    mocks.ensure.mockRejectedValueOnce(new Error("空间 ws_a 没有可用会话"));
    renderDetail();

    expect(
      await screen.findByText("空间 ws_a 没有可用会话"),
    ).toBeInTheDocument();
    expect(screen.getByTestId("space-chat-shell")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "重试" }));

    const shell = screen.getByTestId("space-chat-shell");
    expect(await within(shell).findByRole("textbox")).toBeInTheDocument();
    expect(screen.queryByText("空间 ws_a 没有可用会话")).toBeNull();
  });

  it("空间未打开时先说清楚，并就地给出打开入口", async () => {
    renderDetail("/spaces/ws_closed");

    expect(await screen.findByText(/空间还没打开/)).toBeInTheDocument();
    const shells = screen.getAllByTestId("space-chat-shell");
    expect(shells.map((shell) => shell.getAttribute("data-ws-id"))).toContain(
      "ws_closed",
    );

    // 页头没有「打开」了：入口只能在这条提示旁边。
    fireEvent.click(screen.getByRole("button", { name: "打开" }));
    await waitFor(() =>
      expect(createSpace).toHaveBeenCalledWith({
        work_dir: "/tmp/spaces/ws_closed/work",
        team_id: "research",
      }),
    );
  });

  it("有任务在飞时 composer 上方给非阻塞提示，输入框仍可用", async () => {
    mocks.tasks.mockResolvedValue([taskOf({ task_state: "working" })]);
    renderDetail();

    const hint = await screen.findByText(/个任务在这个空间运行/);
    expect(hint).toBeInTheDocument();
    expect(screen.getByRole("textbox")).not.toBeDisabled();
    // 提示不阻塞：没有确认弹窗，也没有写锁。
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("冲突提示的在飞数取实时快照，不跟台账走", async () => {
    // 台账已经落后（还是空），实时快照里有两个非终态任务：提示要按实时数说话。
    mocks.live.mockResolvedValue([
      liveTask({ id: "t_a" }),
      liveTask({ id: "t_b", state: "input_required" }),
      liveTask({ id: "t_c", state: "completed" }),
    ]);
    renderDetail();

    const hint = await screen.findByText(/个任务在这个空间运行/);
    expect(hint).toHaveTextContent("2");
    expect(screen.getByRole("textbox")).not.toBeDisabled();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("没有在飞任务时不留冲突提示", async () => {
    mocks.tasks.mockResolvedValue([
      taskOf({ id: "t_done", task_id: "t_done", task_state: "completed" }),
    ]);
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    await screen.findByRole("textbox");
    expect(screen.queryByText(/个任务在这个空间运行/)).toBeNull();
  });

  it("URL 的 ?panel= 压过存档：存档是文件，URL 说任务就渲染任务", async () => {
    localStorage.setItem(
      "peco-webui:space-panel:ws_a",
      JSON.stringify({ mode: "files", collapsed: false }),
    );
    renderDetail("/spaces/ws_a?panel=tasks");
    await screen.findByTestId("space-chat-shell");

    expect(screen.getByTestId("space-panel")).toHaveAttribute(
      "data-mode",
      "tasks",
    );
  });

  it("右栏头部与内容同轴，且表头行落在 panel 容器里（不是 main 的 work）", async () => {
    mocks.tasks.mockResolvedValue([taskOf({ task_state: "working" })]);
    renderDetail("/spaces/ws_a?panel=tasks");
    await screen.findByTestId("space-chat-shell");

    const panel = screen.getByTestId("space-panel");
    const panelHeader = panel.querySelector("header") as HTMLElement;
    // 「摘要」在两个地方出现（保真度标注也是一枚「摘要」徽标），这里认表头那一个。
    const headerRow = (await screen.findByText("状态"))
      .parentElement as HTMLElement;

    // 右栏内容行自带 px-3：头部跟着同一条竖线，差 4px 就是两套内距。
    expect(panelHeader.className).toContain("px-3");
    expect(headerRow.className).toContain("px-3");

    // 容器查询挂在这块内容上：缺了它 616 宽的右栏量到的是 main 的 1232，列全开。
    const container = document.querySelector(
      '[class*="@container/panel"]',
    ) as HTMLElement | null;
    expect(container).not.toBeNull();
    expect(container?.contains(headerRow)).toBe(true);
    expect(panel.contains(container as HTMLElement)).toBe(true);
  });

  it("URL 里的模式不属于这个变体时忽略，回落存档", async () => {
    // 「总览」只属于 Peco 页：空间页收到的 overview 是无效值，不该造出一个死面板。
    localStorage.setItem(
      "peco-webui:space-panel:ws_a",
      JSON.stringify({ mode: "memory", collapsed: false }),
    );
    renderDetail("/spaces/ws_a?panel=overview");
    await screen.findByTestId("space-chat-shell");

    expect(screen.getByTestId("space-panel")).toHaveAttribute(
      "data-mode",
      "memory",
    );
  });

  it("切模式把 panel 写进 URL，其它查询参数原样保留", async () => {
    renderDetail("/spaces/ws_a?face=chat&panel=files");
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("tab", { name: /^任务/ }));

    const search = searchOf();
    expect(search.get("panel")).toBe("tasks");
    expect(search.get("face")).toBe("chat");
    expect(storedPanel()).toEqual({
      mode: "tasks",
      collapsed: false,
      chatCollapsed: false,
    });
  });

  it("认的 URL 参数不会被一次折叠清掉", async () => {
    renderDetail("/spaces/ws_a?panel=memory");
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("button", { name: "收起右栏" }));
    expect(screen.queryByTestId("space-panel")).toBeNull();
    // 折叠是偏好，不进 URL；模式还在 URL 里，刷新后仍回到记忆。
    expect(searchOf().get("panel")).toBe("memory");
  });

  it("折叠不动 visible：收起对话后当前空间的壳仍挂在页面上", async () => {
    openBothSpaces();
    renderDetail("/spaces/ws_a");
    await screen.findAllByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("button", { name: "收起对话" }));

    // 两个壳都还在，且「谁是当前空间」没变：折叠只改显隐。
    const shells = screen.getAllByTestId("space-chat-shell");
    expect(shells.map((shell) => shell.getAttribute("data-ws-id"))).toEqual([
      "ws_a",
      "ws_b",
    ]);
    expect(shells.map((shell) => shell.getAttribute("data-visible"))).toEqual([
      "true",
      "false",
    ]);
  });

  it("收起一块后焦点落到对面刚出现的「展开」上", async () => {
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("button", { name: "收起对话" }));
    expect(screen.getByRole("button", { name: "展开对话" })).toHaveFocus();

    fireEvent.click(screen.getByRole("button", { name: "展开对话" }));
    fireEvent.click(screen.getByRole("button", { name: "收起右栏" }));
    expect(screen.getByRole("button", { name: "展开右栏" })).toHaveFocus();
  });

  it("左头内容行与聊天正文同轴：mx-auto + max-w-chat，两侧内距同为 px-4", async () => {
    renderDetail();
    const shell = await screen.findByTestId("space-chat-shell");

    // 双开时两块均分，x 轴不由 max-w-chat 封顶：头部与正文差 4px 就是两条竖线。
    const header = document.querySelector("header") as HTMLElement;
    expect(header.className).toContain("px-4");
    expect(
      (shell.parentElement as HTMLElement).className.split(/\s+/),
    ).toContain("px-4");
    // 三条：左边界槽、居中内容行、右边界槽。内容行以列中心为轴。
    expect(header.children).toHaveLength(3);
    const row = header.querySelector(".max-w-chat") as HTMLElement;
    expect(row.className).toContain("mx-auto");
    expect(row.className).toContain("max-w-chat");
    // h1 在这一行里：标题与消息落在同一条竖线上。
    expect(
      within(row).getByRole("heading", { level: 1, name: "空间 A" }),
    ).toBeInTheDocument();

    // 正文那一列用同一对上限类：jsdom 量不了布局，量的是同轴的前提。
    const chatView = shell.querySelector<HTMLElement>(
      '[data-slot="message-scroller"]',
    )?.parentElement as HTMLElement;
    expect(chatView.className).toContain("mx-auto");
    expect(chatView.className).toContain("max-w-chat");
  });

  describe("清除对话", () => {
    it("确认之前不发请求；确认后清空恰好一次，消息区清空且会话 id 不变", async () => {
      let cleared = false;
      mocks.clearSpaceConversation.mockImplementation(async () => {
        cleared = true;
        return { success: true };
      });
      mocks.getMessages.mockImplementation(async () =>
        cleared
          ? []
          : [
              {
                id: "m_1",
                role: "user",
                content: "清除前的一条消息",
                created_at: "2026-11-05 10:00:00",
              },
            ],
      );

      renderDetail();
      expect(await screen.findByText("清除前的一条消息")).toBeInTheDocument();
      // 历史只取过一次：下面的第二次必须是清空触发的重取。
      expect(mocks.getMessages).toHaveBeenCalledTimes(1);

      fireEvent.click(screen.getByRole("button", { name: "清除对话" }));

      const dialog = await screen.findByRole("dialog");
      expect(dialog).toHaveTextContent("清除这段对话？");
      expect(dialog).toHaveTextContent("会话与已产出的成果保留");
      // 弹窗只是征询：请求要等确认。
      expect(mocks.clearSpaceConversation).not.toHaveBeenCalled();

      fireEvent.click(within(dialog).getByRole("button", { name: "清除对话" }));

      await waitFor(() =>
        expect(mocks.clearSpaceConversation).toHaveBeenCalledTimes(1),
      );
      // 清空的是空间会话的消息，不是删会话：路径带空间，agent 由后端约定。
      expect(mocks.clearSpaceConversation).toHaveBeenCalledWith(
        "ws_a",
        "@master",
      );

      await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
      await waitFor(() =>
        expect(screen.queryByText("清除前的一条消息")).toBeNull(),
      );

      // 会话 id 没变：ensure 不再跑第二轮，历史仍按同一条会话重取。
      expect(mocks.ensure).toHaveBeenCalledTimes(1);
      expect(mocks.getMessages).toHaveBeenCalledTimes(2);
      expect(mocks.getMessages).toHaveBeenLastCalledWith(
        "@master",
        "conv_ws_a",
        { limit: 100, wsId: "ws_a" },
      );
    });

    it("失败时 toast 说原因，弹窗不关", async () => {
      mocks.clearSpaceConversation.mockRejectedValueOnce(
        new ApiError("空间还没打开", { status: 409, code: "space_not_open" }),
      );

      renderDetail();
      await screen.findByTestId("space-chat-shell");
      fireEvent.click(screen.getByRole("button", { name: "清除对话" }));
      const dialog = await screen.findByRole("dialog");
      fireEvent.click(within(dialog).getByRole("button", { name: "清除对话" }));

      await waitFor(() => expect(toast.error).toHaveBeenCalled());
      expect(toast.error).toHaveBeenCalledWith("空间还没打开");
      expect(screen.getByRole("dialog")).toBeInTheDocument();
    });

    it("对话进行中：按钮按不动、点不开弹窗，Tooltip 说明原因", async () => {
      const stream = stubChatStream();
      renderDetail();
      const shell = await screen.findByTestId("space-chat-shell");

      sendInShell(shell);

      const clear = screen.getByRole("button", { name: "清除对话" });
      await waitFor(() =>
        expect(clear).toHaveAttribute("aria-disabled", "true"),
      );
      fireEvent.click(clear);
      expect(screen.queryByRole("dialog")).toBeNull();

      fireEvent.focus(clear);
      expect(await screen.findByRole("tooltip")).toHaveTextContent(
        "对话进行中，结束后再清除",
      );
      fireEvent.blur(clear);

      // 这一轮收尾：同样的按钮现在能开弹窗。
      act(() => stream.end());
      await waitFor(() => expect(clear).not.toHaveAttribute("aria-disabled"));
      fireEvent.click(clear);
      expect(await screen.findByRole("dialog")).toBeInTheDocument();
    });

    it("在飞的是别的空间：当前空间的按钮照常可用", async () => {
      openBothSpaces();
      const stream = stubChatStream();
      // 预置 B 的详情：缓存里没有它时，切过去会先落一帧骨架，把 A 的壳连同
      // 流一起卸载 —— 那是另一条已知限制，本用例要看的是「按空间判断」。
      renderDetail("/spaces/ws_a", (client) =>
        client.setQueryData(["space", "ws_b"], {
          ws_id: "ws_b",
          work_dir: "/tmp/spaces/ws_b/work",
          team_id: "research",
          label: "空间 B",
          exit_policy: "inherit",
          effective_exit_policy: "keep",
          status: "open",
          closed_at: null,
          last_opened_at: "2026-11-05 11:00:00",
          reserved: false,
          inflight_dispatch: 0,
          inflight_foreign: 0,
          marker: null,
        } satisfies Space),
      );

      const shellA = (await screen.findAllByTestId("space-chat-shell")).find(
        (node) => node.getAttribute("data-ws-id") === "ws_a",
      ) as HTMLElement;
      sendInShell(shellA);

      await waitFor(() =>
        expect(
          screen.getByRole("button", { name: "清除对话" }),
        ).toHaveAttribute("aria-disabled", "true"),
      );

      // 切到 B：A 的那一轮还在跑（壳没卸载、流没断），但按不动的是 A，不是 B。
      fireEvent.click(screen.getByRole("button", { name: "切到 B" }));
      await screen.findByRole("heading", { level: 1, name: "空间 B" });
      const clearB = screen.getByRole("button", { name: "清除对话" });
      await waitFor(() => expect(clearB).not.toHaveAttribute("aria-disabled"));
      fireEvent.click(clearB);
      expect(await screen.findByRole("dialog")).toBeInTheDocument();

      act(() => stream.end());
    });

    it("会话 id 未就绪时按钮按不动，Tooltip 仍说明它是谁", async () => {
      mocks.ensure.mockImplementation(() => new Promise(() => {}));
      renderDetail();

      const clear = await screen.findByRole("button", { name: "清除对话" });
      expect(clear).toHaveAttribute("aria-disabled", "true");

      fireEvent.click(clear);
      expect(screen.queryByRole("dialog")).toBeNull();

      // `aria-disabled` 而非 `disabled`：悬浮/聚焦仍能问到原因。
      fireEvent.focus(clear);
      expect(await screen.findByRole("tooltip")).toHaveTextContent("清除对话");
    });
  });
});

// 面板藏起来时它的 tab 栏也一起消失，页面上没有「收起态点 tab」这条路；
// 契约仍在 selectMode 里：点模式就一定让右栏可见。
describe("右栏折叠状态机", () => {
  function stateHook(exclusive = false) {
    return renderHook(() => useSpacePanelState("ws_a", "space", { exclusive }));
  }

  it("收起右栏后点模式会重新展开，并落进存档", () => {
    const { result } = stateHook();

    act(() => result.current.toggleCollapsed());
    expect(result.current.collapsed).toBe(true);

    act(() => result.current.selectMode("tasks"));
    expect(result.current.collapsed).toBe(false);
    expect(result.current.mode).toBe("tasks");
    expect(storedPanel()).toEqual({
      mode: "tasks",
      collapsed: false,
      chatCollapsed: false,
    });
  });

  it("窄屏下展开右栏会收起对话，收起右栏会放回对话", () => {
    const { result } = stateHook(true);

    // 窄屏默认只留聊天。
    expect(result.current.collapsed).toBe(true);
    expect(result.current.chatCollapsed).toBe(false);

    act(() => result.current.toggleCollapsed());
    expect(result.current.chatCollapsed).toBe(true);
    expect(result.current.collapsed).toBe(false);

    act(() => result.current.toggleCollapsed());
    expect(result.current.chatCollapsed).toBe(false);
    expect(result.current.collapsed).toBe(true);

    // 展开对话同样只留一块。
    act(() => result.current.toggleChat());
    act(() => result.current.toggleChat());
    expect(result.current.chatCollapsed).toBe(false);
    expect(result.current.collapsed).toBe(true);
  });

  it("存档同时说两块都收起时仍然只藏一块", () => {
    localStorage.setItem(
      "peco-webui:space-panel:ws_a",
      JSON.stringify({ mode: "files", collapsed: true, chatCollapsed: true }),
    );
    const { result } = stateHook();

    expect(result.current.chatCollapsed).toBe(true);
    expect(result.current.collapsed).toBe(false);
  });

  it("坏掉的存档回默认态，不抛", () => {
    localStorage.setItem("peco-webui:space-panel:ws_a", "{不是 JSON");
    const { result } = stateHook();

    expect(result.current.mode).toBe("files");
    expect(result.current.collapsed).toBe(false);
    expect(result.current.chatCollapsed).toBe(false);
  });
});
