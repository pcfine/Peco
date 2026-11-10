import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TeamListPage } from "@/pages/TeamListPage";
import { createTeam, deleteTeam, updateTeam } from "@/api/teams";

// 计数由列表接口一次带回；后端未就绪时字段缺失，卡片只能显示「–」。
vi.mock("@/api/teams", () => ({
  listTeams: async () => [
    {
      team_id: "research",
      display_name: "研究团队",
      template_version: 3,
      icon: "",
      agents: ["@master", "writer"],
      bound_spaces: ["ws_a"],
      agent_count: 2,
      skill_count: 4,
      mcp_count: 1,
      space_count: 3,
    },
    {
      team_id: "writer",
      display_name: "写作团队",
      template_version: 1,
      icon: "/uploads/teams/writer.png",
      agents: [],
      bound_spaces: [],
    },
  ],
  getTeam: async () => {
    throw new Error("卡片不该逐个查详情");
  },
  createTeam: vi.fn(async (body: { id: string; display_name?: string }) => ({
    team_id: body.id,
    display_name: body.display_name ?? body.id,
    template_version: 1,
    icon: "",
    agents: [],
    bound_spaces: [],
  })),
  updateTeam: vi.fn(async (teamId: string, body: { icon: string }) => ({
    team_id: teamId,
    display_name: "研究团队",
    template_version: 3,
    icon: body.icon,
    agents: [],
    bound_spaces: [],
  })),
  deleteTeam: vi.fn(async () => undefined),
}));

vi.mock("@/api/upload", () => ({
  uploadImage: vi.fn(async () => "/uploads/teams/new.png"),
}));

vi.mock("@/lib/image", () => ({
  resizeImage: vi.fn(async () => new Blob(["x"], { type: "image/png" })),
}));

function renderTeams() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={["/teams"]}>
        <Routes>
          <Route path="/teams" element={<TeamListPage />} />
          <Route
            path="/teams/:teamId"
            element={<span data-testid="path">detail</span>}
          />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function cardOf(container: HTMLElement, teamId: string): HTMLElement {
  const card = container.querySelector<HTMLElement>(
    `[data-team-id="${teamId}"]`,
  );
  if (!card) throw new Error(`没有 ${teamId} 的卡片`);
  return card;
}

function statOf(card: HTMLElement, key: string): HTMLElement {
  const stat = card.querySelector<HTMLElement>(`[data-stat="${key}"]`);
  if (!stat) throw new Error(`没有 ${key} 计数`);
  return stat;
}

