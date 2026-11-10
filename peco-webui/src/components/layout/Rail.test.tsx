import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Rail } from "@/components/layout/Rail";
import { useDockStore } from "@/stores/dockStore";
import { useOpenSpacesStore } from "@/stores/openSpacesStore";
import { useSpaceStore } from "@/stores/spaceStore";
import type { Space } from "@/types/daemon";

const ensureSpaceConversation = vi.fn(async (_wsId: string) => ({
  conversation_id: "c",
  created: false,
}));
const closeSpace = vi.fn(async (_wsId: string, _force?: boolean) => ({
  stopped: true,
}));

function space(overrides: Partial<Space> & { ws_id: string }): Space {
  return {
    work_dir: `/tmp/${overrides.ws_id}/work`,
    team_id: "research",
    label: null,
    exit_policy: "inherit",
    effective_exit_policy: "inherit",
    status: "open",
    closed_at: null,
    last_opened_at: null,
    reserved: false,
    inflight_dispatch: 0,
    inflight_foreign: 0,
    marker: null,
    ...overrides,
  };
}

const spaces: Space[] = [
  space({ ws_id: "ws_peco", team_id: "peco", label: "Peco", reserved: true }),
  space({ ws_id: "ws_a", label: "空间 A" }),
  space({ ws_id: "ws_b", label: "空间 B" }),
];

vi.mock("@/api/peco", () => ({
  getPeco: async () => ({
    ws_id: "ws_peco",
    work_dir: "/tmp/ws_peco/work",
    team_id: "peco",
    reserved: true,
    from_marker: true,
  }),
}));

vi.mock("@/api/spaces", () => ({
  listSpaces: async () => spaces,
  ensureSpaceConversation: (wsId: string) => ensureSpaceConversation(wsId),
  closeSpace: (wsId: string, force?: boolean) => closeSpace(wsId, force),
}));

vi.mock("@/api/teams", () => ({
  listTeams: async () => [],
  createTeam: vi.fn(),
  updateTeam: vi.fn(),
  deleteTeam: vi.fn(),
}));
vi.mock("@/api/tasks", () => ({ listTasks: async () => [] }));

function PathProbe() {
  const { pathname, search } = useLocation();
  return (
    <span data-testid="path">
      {pathname}
      {search}
    </span>
  );
}

function renderRail(collapsed = false, path = "/peco") {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        <Rail collapsed={collapsed} />
        <PathProbe />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  localStorage.clear();
  ensureSpaceConversation.mockClear();
  closeSpace.mockClear();
  useSpaceStore.setState({ currentWsId: "ws_peco" });
  useDockStore.setState({ open: false });
  useOpenSpacesStore.setState({
    openIds: ["ws_a"],
    activeWsId: "ws_a",
    seeded: true,
  });
});

describe("左栏品牌行", () => {
  it("品牌按钮只 toggle 抽屉，不导航", async () => {
    renderRail(false, "/peco");

    const trigger = await screen.findByRole("button", { name: "Peco 对话" });
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    expect(trigger).toHaveAttribute("aria-controls", "peco-dock");

    fireEvent.click(trigger);
    expect(useDockStore.getState().open).toBe(true);
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    // 开关不是导航入口：点了还停在原页。
    expect(screen.getByTestId("path")).toHaveTextContent(/^\/peco$/);

    fireEvent.click(trigger);
    expect(useDockStore.getState().open).toBe(false);
    expect(trigger).toHaveAttribute("aria-expanded", "false");
  });
});

