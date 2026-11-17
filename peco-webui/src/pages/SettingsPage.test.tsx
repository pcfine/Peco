import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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
/** 系统层的条目：只在这个 scope 被选中时请求。 */
let systemProviders: unknown[];

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

/** 打开配置范围下拉：Radix Select 用键盘开，jsdom 里的指针事件它不认。 */
async function openScopeSelect() {
  const trigger = await screen.findByRole("combobox", { name: "配置范围" });
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
  systemProviders = [];
  get.mockReset().mockImplementation(async (url: string) => {
    if (url === "/teams") return { data: teams };
    if (url === "/brand") return { data: { image: "" } };
    if (url === "/spaces") return { data: [space("ws_r", "research")] };
    if (url === "/teams/research/providers")
      return { data: [provider("qwen")] };
    if (url === "/teams/peco/providers")
      return { data: [provider("deepseek")] };
    // 系统层：不带团队的那组路由
    if (url === "/providers") return { data: systemProviders };
    if (url.endsWith("/providers/types")) return { data: TYPES };
    throw new Error(`unexpected GET ${url}`);
  });
  useSpaceStore.setState({ currentWsId: "ws_r" });
});

describe("设置页", () => {
  it("品牌区排在配置范围之前，并注明对所有空间与团队生效", async () => {
    renderPage();

    const brandHeading = await screen.findByRole("heading", { name: "品牌" });
    const scopeHeading = await screen.findByRole("heading", {
      name: "配置范围",
    });
    expect(
      brandHeading.compareDocumentPosition(scopeHeading) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(screen.getByText("对所有空间与团队生效")).toBeInTheDocument();
    expect(get).toHaveBeenCalledWith("/brand", undefined);
  });

  it("Peco 置顶并自报身份：兜底层先摆出来", () => {
    const ordered = orderTeams(teams);

    expect(ordered.map((row) => row.team_id)).toEqual(["peco", "research"]);
    expect(ordered.map(teamLabel)).toEqual(["Peco（默认兜底）", "研发"]);
    // 入参不被就地改写：调用方还在用原来那一份。
    expect(teams.map((row) => row.team_id)).toEqual(["research", "peco"]);
  });

  it("配置范围下拉来自 include=peco 的那份清单，系统层伪条目置顶", async () => {
    renderPage();
    const options = await openScopeSelect();

    // 继承链自上而下：系统层在最前，Peco 是团队层的兜底
    expect(options.map((option) => option.textContent)).toEqual([
      "系统（本机）",
      "Peco（默认兜底）",
      "研发",
    ]);
    expect(get).toHaveBeenCalledWith("/teams", { params: { include: "peco" } });
  });

  it("选中 Peco 时副标题说明它是继承层，拉的是 Peco 自己的 provider", async () => {
    get.mockImplementation(async (url: string) => {
      if (url === "/teams") return { data: teams };
      if (url === "/brand") return { data: { image: "" } };
      if (url === "/brand") return { data: { image: "" } };
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
    expect(
      screen.getByRole("combobox", { name: "配置范围" }),
    ).toHaveTextContent("Peco（默认兜底）");
    expect(get).toHaveBeenCalledWith("/teams/peco/providers", undefined);
  });

  it("Peco 视图里链上一条可用密钥都没有时摆一条提示", async () => {
    get.mockImplementation(async (url: string) => {
      if (url === "/teams") return { data: teams };
      if (url === "/brand") return { data: { image: "" } };
      if (url === "/brand") return { data: { image: "" } };
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
      await screen.findByText(/Peco 和系统层都没有一条能用的 provider 密钥/),
    ).toBeInTheDocument();
  });

  it("提示看的是整条链：任一层有可解析的密钥就算配好了", () => {
    expect(pecoNeedsSetup(undefined)).toBe(false);

    expect(pecoNeedsSetup([provider("deepseek")])).toBe(false);

    // 一条能用的都没有（没密钥 / 空列表）才提示
    expect(pecoNeedsSetup([provider("deepseek", "team", false)])).toBe(true);
    expect(pecoNeedsSetup([])).toBe(true);

    // 链的终点已迁到系统层：Peco 自己没有、系统层有，也够用
    expect(pecoNeedsSetup([provider("qwen", "peco")])).toBe(false);
    expect(pecoNeedsSetup([provider("openai", "system")])).toBe(false);
  });

  it("默认选当前空间所属团队，拉的是它的 provider 配置", async () => {
    renderPage();

    expect(await screen.findByText("qwen")).toBeInTheDocument();
    expect(screen.queryByText("deepseek")).toBeNull();
    expect(
      screen.getByRole("combobox", { name: "配置范围" }),
    ).toHaveTextContent("研发");
    expect(get).toHaveBeenCalledWith("/teams/research/providers", undefined);
  });

  it("只有一个团队时仍然能选到系统（本机）", async () => {
    teams = [team("research", "研发")];
    get.mockImplementation(async (url: string) => {
      if (url === "/teams") return { data: teams };
      if (url === "/brand") return { data: { image: "" } };
      if (url === "/spaces") return { data: [space("ws_r", "research")] };
      if (url === "/teams/research/providers")
        return { data: [provider("qwen")] };
      if (url === "/providers") return { data: [] };
      if (url.endsWith("/providers/types")) return { data: TYPES };
      throw new Error(`unexpected GET ${url}`);
    });
    renderPage();

    expect(await screen.findByText("qwen")).toBeInTheDocument();
    // 新装环境只有 Peco 一个团队：单团队不渲染下拉，系统层就点不到了
    const options = await openScopeSelect();
    expect(options.map((option) => option.textContent)).toContain(
      "系统（本机）",
    );
  });

  it("选中系统（本机）后保持选中，请求打到不带团队的 /api/providers", async () => {
    systemProviders = [provider("openai", "system")];
    renderPage();
    const options = await openScopeSelect();

    fireEvent.click(options[0]);

    await waitFor(() =>
      expect(get).toHaveBeenCalledWith("/providers", undefined),
    );
    // 选中值不被「回落到某个团队」改掉
    expect(
      screen.getByRole("combobox", { name: "配置范围" }),
    ).toHaveTextContent("系统（本机）");
    // 列表也换成了系统层那份：团队层的 qwen 不在这一屏
    expect(await screen.findByText("openai")).toBeInTheDocument();
    expect(screen.queryByText("qwen")).toBeNull();
  });

  it("零团队时仍渲染系统范围，并说明团队建好后自动继承", async () => {
    teams = [];
    get.mockImplementation(async (url: string) => {
      if (url === "/teams") return { data: teams };
      if (url === "/brand") return { data: { image: "" } };
      if (url === "/spaces") return { data: [] };
      if (url === "/providers") return { data: [] };
      if (url.endsWith("/providers/types")) return { data: TYPES };
      throw new Error(`unexpected GET ${url}`);
    });
    renderPage();

    expect(
      await screen.findByText(/还没有团队；先在这里配系统 provider/),
    ).toBeInTheDocument();
    expect(get).toHaveBeenCalledWith("/providers", undefined);
    expect(
      screen.getByText(
        "所有团队未配置的 provider 都在这里兜底；这一层不属于任何团队",
      ),
    ).toBeInTheDocument();
  });
});
