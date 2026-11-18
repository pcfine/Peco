import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Rail } from "@/components/layout/Rail";
import { useDockStore } from "@/stores/dockStore";
import { useOpenSpacesStore } from "@/stores/openSpacesStore";
import { useSpaceStore } from "@/stores/spaceStore";
import type { Space } from "@/types/daemon";

/** 左栏取到的品牌图；默认未设置，用例里按需改写。 */
let brandImage = "";

const ensureSpaceConversation = vi.fn(async (_wsId: string) => ({
  conversation_id: "c",
  created: false,
}));
const closeSpace = vi.fn(async (_wsId: string, _force?: boolean) => ({
  stopped: true,
}));

function space(overrides: Partial<Space> & { ws_id: string }): Space {
  return {
    work_dir: `/tmp/${overrides.ws_id}/work`,
    team_id: "research",
    label: null,
    name: null,
    exit_policy: "inherit",
    effective_exit_policy: "inherit",
    status: "open",
    closed_at: null,
    last_opened_at: null,
    reserved: false,
    inflight_dispatch: 0,
    inflight_foreign: 0,
    marker: null,
    ...overrides,
  };
}

const spaces: Space[] = [
  space({ ws_id: "ws_peco", team_id: "peco", label: "Peco", reserved: true }),
  space({ ws_id: "ws_a", label: "空间 A" }),
  space({ ws_id: "ws_b", label: "空间 B" }),
  space({ ws_id: "ws_c", label: "空间 C", inflight_dispatch: 2 }),
];

vi.mock("@/api/peco", () => ({
  getPeco: async () => ({
    ws_id: "ws_peco",
    work_dir: "/tmp/ws_peco/work",
    team_id: "peco",
    reserved: true,
    from_marker: true,
  }),
}));

vi.mock("@/api/spaces", () => ({
  listSpaces: async () => spaces,
  ensureSpaceConversation: (wsId: string) => ensureSpaceConversation(wsId),
  closeSpace: (wsId: string, force?: boolean) => closeSpace(wsId, force),
}));

vi.mock("@/api/teams", () => ({
  listTeams: async () => [],
  createTeam: vi.fn(),
  updateTeam: vi.fn(),
  deleteTeam: vi.fn(),
}));
vi.mock("@/api/tasks", () => ({ listTasks: async () => [] }));
vi.mock("@/api/brand", () => ({
  getBrand: async () => ({ image: brandImage }),
  setBrand: async (image: string) => ({ image }),
}));

function PathProbe() {
  const { pathname, search } = useLocation();
  return (
    <span data-testid="path">
      {pathname}
      {search}
    </span>
  );
}

function renderRail(collapsed = false, path = "/peco") {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        <Rail collapsed={collapsed} />
        <PathProbe />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  localStorage.clear();
  brandImage = "";
  ensureSpaceConversation.mockClear();
  closeSpace.mockClear();
  useSpaceStore.setState({ currentWsId: "ws_peco" });
  useDockStore.setState({ open: false });
  useOpenSpacesStore.setState({
    openIds: ["ws_a"],
    activeWsId: "ws_a",
    seeded: true,
  });
});

