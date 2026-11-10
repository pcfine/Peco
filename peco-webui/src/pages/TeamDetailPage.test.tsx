import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { TeamDetailPage } from "@/pages/TeamDetailPage";

// Peco 是保留团队，特例化只在页面这一层：能力根固定挂 Peco 空间、记忆按空间看、
// 没有「新建空间 / 绑定空间」。这里只测这三处，面板内部与各 tab 的取数在别处测。

vi.mock("@/queries/teamDetail", () => ({
  useTeam: (teamId: string) => ({
    isPending: false,
    error: null,
    refetch: () => {},
    data: {
      id: teamId,
      display_name: teamId === "peco" ? "Peco" : "研究团队",
      template_version: 1,
      root: `/teams/${teamId}`,
      has_master: true,
      agents: [],
      skills: [],
      mcp_servers: [],
      knowledge_bases: [],
      spaces: [],
    },
  }),
}));

vi.mock("@/queries/peco", () => ({
  usePeco: () => ({
    data: {
      ws_id: "ws_peco",
      work_dir: "/peco/work",
      team_id: "peco",
      reserved: true,
      from_marker: true,
    },
  }),
}));

vi.mock("@/queries/spaces", () => ({
  useSpaces: () => ({
    data: [
      {
        ws_id: "ws_peco",
        work_dir: "/peco/work",
        team_id: "peco",
        label: "Peco",
        status: "open",
        closed_at: null,
      },
    ],
  }),
}));

vi.mock("@/components/teams/TeamAgentsTab", () => ({
  TeamAgentsTab: () => <div data-testid="agents-panel" />,
}));
vi.mock("@/components/teams/TeamSkillsTab", () => ({
  TeamSkillsTab: () => <div data-testid="skills-panel" />,
}));
vi.mock("@/components/teams/TeamMcpTab", () => ({
  TeamMcpTab: () => <div data-testid="mcp-panel" />,
}));
vi.mock("@/components/teams/TeamKnowledgeTab", () => ({
  TeamKnowledgeTab: () => <div data-testid="knowledge-panel" />,
}));
vi.mock("@/components/teams/TeamSpacesTab", () => ({
  TeamSpacesTab: () => <div data-testid="spaces-panel" />,
}));
vi.mock("@/components/teams/TeamMemoryTab", () => ({
  TeamMemoryTab: () => <div data-testid="team-memory" />,
}));
vi.mock("@/components/spaces/SpaceMemoryTab", () => ({
  SpaceMemoryTab: ({ wsId }: { wsId: string }) => (
    <div data-testid="space-memory" data-ws-id={wsId} />
  ),
}));

function renderTeam(teamId: string) {
  return render(
    <MemoryRouter initialEntries={[`/teams/${teamId}`]}>
      <Routes>
        <Route path="/teams/:teamId" element={<TeamDetailPage />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe("TeamDetailPage", () => {
  it("Peco 团队：没有绑定空间与新建空间，记忆浏览 Peco 空间自己的记忆", () => {
    renderTeam("peco");

    expect(screen.queryByRole("tab", { name: "绑定空间" })).toBeNull();
    expect(screen.queryByRole("button", { name: "新建空间" })).toBeNull();
    expect(screen.queryByText("工作")).toBeNull();

    // 记忆收在能力栏里，直接绑 Peco 空间的 ws。
    fireEvent.click(screen.getByRole("tab", { name: "记忆" }));
    expect(screen.getByTestId("space-memory")).toHaveAttribute(
      "data-ws-id",
      "ws_peco",
    );
    expect(screen.queryByTestId("team-memory")).toBeNull();
  });

  it("普通团队：绑定空间在导航里，记忆走团队维度占位", () => {
    renderTeam("research");

    expect(screen.getByRole("tab", { name: "绑定空间" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("tab", { name: "记忆" }));
    expect(screen.getByTestId("team-memory")).toBeInTheDocument();
    expect(screen.queryByTestId("space-memory")).toBeNull();
  });
});
