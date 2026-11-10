import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TooltipProvider } from "@/components/ui/tooltip";
import { PecoChatPage } from "@/pages/PecoChatPage";
import type { Space, Task } from "@/types/daemon";
import type { Message } from "@/types/chat";

// Peco 主区：聊天列 + 右栏四模式。取数全换成固定值 ——
// 这一层测的是页面装配与右栏语义，真接口由 daemon 的集成测试覆盖。

const mocks = vi.hoisted(() => ({
  ensure: vi.fn(async (_wsId: string) => ({
    conversation_id: "conv_peco",
    created: false,
  })),
  createSpace: vi.fn(async (..._args: unknown[]) => ({
    ws_id: "ws_peco",
    work_dir: "/tmp/peco/work",
    team_id: "peco",
    status: "open",
    reserved: true,
  })),
  /** 同一个页面两种空间状态：未打开那条路要走完整的空态与打开入口。 */
  spaceStatus: { value: "open" as "open" | "closed" },
  /** 空间名来自目录：真机上 Peco 空间的 label 就是小写目录名。 */
  spaceLabel: { value: "Peco" },
}));

const pecoSpace = (): Space => ({
  ws_id: "ws_peco",
  work_dir: "/tmp/peco/work",
  team_id: "peco",
  label: mocks.spaceLabel.value,
  exit_policy: "inherit",
  effective_exit_policy: "keep",
  status: mocks.spaceStatus.value,
  closed_at:
    mocks.spaceStatus.value === "closed" ? "2026-11-05 12:00:00" : null,
  last_opened_at: "2026-11-05 10:00:00",
  reserved: true,
  inflight_dispatch: 0,
  inflight_foreign: 0,
  marker: null,
});

vi.mock("@/api/peco", () => ({
  getPeco: async () => ({
    ws_id: "ws_peco",
    work_dir: "/tmp/peco/work",
    team_id: "peco",
    reserved: true,
    from_marker: true,
  }),
}));

vi.mock("@/api/spaces", () => ({
  listSpaces: async () => [pecoSpace()],
  getSpace: async () => pecoSpace(),
  ensureSpaceConversation: (wsId: string) => mocks.ensure(wsId),
  spaceTasks: async (): Promise<Task[]> => [],
  spaceLiveTasks: async () => [],
  cancelSpaceTask: async () => ({ cancelled: true }),
  spaceFiles: async () => ({ path: "", entries: [], truncated: false }),
  // 右栏的「记忆」模式在挂载时就会取清单；这里给空列表，本文件测的是面板装配。
  spaceMemoryDocuments: async () => ({ total: 0, items: [] }),
  spaceArtifacts: async () => ({ items: [], total: 0 }),
  closeSpace: async () => ({ stopped: true }),
  createSpace: (...args: unknown[]) => mocks.createSpace(...args),
  deleteSpace: async () => ({ purged: true }),
}));

vi.mock("@/api/teams", () => ({
  listTeams: async () => [],
  createTeam: vi.fn(),
  updateTeam: vi.fn(),
  deleteTeam: vi.fn(),
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
    throw new Error("unused");
  },
}));

vi.mock("@/api/tasks", () => ({
  listTasks: async () => [],
}));

vi.mock("@/api/conversations", () => ({
  getMessages: async (): Promise<Message[]> => [],
  conversationStreamUrl: (agentId: string, convId: string) =>
    `/api/chat/${agentId}/conversations/${convId}/stream`,
  uploadConversationImage: async () => ({ id: "img", url: "/img" }),
}));

function PathProbe() {
  const { pathname, search } = useLocation();
  return (
    <span data-testid="path">
      {pathname}
      {search}
    </span>
  );
}

