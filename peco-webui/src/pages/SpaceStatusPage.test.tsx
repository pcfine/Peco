import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ApiError } from "@/lib/api";
import { useSpaceStore } from "@/stores/spaceStore";
import { SpaceStatusPage } from "@/pages/SpaceStatusPage";
import type { Task } from "@/types/daemon";

// 状态页看的是 URL 里的 ws_id，不是「当前空间」：这两件事分开测才有意义。
const listConversations = vi.fn<
  (agentId: string, options: { wsId?: string }) => Promise<unknown[]>
>(async () => []);
const closeSpace = vi.fn<
  (wsId: string, force?: boolean) => Promise<{ stopped: boolean }>
>(async () => ({ stopped: true }));
const spaceTasks = vi.fn<() => Promise<Task[]>>(async () => []);

vi.mock("@/api/conversations", () => ({
  listConversations: (agentId: string, options: { wsId?: string }) =>
    listConversations(agentId, options),
  getMessages: async () => [],
  createConversation: async () => ({ id: "c_new" }),
  updateConversation: async () => ({ id: "c" }),
  deleteConversation: async () => ({ success: true }),
  uploadConversationImage: async () => ({ id: "i", url: "/i" }),
  conversationStreamUrl: () => "/api/chat/@master/conversations/c/stream",
}));

vi.mock("@/api/spaces", () => ({
  getSpace: async () => ({
    ws_id: "ws_a",
    work_dir: "/tmp/spaces/a/work",
    team_id: "research",
    label: "空间 A",
    exit_policy: "inherit",
    effective_exit_policy: "keep",
    status: "open",
    closed_at: null,
    last_opened_at: "2026-11-05 10:00:00",
    reserved: false,
    inflight_dispatch: 1,
    inflight_foreign: 0,
    marker: null,
  }),
  spaceTasks: () => spaceTasks(),
  spaceArtifacts: async () => ({ items: [], total: 0 }),
  spaceFiles: async () => ({ path: "", entries: [], truncated: false }),
  closeSpace: (wsId: string, force?: boolean) => closeSpace(wsId, force),
  listSpaces: async () => [],
  createSpace: async () => ({ ws_id: "ws_new" }),
  deleteSpace: async () => ({ purged: true }),
}));

function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "t1",
    task_id: "task_1",
    target_ws_id: "ws_a",
    target_work_dir: "/tmp/spaces/a/work",
    target_team_id: "research",
    origin_ws_id: "ws_peco",
    origin_kind: "dispatch",
    agent_name: "@master",
    task_state: "working",
    wake_state: "delivered",
    wake_skip_reason: null,
    created_at: "2026-11-05 10:00:00",
    terminal_at: null,
    result_digest: null,
    ...overrides,
  };
}

function renderStatus() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={["/spaces/ws_a/status"]}>
        {/* 必须走真实路由：wsId 来自 useParams，直接渲染组件拿到的是空参数。 */}
        <Routes>
          <Route path="/spaces/:wsId/status" element={<SpaceStatusPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  listConversations.mockClear();
  spaceTasks.mockReset();
  spaceTasks.mockResolvedValue([]);
  closeSpace.mockReset();
  closeSpace.mockResolvedValue({ stopped: true });
});

describe("空间状态页", () => {
  it("A20：请求归属取 URL 的 ws_id，不取当前空间；并给出「设为当前」", async () => {
    useSpaceStore.setState({ currentWsId: "ws_b" });
    renderStatus();

    expect(await screen.findByText("空间 A")).toBeInTheDocument();
    await waitFor(() => expect(listConversations).toHaveBeenCalled());
    expect(listConversations.mock.calls[0]?.[1]).toMatchObject({
      wsId: "ws_a",
    });
    expect(screen.getByText(/正在查看 空间 A，非当前空间/)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "设为当前" }),
    ).toBeInTheDocument();
  });

  it("在飞任务给一句话摘要，台账 tab 带上条数", async () => {
    spaceTasks.mockResolvedValue([task()]);
    renderStatus();

    expect(await screen.findByText("@master working")).toBeInTheDocument();
    expect(
      await screen.findByRole("tab", { name: /任务台账/ }),
    ).toHaveTextContent("1");
  });

  it("A26：关闭被 409 拦下时 Dialog 仍在，展示两个计数与「强制关闭」", async () => {
    useSpaceStore.setState({ currentWsId: "ws_a" });
    closeSpace.mockRejectedValueOnce(
      new ApiError(
        "workspace 'ws_a' has 3 dispatch and 0 foreign tasks inflight",
        {
          status: 409,
          code: "inflight_tasks",
          inflightDispatch: 3,
          inflightForeign: 0,
        },
      ),
    );
    renderStatus();

    fireEvent.click(await screen.findByRole("button", { name: "关闭" }));
    expect(await screen.findByText("仍有 3 个任务在飞")).toBeInTheDocument();
    expect(screen.getByText(/Peco 派发 3 个，外来 0 个/)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "强制关闭" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "等待完成" }),
    ).toBeInTheDocument();
  });

  it("「强制关闭」带 force 再发一次请求", async () => {
    useSpaceStore.setState({ currentWsId: "ws_a" });
    closeSpace.mockRejectedValueOnce(
      new ApiError("inflight", {
        status: 409,
        code: "inflight_tasks",
        inflightDispatch: 1,
        inflightForeign: 0,
      }),
    );
    renderStatus();

    fireEvent.click(await screen.findByRole("button", { name: "关闭" }));
    fireEvent.click(await screen.findByRole("button", { name: "强制关闭" }));
    await waitFor(() => expect(closeSpace).toHaveBeenCalledWith("ws_a", true));
  });
});
