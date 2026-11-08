import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
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
import { useOpenSpacesStore } from "@/stores/openSpacesStore";
import { ApiError } from "@/lib/api";
import type { LiveTaskView, Space, Task } from "@/types/daemon";
import type { Message } from "@/types/chat";

// 聊天页的装配面：唯一会话、聊天壳保活、右栏模式与持久化、冲突提示。
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

/** `#main` 属于壳层，这里补一个同 id 的容器承接抽屉的 `inert`。 */
function renderDetail(path = "/spaces/ws_a") {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
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

/** 「已打开」有两行：A→B 的保活测试要两个壳同时挂着。 */
function openBothSpaces() {
  useOpenSpacesStore.setState({
    openIds: ["ws_a", "ws_b"],
    activeWsId: "ws_a",
    seeded: true,
  });
}

afterEach(() => {
  setViewport(1024);
});

beforeEach(() => {
  localStorage.clear();
  mocks.ensure.mockClear();
  mocks.tasks.mockClear();
  mocks.getMessages.mockClear();
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
  it("页头是空间名，主体是唯一对话壳，右栏默认文件模式", async () => {
    renderDetail();

    expect(await screen.findByText("空间 A")).toBeInTheDocument();

    const shell = await screen.findByTestId("space-chat-shell");
    expect(shell).toHaveAttribute("data-ws-id", "ws_a");
    expect(shell).toHaveAttribute("data-visible", "true");
    // ChatView 真的挂上了：有 composer。
    expect(await within(shell).findByRole("textbox")).toBeInTheDocument();

    expect(screen.getByTestId("space-panel")).toHaveAttribute(
      "data-mode",
      "files",
    );
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
    expect(screen.getByText("空间 B")).toBeInTheDocument();
  });

  it("右栏模式按空间持久化，收起后点模式先展开再切换", async () => {
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("button", { name: "收起右栏" }));
    expect(screen.queryByTestId("space-panel")).toBeNull();
    expect(
      JSON.parse(localStorage.getItem("peco-webui:space-panel:ws_a") ?? "{}"),
    ).toEqual({ mode: "files", collapsed: true });

    // 收起态点「任务」：一次点击就展开并切到任务。
    fireEvent.click(screen.getByRole("button", { name: "任务" }));
    expect(screen.getByTestId("space-panel")).toHaveAttribute(
      "data-mode",
      "tasks",
    );
    expect(
      JSON.parse(localStorage.getItem("peco-webui:space-panel:ws_a") ?? "{}"),
    ).toEqual({ mode: "tasks", collapsed: false });

    fireEvent.click(screen.getByRole("button", { name: "收起右栏" }));
    fireEvent.click(screen.getByRole("button", { name: "记忆" }));
    expect(screen.getByTestId("space-panel")).toHaveAttribute(
      "data-mode",
      "memory",
    );
    expect(await screen.findByText("本空间还没有记忆。")).toBeInTheDocument();
  });

  it("任务模式先取实时列表，并标注实时", async () => {
    mocks.live.mockResolvedValue([liveTask()]);
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("button", { name: /^任务/ }));

    const note = await screen.findByTestId("task-fidelity");
    expect(note).toHaveTextContent("实时");
    expect(mocks.live).toHaveBeenCalledWith("ws_a");
    // 台账只用来补 prompt / agent：没有对应行时照样给出「未记录」，不留白。
    expect(screen.getByText("未记录")).toBeInTheDocument();
  });

  it("重挂页面时按存档恢复右栏的模式与收起态", async () => {
    localStorage.setItem(
      "peco-webui:space-panel:ws_a",
      JSON.stringify({ mode: "memory", collapsed: false }),
    );
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    expect(screen.getByTestId("space-panel")).toHaveAttribute(
      "data-mode",
      "memory",
    );
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

  it("空间未打开时先说清楚，聊天壳照常挂着", async () => {
    renderDetail("/spaces/ws_closed");

    expect(await screen.findByText(/空间还没打开/)).toBeInTheDocument();
    const shells = screen.getAllByTestId("space-chat-shell");
    expect(shells.map((shell) => shell.getAttribute("data-ws-id"))).toContain(
      "ws_closed",
    );
    expect(screen.getByRole("button", { name: "打开" })).toBeEnabled();
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

    fireEvent.click(screen.getByRole("button", { name: "任务" }));

    const search = searchOf();
    expect(search.get("panel")).toBe("tasks");
    expect(search.get("face")).toBe("chat");
    expect(
      JSON.parse(localStorage.getItem("peco-webui:space-panel:ws_a") ?? "{}"),
    ).toEqual({ mode: "tasks", collapsed: false });
  });

  it("认的 URL 参数不会被一次折叠清掉", async () => {
    renderDetail("/spaces/ws_a?panel=memory");
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("button", { name: "收起右栏" }));
    expect(screen.queryByTestId("space-panel")).toBeNull();
    // 收起是偏好，不进 URL；模式还在 URL 里，刷新后仍回到记忆。
    expect(searchOf().get("panel")).toBe("memory");
  });

  it("窄屏（work < 824）右栏改抽屉：内联节点消失，抽屉里是同一个面板", async () => {
    setViewport(820);
    localStorage.setItem(
      "peco-webui:space-panel:ws_a",
      JSON.stringify({ mode: "memory", collapsed: true }),
    );
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    // 存档说「收起」，但抽屉的开关是瞬时态：不读它，也不会自己弹出来。
    expect(screen.queryByTestId("space-panel")).toBeNull();
    expect(screen.queryByLabelText("空间右栏")).toBeNull();

    const trigger = screen.getByRole("button", { name: "打开右栏" });
    fireEvent.click(trigger);

    const drawer = await screen.findByLabelText("空间右栏");
    expect(drawer).toHaveClass("w-80", "overscroll-contain");
    // 同一个 SpaceSidePanel：模式来自存档里的「记忆」。
    expect(
      await within(drawer).findByText("本空间还没有记忆。"),
    ).toBeInTheDocument();
    expect(document.getElementById("main")).toHaveAttribute("inert");
  });

  it("抽屉里切模式不碰 collapsed 存档，并把模式写进 URL", async () => {
    setViewport(820);
    localStorage.setItem(
      "peco-webui:space-panel:ws_a",
      JSON.stringify({ mode: "files", collapsed: true }),
    );
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("button", { name: "任务" }));

    const drawer = await screen.findByLabelText("空间右栏");
    expect(await within(drawer).findByText(/没有任务/)).toBeInTheDocument();
    expect(searchOf().get("panel")).toBe("tasks");
    // 抽屉不写 collapsed：存档里那一份留着，视口变宽后按它恢复。
    expect(
      JSON.parse(localStorage.getItem("peco-webui:space-panel:ws_a") ?? "{}"),
    ).toEqual({ mode: "tasks", collapsed: true });
  });

  it("Esc 关闭抽屉，焦点回到页面上的 chevron", async () => {
    setViewport(820);
    renderDetail();
    await screen.findByTestId("space-chat-shell");

    const trigger = screen.getByRole("button", { name: "打开右栏" });
    trigger.focus();
    fireEvent.click(trigger);
    await screen.findByLabelText("空间右栏");
    await waitFor(() => expect(document.activeElement).not.toBe(trigger));

    fireEvent.keyDown(document, { key: "Escape" });

    await waitFor(() => expect(screen.queryByLabelText("空间右栏")).toBeNull());
    await waitFor(() => expect(document.activeElement).toBe(trigger));
    expect(document.getElementById("main")).not.toHaveAttribute("inert");
  });
});