describe("团队卡片", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("整卡是进团队配置的链接", async () => {
    renderTeams();

    expect(
      await screen.findByRole("link", { name: /研究团队/ }),
    ).toHaveAttribute("href", "/teams/research");
  });

  it("四个计数与团队 id 都渲染在卡上", async () => {
    const { container } = renderTeams();
    await screen.findByRole("link", { name: /研究团队/ });

    const card = cardOf(container, "research");
    expect(statOf(card, "agent")).toHaveTextContent("2");
    expect(statOf(card, "skill")).toHaveTextContent("4");
    expect(statOf(card, "mcp")).toHaveTextContent("1");
    expect(statOf(card, "space")).toHaveTextContent("3");
    expect(card).toHaveTextContent("research");
  });

  it("卡片是固定高度的圆角描边卡", async () => {
    const { container } = renderTeams();
    await screen.findByRole("link", { name: /研究团队/ });

    expect(container.querySelector("ul")).toBeTruthy();

    for (const teamId of ["research", "writer"]) {
      const card = cardOf(container, teamId);
      expect(card.className).toMatch(/\brounded-xl\b/);
      expect(card.className).toMatch(/\bborder\b/);
      expect(card.className).toMatch(/(^|\s)h-\[115px\](\s|$)/);
      expect(card.querySelector("a")).toHaveAttribute(
        "href",
        `/teams/${teamId}`,
      );
    }
  });

  it("没有图标时渲染像素头像，有 icon 时渲染图片", async () => {
    const { container } = renderTeams();
    await screen.findByRole("link", { name: /研究团队/ });

    expect(
      cardOf(container, "research").querySelector("[data-avatar] svg"),
    ).toBeTruthy();
    expect(
      cardOf(container, "writer").querySelector("[data-avatar] img"),
    ).toHaveAttribute("src", "/uploads/teams/writer.png");
  });

  it("相机与删除按钮都不在链接内；选图后 PATCH 图标", async () => {
    const { container } = renderTeams();
    await screen.findByRole("link", { name: /研究团队/ });

    const card = cardOf(container, "research");
    expect(card.querySelector("a button")).toBeNull();
    expect(
      screen.getByRole("button", { name: /更换 研究团队/ }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "删除 研究团队" }),
    ).toBeInTheDocument();

    const input = card.querySelector('input[type="file"]') as HTMLInputElement;
    fireEvent.change(input, {
      target: { files: [new File(["img"], "a.png", { type: "image/png" })] },
    });

    await waitFor(() =>
      expect(updateTeam).toHaveBeenCalledWith("research", {
        icon: "/uploads/teams/new.png",
      }),
    );
  });

  it("字段缺失显示「–」，不冒充 0", async () => {
    const { container } = renderTeams();
    await screen.findByRole("link", { name: /写作团队/ });

    const card = cardOf(container, "writer");
    for (const key of ["agent", "skill", "mcp", "space"]) {
      expect(statOf(card, key)).toHaveTextContent("–");
      expect(statOf(card, key)).not.toHaveTextContent("0");
    }
  });

  it("没有绑定空间的团队点删除直接删，不弹确认", async () => {
    renderTeams();
    await screen.findByRole("link", { name: /写作团队/ });

    fireEvent.click(screen.getByRole("button", { name: "删除 写作团队" }));

    await waitFor(() => expect(deleteTeam).toHaveBeenCalledWith("writer"));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("有绑定空间的团队先列出会失效的空间，确认后才删", async () => {
    renderTeams();
    await screen.findByRole("link", { name: /研究团队/ });

    fireEvent.click(screen.getByRole("button", { name: "删除 研究团队" }));

    expect(await screen.findByRole("dialog")).toHaveTextContent("ws_a");
    expect(screen.getByRole("dialog")).toHaveTextContent("1 个工作空间");
    expect(deleteTeam).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "删除团队" }));

    await waitFor(() => expect(deleteTeam).toHaveBeenCalledWith("research"));
  });
});

describe("新建团队", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("默认用内置模板创建，成功后进入团队详情", async () => {
    renderTeams();
    await screen.findByRole("link", { name: /研究团队/ });

    fireEvent.click(screen.getByRole("button", { name: "新建团队" }));
    fireEvent.change(screen.getByLabelText("团队 id（必填）"), {
      target: { value: "legal" },
    });
    fireEvent.change(screen.getByLabelText("显示名"), {
      target: { value: "法务" },
    });
    fireEvent.click(screen.getByRole("button", { name: "创建" }));

    await waitFor(() =>
      expect(createTeam).toHaveBeenCalledWith({
        id: "legal",
        display_name: "法务",
        from_template: "team_default",
      }),
    );
    expect(await screen.findByTestId("path")).toBeInTheDocument();
  });

  it("id 非法时禁用创建", async () => {
    renderTeams();
    await screen.findByRole("link", { name: /研究团队/ });

    fireEvent.click(screen.getByRole("button", { name: "新建团队" }));
    fireEvent.change(screen.getByLabelText("团队 id（必填）"), {
      target: { value: "Legal" },
    });

    expect(screen.getByRole("button", { name: "创建" })).toBeDisabled();
  });

  it("可以从已有团队复制：提交 from_team 而不是模板", async () => {
    renderTeams();
    await screen.findByRole("link", { name: /研究团队/ });

    fireEvent.click(screen.getByRole("button", { name: "新建团队" }));
    fireEvent.change(screen.getByLabelText("团队 id（必填）"), {
      target: { value: "clone" },
    });
    fireEvent.change(screen.getByLabelText("来源"), {
      target: { value: "copy" },
    });
    fireEvent.change(screen.getByLabelText("源团队"), {
      target: { value: "research" },
    });
    fireEvent.click(screen.getByRole("button", { name: "创建" }));

    await waitFor(() =>
      expect(createTeam).toHaveBeenCalledWith({
        id: "clone",
        display_name: "clone",
        from_team: "research",
      }),
    );
    expect(createTeam).not.toHaveBeenCalledWith(
      expect.objectContaining({ from_template: expect.anything() }),
    );
  });
});