describe("左栏打开集", () => {
  it("只渲染打开集里的空间，不在集里的不出现在左栏", async () => {
    renderRail();

    expect(
      await screen.findByRole("button", { name: "空间 A（团队 research）" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /空间 B/ }),
    ).not.toBeInTheDocument();
  });

  it("「状态」链接指向该空间的状态页", async () => {
    renderRail();

    expect(
      await screen.findByRole("link", { name: "空间 A 状态" }),
    ).toHaveAttribute("href", "/spaces/ws_a/status");
  });

  it("主点击切激活态并进聊天页，不重发 ensure", async () => {
    renderRail();

    fireEvent.click(
      await screen.findByRole("button", { name: "空间 A（团队 research）" }),
    );

    expect(useOpenSpacesStore.getState().activeWsId).toBe("ws_a");
    expect(screen.getByTestId("path")).toHaveTextContent("/spaces/ws_a");
    expect(ensureSpaceConversation).not.toHaveBeenCalled();
  });

  it("× 只把空间移出打开集，不关闭空间、不发请求", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    renderRail();

    fireEvent.click(
      await screen.findByRole("button", { name: "从打开列表移除" }),
    );

    expect(useOpenSpacesStore.getState().openIds).toEqual([]);
    expect(closeSpace).not.toHaveBeenCalled();
    expect(ensureSpaceConversation).not.toHaveBeenCalled();
    expect(fetchSpy).not.toHaveBeenCalled();
    fetchSpy.mockRestore();
  });

  it("× 移除当前激活空间时，激活第一个剩余空间并导航过去", async () => {
    useOpenSpacesStore.setState({
      openIds: ["ws_a", "ws_b"],
      activeWsId: "ws_a",
      seeded: true,
    });
    const { container } = renderRail(false, "/spaces/ws_a");

    fireEvent.click(
      (await screen.findAllByRole("button", { name: "从打开列表移除" }))[0],
    );

    expect(useOpenSpacesStore.getState().openIds).toEqual(["ws_b"]);
    expect(useOpenSpacesStore.getState().activeWsId).toBe("ws_b");
    expect(screen.getByTestId("path")).toHaveTextContent(/^\/spaces\/ws_b$/);
    // 激活态必须落在看得见的那一行上，不能只剩页面标题还指着旧空间。
    expect(container.querySelectorAll('[aria-current="true"]')).toHaveLength(1);
    const activeRow = screen.getByRole("button", {
      name: "空间 B（团队 research）",
    });
    expect(activeRow).toHaveAttribute("aria-current", "true");
    // 选中态的形状是行容器上的圆角矩形 —— 主点击区自己不带框。
    const pill = activeRow.parentElement;
    expect(pill?.className).toContain("rounded-[6px]");
    expect(pill?.className).toContain("bg-brand/10");
    expect(ensureSpaceConversation).not.toHaveBeenCalled();
    expect(closeSpace).not.toHaveBeenCalled();
  });

  it("× 移除最后一个打开的空间时回工作室并清空激活态", async () => {
    const { container } = renderRail(false, "/spaces/ws_a");

    fireEvent.click(
      await screen.findByRole("button", { name: "从打开列表移除" }),
    );

    expect(useOpenSpacesStore.getState().openIds).toEqual([]);
    expect(useOpenSpacesStore.getState().activeWsId).toBeNull();
    expect(screen.getByTestId("path")).toHaveTextContent(/^\/spaces$/);
    expect(container.querySelectorAll('[aria-current="true"]')).toHaveLength(0);
    expect(closeSpace).not.toHaveBeenCalled();
  });

  it("× 移除的不是当前激活空间时不导航、不切激活", async () => {
    useOpenSpacesStore.setState({
      openIds: ["ws_a", "ws_b"],
      activeWsId: "ws_b",
      seeded: true,
    });
    renderRail(false, "/spaces/ws_b");

    fireEvent.click(
      (await screen.findAllByRole("button", { name: "从打开列表移除" }))[0],
    );

    expect(useOpenSpacesStore.getState().openIds).toEqual(["ws_b"]);
    expect(useOpenSpacesStore.getState().activeWsId).toBe("ws_b");
    expect(screen.getByTestId("path")).toHaveTextContent(/^\/spaces\/ws_b$/);
  });

  it("收起态保留行与全名 title，隐藏「状态」与 ×", async () => {
    renderRail(true);

    const row = await screen.findByRole("button", {
      name: "空间 A（团队 research）",
    });
    expect(row).toHaveAttribute("title", "空间 A");
    expect(screen.queryByRole("link", { name: "空间 A 状态" })).toBeNull();
    expect(screen.queryByRole("button", { name: "从打开列表移除" })).toBeNull();
  });

  it("「打开空间」进工作室页", async () => {
    renderRail();

    expect(
      await screen.findByRole("link", { name: "打开空间" }),
    ).toHaveAttribute("href", "/spaces");
  });

  it("不在 Peco 页时点 Peco 行：进 /peco 聊天并把请求归属切回 Peco", async () => {
    useSpaceStore.setState({ currentWsId: "ws_a" });
    renderRail(false, "/spaces/ws_a");

    fireEvent.click(await screen.findByRole("button", { name: "Peco" }));

    expect(screen.getByTestId("path")).toHaveTextContent("/peco");
    expect(screen.getByTestId("path")).not.toHaveTextContent("face=config");
    expect(useSpaceStore.getState().currentWsId).toBe("ws_peco");
  });

  it("已在 Peco 聊天面时再点 Peco 行：翻到能力配置", async () => {
    renderRail();

    fireEvent.click(await screen.findByRole("button", { name: "Peco" }));

    expect(screen.getByTestId("path")).toHaveTextContent("/peco?face=config");
  });

  it("在 Peco 能力配置面时点 Peco 行：回到聊天面", async () => {
    renderRail(false, "/peco?face=config");

    fireEvent.click(await screen.findByRole("button", { name: "Peco" }));

    expect(screen.getByTestId("path")).toHaveTextContent("/peco");
    expect(screen.getByTestId("path")).not.toHaveTextContent("face=config");
  });
});
