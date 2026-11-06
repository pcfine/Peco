import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { App } from "@/App";

// 壳的挂载冒烟：只验证路由与面板都渲染出来。数据来源全换成固定值 ——
// 这一层测的不是取数，真接口由 daemon 的集成测试覆盖。
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
  listSpaces: async () => [
    {
      ws_id: "ws_peco",
      work_dir: "/tmp/peco",
      team_id: "peco",
      label: "Peco",
      exit_policy: "inherit",
      effective_exit_policy: "inherit",
      status: "open",
      reserved: true,
      inflight_dispatch: 0,
      inflight_foreign: 0,
      marker: null,
    },
  ],
  closeSpace: async () => ({ stopped: true }),
}));

vi.mock("@/api/teams", () => ({
  listTeams: async () => [],
  getTeam: async () => {
    throw new Error("unused in smoke test");
  },
}));

vi.mock("@/api/tasks", () => ({
  listTasks: async () => [],
}));

vi.mock("@/lib/events", () => ({
  openTaskStream: () => () => {},
}));

function renderAt(path: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        <App />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  localStorage.clear();
});

describe("壳与路由", () => {
  it("/ 渲染四块面板", async () => {
    renderAt("/");
    expect(await screen.findByTestId("panel-today")).toBeInTheDocument();
    expect(screen.getByTestId("panel-spaces")).toBeInTheDocument();
    expect(screen.getByTestId("panel-teams")).toBeInTheDocument();
    expect(screen.getByTestId("panel-results")).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Peco 面板" }),
    ).toBeInTheDocument();
  });

  it("/spaces 是列表页，空态给一句事实", async () => {
    renderAt("/spaces");
    expect(
      await screen.findByRole("heading", { name: "工作室" }),
    ).toBeInTheDocument();
    expect(await screen.findByText(/进行中/)).toBeInTheDocument();
  });

  it("/results 空台账给一句事实", async () => {
    renderAt("/results");
    expect(await screen.findByText(/台账里没有记录/)).toBeInTheDocument();
  });

  it("/spaces/new 两块同时可见，缺目录时主按钮禁用", async () => {
    renderAt("/spaces/new");
    // topbar 与页面各有一个 h1「新建空间」：这里只验两块表单同屏可见。
    expect(
      (await screen.findAllByRole("heading", { name: "新建空间" })).length,
    ).toBeGreaterThan(0);
    expect(screen.getByText("目录")).toBeInTheDocument();
    expect(screen.getByText("团队")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "创建并打开" })).toBeDisabled();
  });

  it("/schedules 是占位页，文案说清未实现", async () => {
    renderAt("/schedules");
    expect(await screen.findByText(/本期尚未实现/)).toBeInTheDocument();
  });

  it("未注册的路径给 404 文案，不是营销页", async () => {
    renderAt("/workflows/demo");
    expect(await screen.findByText("页面不存在")).toBeInTheDocument();
  });
});
