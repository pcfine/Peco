import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Rail } from "@/components/layout/Rail";
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

vi.mock("@/api/teams", () => ({ listTeams: async () => [] }));
vi.mock("@/api/tasks", () => ({ listTasks: async () => [] }));

function PathProbe() {
  const { pathname } = useLocation();
  return <span data-testid="path">{pathname}</span>;
}

function renderRail(collapsed = false) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={["/peco"]}>
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
  useOpenSpacesStore.setState({
    openIds: ["ws_a"],
    activeWsId: "ws_a",
    seeded: true,
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
    expect(screen.getByText("已打开")).toBeInTheDocument();
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

    expect(await screen.findByRole("link", { name: "打开空间" })).toHaveAttribute(
      "href",
      "/spaces",
    );
  });

  it("Peco 行进 /peco 并把请求归属切回 Peco", async () => {
    useSpaceStore.setState({ currentWsId: "ws_a" });
    renderRail();

    fireEvent.click(await screen.findByRole("button", { name: "Peco" }));

    expect(screen.getByTestId("path")).toHaveTextContent("/peco");
    expect(useSpaceStore.getState().currentWsId).toBe("ws_peco");
  });
});
