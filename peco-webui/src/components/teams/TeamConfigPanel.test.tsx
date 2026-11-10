import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router-dom";
import { TeamConfigPanel } from "@/components/teams/TeamConfigPanel";

// 团队详情只提供身份块与空间列表，接口层不在这里测。
// 有没有 @master 是团队事实，某条用例要摆「缺 master」那一支。
const mocks = vi.hoisted(() => ({ hasMaster: true }));

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
      has_master: mocks.hasMaster,
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
  TeamAgentsTab: ({ noMaster }: { noMaster?: boolean }) => (
    <div data-testid="agents-panel" data-no-master={String(noMaster)} />
  ),
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

function PathProbe() {
  const { pathname } = useLocation();
  return <output data-testid="path">{pathname}</output>;
}

function renderPanel(
  props: Partial<Parameters<typeof TeamConfigPanel>[0]> = {},
  entry = "/teams/x",
) {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <TeamConfigPanel teamId="x" wsId="ws_1" {...props} />
      <SearchProbe />
      <PathProbe />
    </MemoryRouter>,
  );
}

beforeEach(() => {
  mocks.hasMaster = true;
});

function navItems(): HTMLElement[] {
  const nav = screen.getByRole("tablist");
  return within(nav).getAllByRole("tab");
}

function labelsOf(items: HTMLElement[]): (string | null)[] {
  return items.map((item) => item.getAttribute("aria-label"));
}

describe("TeamConfigPanel 分类导航", () => {
  it("按 agents/skills/mcp/knowledge/memory/spaces 排出 6 项", () => {
    renderPanel();

    expect(labelsOf(navItems())).toEqual([
      "agents",
      "skills",
      "MCP",
      "知识库",
      "记忆",
      "绑定空间",
    ]);
  });

  it("分组是「能力」与「工作」，记忆收在能力栏里知识库下方", () => {
    renderPanel();

    const nav = screen.getByRole("tablist");
    expect(within(nav).getByText("能力")).toBeInTheDocument();
    expect(within(nav).getByText("工作")).toBeInTheDocument();
    expect(within(nav).queryByText("其他")).toBeNull();

    // 记忆紧跟在知识库后面：能力栏先看完知识库，下一行就是记忆。
    const labels = labelsOf(navItems());
    expect(labels.indexOf("记忆")).toBe(labels.indexOf("知识库") + 1);
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

  it("hideSpaces 时不出现「绑定空间」，连整个「工作」分组一起收掉", () => {
    renderPanel({ hideSpaces: true });

    expect(labelsOf(navItems())).toEqual([
      "agents",
      "skills",
      "MCP",
      "知识库",
      "记忆",
    ]);
    expect(screen.queryByRole("tab", { name: "绑定空间" })).toBeNull();
    expect(screen.queryByText("工作")).toBeNull();
    expect(screen.queryByRole("button", { name: "新建空间" })).toBeNull();
  });

  it("去掉顶部 header：页面上没有 header 元素", () => {
    const { container } = renderPanel();

    expect(container.querySelectorAll("header")).toHaveLength(0);
  });

  it("团队名是页面唯一的 h1，root 只当它的 title，团队 id 不上屏", () => {
    renderPanel();

    const headings = screen.getAllByRole("heading", { level: 1 });
    expect(headings).toHaveLength(1);
    expect(headings[0].textContent).toBe("研究团队");
    expect(headings[0].getAttribute("title")).toBe("/teams/x");
    // id 只在路由里：分类栏顶部只留名称一行。
    expect(screen.queryByText("x")).toBeNull();
  });

  it("团队名左侧有返回图标：直接打开时退回团队列表", () => {
    renderPanel();
    const heading = screen.getByRole("heading", { level: 1, name: "研究团队" });
    const back = screen.getByRole("button", { name: "返回上一页" });

    expect(
      back.compareDocumentPosition(heading) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    // 直开没有上一页时兜底到团队列表。
    fireEvent.click(back);
    expect(screen.getByTestId("path")).toHaveTextContent("/teams");
  });

  it("缺 @master 时把 noMaster 交给 agents 面板", () => {
    mocks.hasMaster = false;
    renderPanel();

    expect(
      screen.getByTestId("agents-panel").getAttribute("data-no-master"),
    ).toBe("true");
  });
});