describe("左栏品牌行", () => {
  it("品牌按钮只 toggle 抽屉，不导航", async () => {
    // 抽屉只在非 Peco 页存在，开关也只在那里有。
    renderRail(false, "/spaces/ws_a");

    const trigger = await screen.findByRole("button", { name: "Peco 对话" });
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    expect(trigger).toHaveAttribute("aria-controls", "peco-dock");

    fireEvent.click(trigger);
    expect(useDockStore.getState().open).toBe(true);
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    // 开关不是导航入口：点了还停在原页。
    expect(screen.getByTestId("path")).toHaveTextContent(/^\/spaces\/ws_a$/);

    fireEvent.click(trigger);
    expect(useDockStore.getState().open).toBe(false);
    expect(trigger).toHaveAttribute("aria-expanded", "false");
  });

  it("有品牌图时按钮里就是图片，开关语义不变", async () => {
    brandImage = "/uploads/brand/x.png";
    renderRail(false, "/spaces/ws_a");

    const trigger = await screen.findByRole("button", { name: "Peco 对话" });
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    expect(trigger).toHaveAttribute("aria-controls", "peco-dock");
    // 品牌图是异步取回来的：等它换掉默认标记。
    await waitFor(() =>
      expect(trigger.querySelector("img")).toHaveAttribute(
        "src",
        "/uploads/brand/x.png",
      ),
    );
    // 展开槽位 176×32：按钮让宽，折叠态仍是 32px 方块。
    expect(trigger.className).toContain("w-44");
    expect(trigger.className).toContain("h-8");

    fireEvent.click(trigger);
    expect(useDockStore.getState().open).toBe(true);
    expect(trigger).toHaveAttribute("aria-expanded", "true");
  });

  it("折叠态品牌图只露左半：内层按 2 倍宽度压缩", async () => {
    brandImage = "/uploads/brand/x.png";
    renderRail(true, "/spaces/ws_a");

    const trigger = await screen.findByRole("button", { name: "Peco 对话" });
    const img = await waitFor(() => {
      const found = trigger.querySelector<HTMLImageElement>(
        'img[src="/uploads/brand/x.png"]',
      );
      expect(found).not.toBeNull();
      return found as HTMLImageElement;
    });
    expect(img.className).toContain("w-[200%]");
    expect(img.className).toContain("object-fill");
    // 折叠槽位不因品牌图变宽：还是那枚 32px 的方块按钮。
    expect(trigger.className).toContain("size-8");
  });

  it("Peco 页的静态标记也换成品牌图", async () => {
    brandImage = "/uploads/brand/x.png";
    renderRail(false, "/peco");

    await screen.findByRole("button", { name: "Peco" });
    await waitFor(() =>
      expect(
        document.querySelector('img[src="/uploads/brand/x.png"]'),
      ).not.toBeNull(),
    );
    expect(screen.queryByRole("button", { name: "Peco 对话" })).toBeNull();
  });

  it("未设置品牌图时仍是默认标记", async () => {
    renderRail(false, "/spaces/ws_a");

    const trigger = await screen.findByRole("button", { name: "Peco 对话" });
    expect(trigger.className).toContain("size-8");
    expect(trigger.querySelector("img")).toHaveAttribute("src", "/favicon.svg");
  });

  it("在 Peco 页品牌行只是标记：没有开关按钮", async () => {
    renderRail(false, "/peco");

    // 等 Peco 行出现（它依赖身份查询）再断言「没有开关」。
    await screen.findByRole("button", { name: "Peco" });
    expect(screen.queryByRole("button", { name: "Peco 对话" })).toBeNull();
    expect(document.querySelector('[aria-controls="peco-dock"]')).toBeNull();
  });

  it("Peco 空间页同样没有开关：那一页自己就是会话本体", async () => {
    renderRail(false, "/spaces/ws_peco");

    await screen.findByRole("button", { name: "Peco" });
    expect(screen.queryByRole("button", { name: "Peco 对话" })).toBeNull();
    expect(document.querySelector('[aria-controls="peco-dock"]')).toBeNull();
  });
});

