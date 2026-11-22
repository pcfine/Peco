import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
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

const closeSpace = vi.fn(async (_wsId: string) => ({ stopped: true }));

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

/** 有行、本次没开：关闭对它是无意义的，按钮只能是灰的。 */
const spaceC: Space = {
  ...spaceA,
  ws_id: "ws_c",
  work_dir: "/tmp/spaces/c/work",
  label: "空间 C",
  status: "closed",
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
  listSpaces: async () => [spaceA, spaceB, spaceC],
  ensureSpaceConversation: (wsId: string) => ensureSpaceConversation(wsId),
  closeSpace: (wsId: string) => closeSpace(wsId),
  listTrashSpaces: async () => [],
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
  closeSpace.mockClear();
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

  it("空闲的已打开空间能关闭：调接口并把行移出打开集", async () => {
    useOpenSpacesStore.setState({
      openIds: ["ws_a"],
      activeWsId: "ws_a",
      seeded: true,
    });
    renderList();

    const button = await screen.findByRole("button", {
      name: "关闭空间 空间 A",
    });
    expect(button).toBeEnabled();
    fireEvent.click(button);

    await waitFor(() => expect(closeSpace).toHaveBeenCalledWith("ws_a"));
    await waitFor(() =>
      expect(useOpenSpacesStore.getState().openIds).toEqual([]),
    );
  });

  it("在飞的空间与未打开的空间都不能关闭", async () => {
    renderList();

    const busy = await screen.findByRole("button", {
      name: "关闭空间 空间 B",
    });
    expect(busy).toBeDisabled();
    expect(busy).toHaveAttribute(
      "title",
      "还有 2 个任务在飞，等工作结束再关闭",
    );

    const dormant = screen.getByRole("button", { name: "关闭空间 空间 C" });
    expect(dormant).toBeDisabled();
    expect(dormant).toHaveAttribute("title", "空间未打开，无需关闭");
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
