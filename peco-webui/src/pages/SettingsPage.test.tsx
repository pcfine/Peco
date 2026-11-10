import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { SettingsPage } from "@/pages/SettingsPage";
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

function team(teamId: string, displayName: string): Team {
  return {
    team_id: teamId,
    display_name: displayName,
    template_version: 1,
    icon: "",
    agents: [],
    bound_spaces: [],
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

function provider(name: string) {
  return {
    name,
    provider_type: "deepseek",
    base_url: null,
    default_model: null,
    api: null,
    has_api_key: true,
    is_default: false,
  };
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
  teams = [team("peco", "Peco"), team("research", "研发")];
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