describe("左栏打开集", () => {
  it("只渲染打开集里的空间，不在集里的不出现在左栏", async () => {
    renderRail();

    expect(
      await screen.findByRole("button", { name: "空间 A（团队 research）" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /空间 B/ }),
    ).not.toBeInTheDocument();
  });

  it("主点击切激活态并进聊天页，不重发 ensure", async () => {
    renderRail();

    fireEvent.click(
      await screen.findByRole("button", { name: "空间 A（团队 research）" }),
    );

    expect(useOpenSpacesStore.getState().activeWsId).toBe("ws_a");
    expect(screen.getByTestId("path")).toHaveTextContent("/spaces/ws_a");
    expect(ensureSpaceConversation).not.toHaveBeenCalled();
  });

  it("× 只把空间移出打开集，不关闭空间、不发请求", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    renderRail();

    fireEvent.click(
      await screen.findByRole("button", { name: "从打开列表移除" }),
    );

    expect(useOpenSpacesStore.getState().openIds).toEqual([]);
    expect(closeSpace).not.toHaveBeenCalled();
    expect(ensureSpaceConversation).not.toHaveBeenCalled();
    expect(fetchSpy).not.toHaveBeenCalled();
    fetchSpy.mockRestore();
  });

  it("× 移除当前激活空间时，激活第一个剩余空间并导航过去", async () => {
    useOpenSpacesStore.setState({
      openIds: ["ws_a", "ws_b"],
      activeWsId: "ws_a",
      seeded: true,
    });
    const { container } = renderRail(false, "/spaces/ws_a");

    fireEvent.click(
      (await screen.findAllByRole("button", { name: "从打开列表移除" }))[0],
    );

    expect(useOpenSpacesStore.getState().openIds).toEqual(["ws_b"]);
    expect(useOpenSpacesStore.getState().activeWsId).toBe("ws_b");
    expect(screen.getByTestId("path")).toHaveTextContent(/^\/spaces\/ws_b$/);
    // 激活态必须落在看得见的那一行上，不能只剩页面标题还指着旧空间。
    expect(container.querySelectorAll('[aria-current="true"]')).toHaveLength(1);
    const activeRow = screen.getByRole("button", {
      name: "空间 B（团队 research）",
    });
    expect(activeRow).toHaveAttribute("aria-current", "true");
    // 选中态的形状是行容器上的圆角矩形 —— 主点击区自己不带框。
    const pill = activeRow.parentElement;
    expect(pill?.className).toContain("rounded-lg");
    expect(pill?.className).toContain("bg-brand/10");
    expect(ensureSpaceConversation).not.toHaveBeenCalled();
    expect(closeSpace).not.toHaveBeenCalled();
  });

  it("× 移除最后一个打开的空间时回工作室并清空激活态", async () => {
    const { container } = renderRail(false, "/spaces/ws_a");

    fireEvent.click(
      await screen.findByRole("button", { name: "从打开列表移除" }),
    );

    expect(useOpenSpacesStore.getState().openIds).toEqual([]);
    expect(useOpenSpacesStore.getState().activeWsId).toBeNull();
    expect(screen.getByTestId("path")).toHaveTextContent(/^\/spaces$/);
    expect(container.querySelectorAll('[aria-current="true"]')).toHaveLength(0);
    expect(closeSpace).not.toHaveBeenCalled();
  });

  it("× 移除的不是当前激活空间时不导航、不切激活", async () => {
    useOpenSpacesStore.setState({
      openIds: ["ws_a", "ws_b"],
      activeWsId: "ws_b",
      seeded: true,
    });
    renderRail(false, "/spaces/ws_b");

    fireEvent.click(
      (await screen.findAllByRole("button", { name: "从打开列表移除" }))[0],
    );

    expect(useOpenSpacesStore.getState().openIds).toEqual(["ws_b"]);
    expect(useOpenSpacesStore.getState().activeWsId).toBe("ws_b");
    expect(screen.getByTestId("path")).toHaveTextContent(/^\/spaces\/ws_b$/);
  });

  it("收起态保留行与全名 title，只留首字标记", async () => {
    renderRail(true);

    const row = await screen.findByRole("button", {
      name: "空间 A（团队 research）",
    });
    expect(row).toHaveAttribute("title", "空间 A");
    expect(screen.queryByRole("link", { name: "空间 A 状态" })).toBeNull();
    expect(screen.queryByRole("button", { name: "从打开列表移除" })).toBeNull();
    expect(screen.queryByLabelText("最近打开")).toBeNull();
    expect(screen.getByText("空")).toBeInTheDocument();
  });

  it("蓝点只点最近打开的那一行，其余空间留同宽空槽", async () => {
    useOpenSpacesStore.setState({
      openIds: ["ws_a", "ws_b"],
      activeWsId: "ws_a",
      seeded: true,
    });
    renderRail();

    const recentRow = await screen.findByRole("button", {
      name: "空间 A（团队 research）",
    });
    expect(screen.getByLabelText("最近打开")).toBeInTheDocument();
    expect(recentRow).toContainElement(screen.getByLabelText("最近打开"));

    const otherRow = screen.getByRole("button", {
      name: "空间 B（团队 research）",
    });
    expect(otherRow).not.toContainElement(screen.getByLabelText("最近打开"));
  });

  it("最近打开的行有在飞任务时：状态换成在运行，数量进按钮名字", async () => {
    useOpenSpacesStore.setState({
      openIds: ["ws_c"],
      activeWsId: "ws_c",
      seeded: true,
    });
    renderRail();

    // 展开态的数字徽章会被按钮自己的 aria-label 盖掉，数字因此也写进名字里。
    const row = await screen.findByRole("button", {
      name: "空间 C（团队 research） · 2 个任务在飞",
    });
    // `●` 只说「最近打开」，在跑由 `◉` 承担：两个意思挤进一个符号就分不清了。
    expect(screen.getByLabelText("在运行 · 2 个任务在飞")).toHaveTextContent(
      "◉",
    );
    expect(screen.queryByLabelText("最近打开")).toBeNull();
    // 徽章本身照旧摆着数量，只是不再自带 aria-label。
    expect(row).toHaveTextContent("2");
    expect(screen.queryByLabelText("2 个任务在飞")).toBeNull();
  });

  it("收起态没有徽章位置：在飞数进名字与 title，空闲行不带", async () => {
    useOpenSpacesStore.setState({
      openIds: ["ws_a", "ws_c"],
      activeWsId: "ws_c",
      seeded: true,
    });
    renderRail(true);

    const busy = await screen.findByRole("button", {
      name: "空间 C（团队 research） · 2 个任务在飞",
    });
    expect(busy).toHaveAttribute(
      "title",
      "空间 C（团队 research） · 2 个任务在飞",
    );
    // 收起态不摆数字徽章：整行只剩首字与那枚 brand 小圆点。
    expect(screen.queryByLabelText("2 个任务在飞")).toBeNull();

    const idle = screen.getByRole("button", {
      name: "空间 A（团队 research）",
    });
    expect(idle).toHaveAttribute("title", "空间 A");
    expect(idle).not.toHaveTextContent("任务在飞");
  });

  it("边框只跟当前路由：在团队页时空间行不高亮，团队入口高亮", async () => {
    renderRail(false, "/teams");

    const teamLink = await screen.findByRole("link", { name: /团队/ });
    expect(teamLink).toHaveClass("bg-brand/10");

    const row = await screen.findByRole("button", {
      name: "空间 A（团队 research）",
    });
    expect(row.parentElement?.className).not.toContain("bg-brand/10");
    expect(row).not.toHaveAttribute("aria-current");
    expect(screen.getByLabelText("最近打开")).toBeInTheDocument();
  });

  it("边框只跟当前路由：空间页高亮空间行，工作室入口不高亮", async () => {
    renderRail(false, "/spaces/ws_a");

    const row = await screen.findByRole("button", {
      name: "空间 A（团队 research）",
    });
    expect(row.parentElement?.className).toContain("bg-brand/10");
    expect(row).toHaveAttribute("aria-current", "true");
    expect(screen.getByRole("link", { name: "工作室" })).not.toHaveClass(
      "bg-brand/10",
    );
  });

  it("收起态保留栏目图标，文字退到 title / aria-label", async () => {
    renderRail(true);

    for (const name of ["工作室", "团队", "成果", "定时任务"]) {
      const link = await screen.findByRole("link", { name });
      expect(link).toHaveAttribute("title", name);
      expect(link).toHaveAttribute("href");
    }
  });

  it("不在 Peco 页时点 Peco 行：进 /peco 聊天并把请求归属切回 Peco", async () => {
    useSpaceStore.setState({ currentWsId: "ws_a" });
    renderRail(false, "/spaces/ws_a");

    fireEvent.click(await screen.findByRole("button", { name: "Peco" }));

    expect(screen.getByTestId("path")).toHaveTextContent("/peco");
    expect(useSpaceStore.getState().currentWsId).toBe("ws_peco");
  });

  it("已在 Peco 页时再点 Peco 行：只保持激活", async () => {
    renderRail();

    fireEvent.click(await screen.findByRole("button", { name: "Peco" }));

    expect(screen.getByTestId("path")).toHaveTextContent(/^\/peco$/);
  });

  it("在 Peco 团队配置页时点 Peco 行：回到聊天页", async () => {
    renderRail(false, "/teams/peco");

    fireEvent.click(await screen.findByRole("button", { name: "Peco" }));

    expect(screen.getByTestId("path")).toHaveTextContent("/peco");
  });
});
