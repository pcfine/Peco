import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router-dom";
import { TeamConfigPanel } from "@/components/teams/TeamConfigPanel";

// 团队详情只提供头部与空间列表，接口层不在这里测。
vi.mock("@/queries/teamDetail", () => ({
  useTeam: () => ({
    isPending: false,
    error: null,
    refetch: () => {},
    data: {
      id: "x",
      display_name: "研究团队",
      template_version: 1,
      root: "/teams/x",
      has_master: true,
      agents: [{ name: "@master", description: "" }],
      skills: [],
      mcp_servers: [],
      knowledge_bases: [{ name: "kb", document_count: 0 }],
      spaces: [{ ws_id: "ws_1", work_dir: "/ws/1", closed_at: null }],
    },
  }),
}));

// 六个资源面板各有自己的网络逻辑，面板布局与它们无关。
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
  TeamMemoryTab: () => <div data-testid="memory-panel" />,
}));

/** 导航项切换要落到 URL 上：`?tab=` 是可分享、可回退的那份状态。 */
function SearchProbe() {
  const { search } = useLocation();
  return <output data-testid="search">{search}</output>;
}

function renderPanel(
  props: Partial<Parameters<typeof TeamConfigPanel>[0]> = {},
  entry = "/teams/x",
) {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <TeamConfigPanel teamId="x" wsId="ws_1" {...props} />
      <SearchProbe />
    </MemoryRouter>,
  );
}

function navItems(): HTMLElement[] {
  const nav = screen.getByRole("tablist");
  return within(nav).getAllByRole("tab");
}

function labelsOf(items: HTMLElement[]): (string | null)[] {
  return items.map((item) => item.getAttribute("aria-label"));
}

describe("TeamConfigPanel 分类导航", () => {
  it("按 agents/skills/mcp/knowledge/spaces/memory 排出 6 项", () => {
    renderPanel();

    expect(labelsOf(navItems())).toEqual([
      "agents",
      "skills",
      "MCP",
      "知识库",
      "绑定空间",
      "记忆",
    ]);
  });

  it("?tab=skills 只有 skills 项被选中", () => {
    renderPanel({}, "/teams/x?tab=skills");

    const selected = navItems()
      .filter((item) => item.getAttribute("aria-selected") === "true")
      .map((item) => item.getAttribute("aria-label"));
    expect(selected).toEqual(["skills"]);
  });

  it("点 MCP 项把 ?tab= 写成 mcp", () => {
    renderPanel({}, "/teams/x?tab=skills");

    fireEvent.click(screen.getByRole("tab", { name: "MCP" }));

    expect(screen.getByTestId("search").textContent).toBe("?tab=mcp");
    expect(
      screen.getByRole("tab", { name: "MCP" }).getAttribute("aria-selected"),
    ).toBe("true");
  });

  it("唯一的 tablist 是纵向分类栏，不再是横向 tab 条", () => {
    renderPanel();

    const navs = screen.getAllByRole("tablist");
    expect(navs).toHaveLength(1);
    expect(navs[0].tagName).toBe("NAV");
    expect(navs[0].getAttribute("aria-orientation")).toBe("vertical");
  });

  it("hideSpaces 时不出现「绑定空间」", () => {
    renderPanel({ hideSpaces: true });

    expect(labelsOf(navItems())).toEqual([
      "agents",
      "skills",
      "MCP",
      "知识库",
      "记忆",
    ]);
    expect(screen.queryByRole("tab", { name: "绑定空间" })).toBeNull();
  });
});