function renderPeco(path = "/peco") {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <TooltipProvider>
        <MemoryRouter initialEntries={[path]}>
          <Routes>
            <Route path="/peco" element={<PecoChatPage />} />
          </Routes>
          <PathProbe />
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

/** 聊天列（右栏收起时是唯一的一块）。 */
function chatColumnOf(): HTMLElement {
  return screen.getByTestId("peco-chat-column");
}

const DEFAULT_VIEWPORT = 1024;

afterEach(() => {
  setViewport(DEFAULT_VIEWPORT);
});

beforeEach(() => {
  localStorage.clear();
  mocks.spaceStatus.value = "open";
  mocks.ensure.mockClear();
  mocks.createSpace.mockClear();
});

describe("Peco 主区", () => {
  it("/peco 渲染聊天面，右栏默认总览（四块面板纵向堆叠）", async () => {
    renderPeco();

    const shell = await screen.findByTestId("space-chat-shell");
    expect(shell).toHaveAttribute("data-ws-id", "ws_peco");
    expect(shell).toHaveAttribute("data-visible", "true");
    // 唯一的 @master 会话：按 Peco 空间的 ws 取。
    expect(await within(shell).findByRole("textbox")).toBeInTheDocument();
    expect(mocks.ensure).toHaveBeenCalledWith("ws_peco");

    const panel = screen.getByTestId("peco-panel");
    expect(panel).toHaveAttribute("data-mode", "overview");
    for (const id of ["today", "spaces", "teams", "results"]) {
      expect(screen.getByTestId(`panel-${id}`)).toBeInTheDocument();
    }
    // 看板的拖拽栅格不进新壳。
    expect(document.querySelector(".board-grid")).toBeNull();
  });

  it("两块骨架：左头有唯一的 h1 与两枚图标按钮，右头是四个 tab", async () => {
    renderPeco();

    const chatColumn = await screen.findByTestId("peco-chat-column");
    // 这一页就两块，两块各有一行头部，没有第三块。
    const headers = Array.from(document.querySelectorAll("header"));
    expect(headers).toHaveLength(2);
    for (const header of headers) {
      expect(
        chatColumn.contains(header) ||
          screen.getByTestId("peco-panel").contains(header),
      ).toBe(true);
    }
    // 全页仍只有一个 h1。
    expect(document.querySelectorAll("h1")).toHaveLength(1);

    const leftHeader = chatColumn.querySelector("header") as HTMLElement;
    expect(leftHeader.className).toContain("h-12");
    expect(leftHeader.className).toContain("border-b");
    expect(
      within(leftHeader).getByRole("heading", { level: 1, name: "Peco" }),
    ).toBeInTheDocument();
    // 两枚纯图标按钮：名字只能靠 aria-label，零文本节点。
    for (const label of ["清除对话", "收起对话"]) {
      const button = within(leftHeader).getByRole("button", { name: label });
      expect(button.textContent).toBe("");
    }
    // 团队配置的唯一入口是标题旁的 ·peco 链接。
    expect(
      within(leftHeader).getByRole("link", { name: /peco/ }),
    ).toHaveAttribute("href", "/teams/peco");
    expect(
      within(leftHeader).queryByRole("button", { name: "能力配置" }),
    ).toBeNull();

    const rightHeader = screen
      .getByTestId("peco-panel")
      .querySelector("header") as HTMLElement;
    expect(rightHeader.className).toContain("h-12");
    const tablist = within(rightHeader).getByRole("tablist", { name: "右栏" });
    expect(within(tablist).getAllByRole("tab")).toHaveLength(4);
  });

  it("收起对话：左块藏起来但聊天壳不卸载，右栏接手整宽，回来的路在右头", async () => {
    renderPeco();
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("button", { name: "收起对话" }));

    // 视觉消失 ≠ 卸载：壳还在文档里，在途那一轮不会被打断。
    expect(screen.getByTestId("space-chat-shell")).toHaveAttribute(
      "data-visible",
      "true",
    );
    const panel = screen.getByTestId("peco-panel");
    expect(panel).toHaveClass("flex-1");
    // 独占时内容吃满整行，与空间页同一口径。
    expect((panel.lastElementChild as HTMLElement).className).not.toContain(
      "max-w-mid",
    );
    expect(screen.getByRole("button", { name: "展开对话" })).toHaveFocus();
    expect(
      JSON.parse(
        localStorage.getItem("peco-webui:space-panel:ws_peco") ?? "{}",
      ),
    ).toEqual({ mode: "overview", collapsed: false, chatCollapsed: true });

    fireEvent.click(screen.getByRole("button", { name: "展开对话" }));
    expect(
      screen.getByRole("button", { name: "收起对话" }),
    ).toBeInTheDocument();
  });

  it("右栏模式按空间持久化，key 是 peco-webui:space-panel:<ws_id>", async () => {
    renderPeco();
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("tab", { name: "记忆" }));

    expect(screen.getByTestId("peco-panel")).toHaveAttribute(
      "data-mode",
      "memory",
    );
    expect(
      JSON.parse(
        localStorage.getItem("peco-webui:space-panel:ws_peco") ?? "{}",
      ),
    ).toEqual({ mode: "memory", collapsed: false, chatCollapsed: false });

    fireEvent.click(screen.getByRole("button", { name: "收起右栏" }));
    expect(screen.queryByTestId("peco-panel")).toBeNull();
    // 收起后的回来路径在左头：右栏藏着时才出现一枚「展开右栏」。
    expect(
      screen.getByRole("button", { name: "展开右栏" }),
    ).toBeInTheDocument();
    expect(
      JSON.parse(
        localStorage.getItem("peco-webui:space-panel:ws_peco") ?? "{}",
      ),
    ).toEqual({ mode: "memory", collapsed: true, chatCollapsed: false });
  });

  it("URL 的 ?panel= 压过存档，切模式只改 panel", async () => {
    localStorage.setItem(
      "peco-webui:space-panel:ws_peco",
      JSON.stringify({ mode: "files", collapsed: false }),
    );
    renderPeco("/peco?panel=memory");
    await screen.findByTestId("space-chat-shell");

    // 存档说文件，URL 说记忆：URL 赢，右栏直接是记忆。
    expect(screen.getByTestId("peco-panel")).toHaveAttribute(
      "data-mode",
      "memory",
    );

    fireEvent.click(screen.getByRole("tab", { name: /^任务/ }));

    expect(screen.getByTestId("peco-panel")).toHaveAttribute(
      "data-mode",
      "tasks",
    );
    expect(screen.getByTestId("path")).toHaveTextContent("/peco?panel=tasks");
  });

  it("Peco 空间未打开：聊天列与文件面板都给空态与打开入口", async () => {
    mocks.spaceStatus.value = "closed";
    renderPeco();

    const chatColumn = await screen.findByTestId("peco-chat-column");
    expect(
      await within(chatColumn).findByText(/Peco 空间还没打开/),
    ).toBeInTheDocument();
    // 没有实例就不发 ensure：先去打开，而不是在 composer 里撞一次 409。
    expect(screen.queryByTestId("space-chat-shell")).toBeNull();
    expect(mocks.ensure).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("tab", { name: "文件" }));
    expect(screen.getByTestId("peco-panel")).toHaveAttribute(
      "data-mode",
      "files",
    );
    const openButtons = screen.getAllByRole("button", {
      name: "打开 Peco 空间",
    });
    expect(openButtons.length).toBeGreaterThan(0);

    fireEvent.click(openButtons[0]);
    await waitFor(() =>
      expect(mocks.createSpace).toHaveBeenCalledWith({
        work_dir: "/tmp/peco/work",
        team_id: "peco",
      }),
    );
  });

  it("窄屏（work < 824）两块互斥且聊天优先：展开右栏换位，展开对话换回来", async () => {
    setViewport(820);
    renderPeco();
    await screen.findByTestId("space-chat-shell");

    // 放不下两块：默认只留聊天，右栏连同它的 tab 栏一起不在。
    expect(screen.queryByTestId("peco-panel")).toBeNull();
    expect(screen.queryByRole("tablist", { name: "右栏" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "展开右栏" }));

    expect(screen.getByTestId("peco-panel")).toHaveClass("flex-1");
    // 让位是 display:none，不是卸载：在途那一轮不随换位中断。
    expect(chatColumnOf()).toHaveClass("hidden");
    expect(screen.getByTestId("space-chat-shell")).toHaveAttribute(
      "data-visible",
      "true",
    );
    expect(screen.getByRole("button", { name: "展开对话" })).toHaveFocus();

    fireEvent.click(screen.getByRole("button", { name: "展开对话" }));

    expect(screen.queryByTestId("peco-panel")).toBeNull();
    expect(chatColumnOf()).not.toHaveClass("hidden");
  });

  it("左头的 h1 用 title 挂着工作目录，可访问名仍是空间名", async () => {
    // 空间名取自目录：这里就是小写的那种，页面得显示品牌名。
    mocks.spaceLabel.value = "peco";
    renderPeco();

    const heading = await screen.findByRole("heading", {
      level: 1,
      name: "Peco",
    });
    expect(heading).toHaveAttribute("title", "/tmp/peco/work");
    // title 只是悬停补充：读屏念到的名称不能变成路径。
    expect(heading).toHaveAccessibleName("Peco");
  });

  it("聊天正文的内距与空间页同构：聊天壳宽 = 列宽 − 32", async () => {
    renderPeco();

    const shell = await screen.findByTestId("space-chat-shell");
    const body = shell.parentElement as HTMLElement;
    // 左右各 16 才是六档实测里的「列 − 32」；退回 p-3 会让整条口径漂 8px。
    expect(body).toHaveClass("px-4");
    expect(body).toHaveClass("pb-3");
    expect(body).not.toHaveClass("p-3");
  });

  it("右栏内容声明 panel 容器，头部与内容同一内距（px-3）", async () => {
    renderPeco();
    await screen.findByTestId("space-chat-shell");

    const panel = screen.getByTestId("peco-panel");
    const header = panel.querySelector("header") as HTMLElement;
    expect(header.className).toContain("px-3");

    // 缺了 `@container/panel`，文件/台账的容器查询会一路量到 main 的整宽。
    const content = panel.lastElementChild as HTMLElement;
    expect(content.className).toContain("@container/panel");
  });
});
