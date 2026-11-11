import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TeamSpacesTab } from "@/components/teams/TeamSpacesTab";

// 「新建空间」是内容区的动作：放在清单上方，点开后原地换成表单，
// 左侧「能力 / 工作」导航保持可见。
// 这里覆盖这块内容区自己的事实；导航在 TeamConfigPanel 的用例里。

vi.mock("@/api/spaces", () => ({
  listSpaces: async () => [],
  createSpace: async () => ({ ws_id: "ws_new" }),
}));

vi.mock("@/api/teams", () => ({
  listTeams: async () => [],
  createTeam: vi.fn(),
}));

const spaces = [
  { ws_id: "ws_1", work_dir: "/work/one", closed_at: null },
  { ws_id: "ws_2", work_dir: "/work/two", closed_at: "2026-11-01 10:00:00" },
];

function PathProbe() {
  const { pathname } = useLocation();
  return <output data-testid="path">{pathname}</output>;
}

function renderTab(rows = spaces) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={["/teams/team_x"]}>
        <TeamSpacesTab teamId="team_x" spaces={rows} />
        <PathProbe />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("TeamSpacesTab", () => {
  it("「新建空间」在绑定空间清单上方", () => {
    renderTab();

    const create = screen.getByRole("button", { name: /新建空间/ });
    const firstRow = screen.getByRole("link", { name: /\/work\/one/ });
    expect(
      create.compareDocumentPosition(firstRow) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("点「新建空间」在原地换成表单：不离开当前路由，团队锁定", () => {
    renderTab();

    fireEvent.click(screen.getByRole("button", { name: /新建空间/ }));

    expect(screen.getByLabelText("工作目录")).toBeInTheDocument();
    // 路由不变：左侧分类导航仍在同一页里。
    expect(screen.getByTestId("path")).toHaveTextContent("/teams/team_x");
    // 团队锁成当前团队。
    expect(screen.queryByText("由模板新建团队")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "返回列表" }));
    expect(
      screen.getByRole("link", { name: /\/work\/one/ }),
    ).toBeInTheDocument();
  });

  it("没有绑定空间时「新建空间」仍在头部，且只此一处", () => {
    renderTab([]);

    expect(
      screen.getByRole("button", { name: /新建空间/ }),
    ).toBeInTheDocument();
    expect(screen.getByText("这个团队还没有绑定空间")).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: /新建空间/ })).toHaveLength(1);
  });
});
