import { beforeEach, describe, expect, it, vi } from "vitest";
import {
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
import { PecoChatPage } from "@/pages/PecoChatPage";
import type { Space, Task } from "@/types/daemon";
import type { Message } from "@/types/chat";

// Peco 主区：聊天面 + 右栏四模式 + 3D 翻转。取数全换成固定值 ——
// 这一层测的是页面装配与翻面语义，真接口由 daemon 的集成测试覆盖。

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
}));

const pecoSpace = (): Space => ({
  ws_id: "ws_peco",
  work_dir: "/tmp/peco/work",
  team_id: "peco",
  label: "Peco",
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
  spaceFiles: async () => ({ path: "", entries: [], truncated: false }),
  spaceArtifacts: async () => ({ items: [], total: 0 }),
  closeSpace: async () => ({ stopped: true }),
  createSpace: (...args: unknown[]) => mocks.createSpace(...args),
  deleteSpace: async () => ({ purged: true }),
}));

vi.mock("@/api/teams", () => ({
  listTeams: async () => [],
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

/** 左栏点 Peco 行只改 URL：这一条路也要走一程翻转动画。 */
function FaceNav() {
  const navigate = useNavigate();
  return (
    <button type="button" onClick={() => navigate("/peco?face=config")}>
      模拟左栏
    </button>
  );
}

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
          <FaceNav />
        </MemoryRouter>
      </TooltipProvider>
    </QueryClientProvider>,
  );
}

/** 打开的舞台是翻转容器的唯一子元素：过渡类挂在它身上。 */
function stageOf(flip: HTMLElement): HTMLElement {
  return flip.firstElementChild as HTMLElement;
}

/** jsdom 的 matchMedia 一律 false；减弱动效分支要显式打开。 */
function useReducedMotion(): () => void {
  const original = window.matchMedia;
  window.matchMedia = ((query: string) => ({
    matches: query.includes("prefers-reduced-motion"),
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

  it("点「能力配置」翻到配置面：URL 带 face=config，没有绑定空间与装配为空间", async () => {
    renderPeco();
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("button", { name: "能力配置" }));

    expect(screen.getByTestId("path")).toHaveTextContent("/peco?face=config");
    const flip = screen.getByTestId("peco-flip");
    expect(flip).toHaveAttribute("data-face", "config");
    expect(flip).toHaveAttribute("data-animating", "true");
    // 配置面挡住了右栏，模式按钮不留在页头上做死控件。
    expect(screen.queryByRole("button", { name: "总览" })).toBeNull();

    const config = screen.getByTestId("peco-face-config");
    expect(
      await within(config).findByRole("tab", { name: "agents" }),
    ).toBeInTheDocument();
    expect(within(config).queryByRole("tab", { name: "绑定空间" })).toBeNull();
    expect(
      within(config).queryByRole("button", { name: "装配为空间" }),
    ).toBeNull();
  });

  it("翻转进行中锁掉重复点击，锁到期后能翻回来", async () => {
    renderPeco();
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("button", { name: "能力配置" }));
    // 半途再点会从中间反向，观感是抖动：这一程走完前不接受第二次点击。
    fireEvent.click(screen.getByRole("button", { name: "返回聊天" }));

    expect(screen.getByTestId("path")).toHaveTextContent("?face=config");
    expect(screen.getByTestId("peco-flip")).toHaveAttribute(
      "data-face",
      "config",
    );

    await waitFor(
      () =>
        expect(screen.getByTestId("peco-flip")).toHaveAttribute(
          "data-animating",
          "false",
        ),
      { timeout: 2000 },
    );

    fireEvent.click(screen.getByRole("button", { name: "返回聊天" }));
    expect(screen.getByTestId("path")).not.toHaveTextContent("face=config");
    expect(screen.getByTestId("peco-flip")).toHaveAttribute(
      "data-face",
      "chat",
    );
  });

  it("从别处改 URL 换面（左栏点 Peco 行）同样播一程翻转", async () => {
    renderPeco();
    await screen.findByTestId("space-chat-shell");
    expect(screen.getByTestId("peco-flip")).toHaveAttribute(
      "data-animating",
      "false",
    );

    fireEvent.click(screen.getByRole("button", { name: "模拟左栏" }));

    const flip = screen.getByTestId("peco-flip");
    expect(flip).toHaveAttribute("data-face", "config");
    expect(flip).toHaveAttribute("data-animating", "true");
    expect(stageOf(flip).className).toContain("transition-transform");
  });

  it("直达 ?face=config 不播入场动画：上屏即终态、无 isAnimating", async () => {
    renderPeco("/peco?face=config");

    const flip = await screen.findByTestId("peco-flip");
    expect(flip).toHaveAttribute("data-face", "config");
    expect(flip).toHaveAttribute("data-animating", "false");
    const stage = stageOf(flip);
    expect(stage.className).not.toContain("transition-transform");
    // 3D 的终态直接在首次渲染里，没有从 0 到 180 的那一程。
    expect(stage.className).toContain("rotateY(180deg)");
  });

  it("两个面常驻：隐藏面 inert + aria-hidden，焦点与读屏都进不去", async () => {
    renderPeco();

    const chatFace = await screen.findByTestId("peco-face-chat");
    const configFace = screen.getByTestId("peco-face-config");
    expect(chatFace).toHaveAttribute("aria-hidden", "false");
    expect(chatFace).not.toHaveAttribute("inert");
    expect(configFace).toHaveAttribute("aria-hidden", "true");
    expect(configFace).toHaveAttribute("inert");
    // byRole 默认忽略 aria-hidden 子树：配置面的 tab 不该被读屏念到。
    expect(screen.queryByRole("tab", { name: "agents" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "能力配置" }));

    expect(
      await screen.findByRole("tab", { name: "agents" }),
    ).toBeInTheDocument();
    expect(screen.getByTestId("peco-face-config")).not.toHaveAttribute("inert");
    expect(screen.getByTestId("peco-face-chat")).toHaveAttribute("inert");
    expect(screen.getByTestId("peco-face-chat")).toHaveAttribute(
      "aria-hidden",
      "true",
    );
  });

  it("减弱动效时走 120ms 淡入淡出，不转 3D", async () => {
    const restore = useReducedMotion();
    try {
      renderPeco();
      const flip = await screen.findByTestId("peco-flip");
      expect(flip).toHaveAttribute("data-motion", "fade");
      expect(stageOf(flip).className).not.toContain("rotateY");

      fireEvent.click(screen.getByRole("button", { name: "能力配置" }));

      const chatFace = screen.getByTestId("peco-face-chat");
      expect(chatFace.className).toContain("opacity-0");
      expect(chatFace.className).toContain("transition-opacity");
      expect(chatFace.className).toContain("peco-flip-fade");
      const configFace = screen.getByTestId("peco-face-config");
      expect(configFace.className).not.toContain("opacity-0");
    } finally {
      restore();
    }
  });

  it("右栏模式按空间持久化，key 是 peco-webui:space-panel:<ws_id>", async () => {
    renderPeco();
    await screen.findByTestId("space-chat-shell");

    fireEvent.click(screen.getByRole("button", { name: "记忆" }));

    expect(screen.getByTestId("peco-panel")).toHaveAttribute(
      "data-mode",
      "memory",
    );
    expect(
      JSON.parse(
        localStorage.getItem("peco-webui:space-panel:ws_peco") ?? "{}",
      ),
    ).toEqual({ mode: "memory", collapsed: false });

    fireEvent.click(screen.getByRole("button", { name: "收起右栏" }));
    expect(screen.queryByTestId("peco-panel")).toBeNull();
    expect(
      JSON.parse(
        localStorage.getItem("peco-webui:space-panel:ws_peco") ?? "{}",
      ),
    ).toEqual({ mode: "memory", collapsed: true });
  });

  it("Peco 空间未打开：聊天列与文件面板都给空态与打开入口", async () => {
    mocks.spaceStatus.value = "closed";
    renderPeco();

    const chatFace = await screen.findByTestId("peco-face-chat");
    expect(
      await within(chatFace).findByText(/Peco 空间还没打开/),
    ).toBeInTheDocument();
    // 没有实例就不发 ensure：先去打开，而不是在 composer 里撞一次 409。
    expect(screen.queryByTestId("space-chat-shell")).toBeNull();
    expect(mocks.ensure).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "文件" }));
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

  it("Peco 空间未打开时，配置面的资源 tab 也摆同一条空态", async () => {
    mocks.spaceStatus.value = "closed";
    renderPeco("/peco?face=config");

    const config = await screen.findByTestId("peco-face-config");
    expect(
      await within(config).findByText(/Peco 空间还没打开/),
    ).toBeInTheDocument();
    expect(
      within(config).getByRole("button", { name: "打开 Peco 空间" }),
    ).toBeInTheDocument();
    // 资源 tab 没有 X-Peco-Space，就不该发请求。
    expect(screen.queryByRole("tab", { name: "绑定空间" })).toBeNull();
  });
});
