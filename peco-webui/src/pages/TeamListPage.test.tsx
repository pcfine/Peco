import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TeamListPage } from "@/pages/TeamListPage";

// 计数由列表接口一次带回；后端未就绪时字段缺失，卡片只能显示「–」。
vi.mock("@/api/teams", () => ({
  listTeams: async () => [
    {
      team_id: "research",
      display_name: "研究团队",
      template_version: 3,
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
      agents: [],
      bound_spaces: [],
    },
  ],
  getTeam: async () => {
    throw new Error("卡片不该逐个查详情");
  },
}));

function renderTeams() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <TeamListPage />
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
  it("整卡是进团队配置的链接", async () => {
    renderTeams();

    expect(
      await screen.findByRole("link", { name: /研究团队/ }),
    ).toHaveAttribute("href", "/teams/research");
  });

  it("四个计数与模板版本都渲染在卡上", async () => {
    const { container } = renderTeams();
    await screen.findByRole("link", { name: /研究团队/ });

    const card = cardOf(container, "research");
    expect(statOf(card, "agent")).toHaveTextContent("2");
    expect(statOf(card, "skill")).toHaveTextContent("4");
    expect(statOf(card, "mcp")).toHaveTextContent("1");
    expect(statOf(card, "space")).toHaveTextContent("3");
    expect(card).toHaveTextContent("v3");
    expect(card).toHaveTextContent("research");
  });

  it("卡片栅格靠共享分隔线分组，卡片自身不描边", async () => {
    const { container } = renderTeams();
    await screen.findByRole("link", { name: /研究团队/ });

    expect(container.querySelector(".board-grid")?.tagName).toBe("UL");

    for (const teamId of ["research", "writer"]) {
      const card = cardOf(container, teamId).querySelector("a");
      expect(card?.className).not.toMatch(/\bborder(-|\b)/);
    }
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
});
