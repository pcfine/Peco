import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  SettingsPage,
  orderTeams,
  pecoNeedsSetup,
  teamLabel,
} from "@/pages/SettingsPage";
import { useSpaceStore } from "@/stores/spaceStore";
import type { Space, Team } from "@/types/daemon";

// 只替换 axios 实例：默认选中哪个团队，从「取的是哪个团队的 providers」看得出来。
const get = vi.fn();

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
    api: {
      get: (url: string, config?: unknown) => get(url, config),
      put: () => Promise.reject(new Error("unexpected PUT")),
      post: () => Promise.reject(new Error("unexpected POST")),
      delete: () => Promise.reject(new Error("unexpected DELETE")),
    },
  };
});

function team(teamId: string, displayName: string, reserved = false): Team {
  return {
    team_id: teamId,
    display_name: displayName,
    template_version: 1,
    icon: "",
    agents: [],
    bound_spaces: [],
    reserved,
  };
}

function space(wsId: string, teamId: string): Space {
  return {
    ws_id: wsId,
    work_dir: `/tmp/${wsId}`,
    team_id: teamId,
    label: null,
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
}

const TYPES = [
  {
    provider_type: "deepseek",
    display_name: "DeepSeek",
    default_base_url: "https://api.deepseek.com",
    api_modes: ["responses"],
    suggested_models: [],
    default_model: "deepseek-v4-flash",
  },
];

let teams: Team[];

function provider(
  name: string,
  source: "team" | "peco" | "system" = "team",
  hasApiKey = true,
) {
  return {
    name,
    provider_type: "deepseek",
    base_url: null,
    default_model: null,
    api: null,
    has_api_key: hasApiKey,
    is_default: false,
    source,
  };
}

/** 打开团队下拉：Radix Select 用键盘开，jsdom 里的指针事件它不认。 */
async function openTeamSelect() {
  const trigger = await screen.findByRole("combobox", { name: "团队" });
  fireEvent.keyDown(trigger, { key: "Enter" });
  return screen.findAllByRole("option");
}

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <SettingsPage />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  teams = [team("research", "研发"), team("peco", "Peco", true)];
  get.mockReset().mockImplementation(async (url: string) => {
    if (url === "/teams") return { data: teams };
    if (url === "/spaces") return { data: [space("ws_r", "research")] };
    if (url === "/teams/research/providers")
      return { data: [provider("qwen")] };
    if (url === "/teams/peco/providers")
      return { data: [provider("deepseek")] };
    if (url.endsWith("/providers/types")) return { data: TYPES };
    throw new Error(`unexpected GET ${url}`);
  });
  useSpaceStore.setState({ currentWsId: "ws_r" });
});

describe("设置页", () => {
  it("Peco 置顶并自报身份：兜底层先摆出来", () => {
    const ordered = orderTeams(teams);

    expect(ordered.map((row) => row.team_id)).toEqual(["peco", "research"]);
    expect(ordered.map(teamLabel)).toEqual(["Peco（默认兜底）", "研发"]);
    // 入参不被就地改写：调用方还在用原来那一份。
    expect(teams.map((row) => row.team_id)).toEqual(["research", "peco"]);
  });

  it("团队下拉来自 include=peco 的那份清单，Peco 排在第一个", async () => {
    renderPage();
    const options = await openTeamSelect();

    expect(options.map((option) => option.textContent)).toEqual([
      "Peco（默认兜底）",
      "研发",
    ]);
    expect(get).toHaveBeenCalledWith("/teams", { params: { include: "peco" } });
  });

  it("选中 Peco 时副标题说明它是继承层，拉的是 Peco 自己的 provider", async () => {
    get.mockImplementation(async (url: string) => {
      if (url === "/teams") return { data: teams };
      if (url === "/spaces") return { data: [space("ws_p", "peco")] };
      if (url === "/teams/peco/providers")
        return { data: [provider("deepseek")] };
      if (url.endsWith("/providers/types")) return { data: TYPES };
      throw new Error(`unexpected GET ${url}`);
    });
    useSpaceStore.setState({ currentWsId: "ws_p" });
    renderPage();

    expect(
      await screen.findByText("其它团队未配置的 provider 从这里继承"),
    ).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "团队" })).toHaveTextContent(
      "Peco（默认兜底）",
    );
    expect(get).toHaveBeenCalledWith("/teams/peco/providers", undefined);
  });

  it("Peco 没有任何带密钥的自有条目时摆一条提示", async () => {
    get.mockImplementation(async (url: string) => {
      if (url === "/teams") return { data: teams };
      if (url === "/spaces") return { data: [space("ws_r", "research")] };
      if (url === "/teams/research/providers")
        return { data: [provider("qwen")] };
      if (url === "/teams/peco/providers")
        return { data: [provider("deepseek", "peco", false)] };
      if (url.endsWith("/providers/types")) return { data: TYPES };
      throw new Error(`unexpected GET ${url}`);
    });
    renderPage();

    expect(
      await screen.findByText(/Peco 还没有配置自己的 provider 密钥/),
    ).toBeInTheDocument();
  });

  it("提示只看 Peco 的自有条目：没拉到、或自有条目已带密钥都不提示", () => {
    expect(pecoNeedsSetup(undefined)).toBe(false);

    expect(pecoNeedsSetup([provider("deepseek")])).toBe(false);

    // 自己的条目没密钥，或压根没有自有条目（列表里只剩继承来的）。
    expect(pecoNeedsSetup([provider("deepseek", "team", false)])).toBe(true);
    expect(pecoNeedsSetup([])).toBe(true);
    expect(pecoNeedsSetup([provider("qwen", "peco")])).toBe(true);
    expect(pecoNeedsSetup([provider("openai", "system")])).toBe(true);
  });

  it("默认选当前空间所属团队，拉的是它的 provider 配置", async () => {
    renderPage();

    expect(await screen.findByText("qwen")).toBeInTheDocument();
    expect(screen.queryByText("deepseek")).toBeNull();
    expect(screen.getByRole("combobox", { name: "团队" })).toHaveTextContent(
      "研发",
    );
    expect(get).toHaveBeenCalledWith("/teams/research/providers", undefined);
  });

  it("只有一个团队时不摆选择器，直接显示团队名", async () => {
    teams = [team("research", "研发")];
    get.mockImplementation(async (url: string) => {
      if (url === "/teams") return { data: teams };
      if (url === "/spaces") return { data: [space("ws_r", "research")] };
      if (url === "/teams/research/providers")
        return { data: [provider("qwen")] };
      if (url.endsWith("/providers/types")) return { data: TYPES };
      throw new Error(`unexpected GET ${url}`);
    });
    renderPage();

    expect(await screen.findByText("qwen")).toBeInTheDocument();
    expect(screen.queryByRole("combobox", { name: "团队" })).toBeNull();
    expect(screen.getByText("研发")).toBeInTheDocument();
  });
});
