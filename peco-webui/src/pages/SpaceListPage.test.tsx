import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { SpaceListPage } from "@/pages/SpaceListPage";
import { useOpenSpacesStore } from "@/stores/openSpacesStore";
import { useSpaceStore } from "@/stores/spaceStore";
import type { Space } from "@/types/daemon";

const ensureSpaceConversation = vi.fn(async (_wsId: string) => ({
  conversation_id: "conv_a",
  created: true,
}));

const spaceA: Space = {
  ws_id: "ws_a",
  work_dir: "/tmp/spaces/a/work",
  team_id: "research",
  label: "空间 A",
  name: null,
  exit_policy: "inherit",
  effective_exit_policy: "inherit",
  status: "open",
  closed_at: null,
  last_opened_at: null,
  reserved: false,
  inflight_dispatch: 0,
  inflight_foreign: 0,
  marker: null,
};

/** 同一组里的一行空闲、一行在跑：状态位必须把这两行分开。 */
const spaceB: Space = {
  ...spaceA,
  ws_id: "ws_b",
  work_dir: "/tmp/spaces/b/work",
  label: "空间 B",
  inflight_dispatch: 2,
};

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
  listSpaces: async () => [spaceA, spaceB],
  ensureSpaceConversation: (wsId: string) => ensureSpaceConversation(wsId),
}));

vi.mock("@/api/tasks", () => ({ listTasks: async () => [] }));

function PathProbe() {
  const { pathname } = useLocation();
  return <span data-testid="path">{pathname}</span>;
}

function renderList() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={["/spaces"]}>
        <SpaceListPage />
        <PathProbe />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  localStorage.clear();
  ensureSpaceConversation.mockClear();
  useSpaceStore.setState({ currentWsId: "ws_peco" });
  useOpenSpacesStore.setState({
    openIds: [],
    activeWsId: null,
    seeded: true,
  });
});

describe("工作室列表", () => {
  it("点行进聊天：加入打开集、确保唯一会话、跳空间聊天页", async () => {
    renderList();

    fireEvent.click(
      await screen.findByRole("button", { name: "进入空间 空间 A" }),
    );

    expect(useOpenSpacesStore.getState().openIds).toEqual(["ws_a"]);
    expect(ensureSpaceConversation).toHaveBeenCalledWith("ws_a");
    expect(screen.getByTestId("path")).toHaveTextContent("/spaces/ws_a");
  });

  it("右侧「状态」进状态页，不进聊天页", async () => {
    renderList();

    expect(
      await screen.findByRole("link", { name: "空间 A 状态" }),
    ).toHaveAttribute("href", "/spaces/ws_a/status");
  });

  it("在跑的行状态位是「在运行 · N 个任务在飞」，空闲的行没有", async () => {
    renderList();

    const busyRow = await screen.findByRole("button", {
      name: "进入空间 空间 B",
    });
    expect(
      within(busyRow).getByLabelText("在运行 · 2 个任务在飞"),
    ).toHaveTextContent("◉");

    // 空闲的已打开空间不摆任何在跑标记：`●` 留给「最近打开」那一行。
    const idleRow = screen.getByRole("button", { name: "进入空间 空间 A" });
    expect(within(idleRow).queryByLabelText(/在运行/)).toBeNull();
    expect(within(idleRow).queryByText("◉")).toBeNull();
  });

  it("保留新建空间入口", async () => {
    renderList();

    expect(
      await screen.findByRole("link", { name: "新建空间" }),
    ).toHaveAttribute("href", "/spaces/new");
  });
});
