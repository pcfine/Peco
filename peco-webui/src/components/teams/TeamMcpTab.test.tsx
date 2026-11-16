import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ApiError, type ResourceScope } from "@/lib/api";
import { TeamMcpTab } from "@/components/teams/TeamMcpTab";

// 只替换 axios 实例：PUT 的 body 与试连的 URL 都在这里看得见，接口层照原样跑。
const get = vi.fn();
const put = vi.fn();
const post = vi.fn();

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
    api: {
      get: (url: string, config?: unknown) => get(url, config),
      put: (url: string, body: unknown, config?: unknown) =>
        put(url, body, config),
      post: (url: string, body: unknown, config?: unknown) =>
        post(url, body, config),
      delete: () => Promise.reject(new Error("unexpected DELETE")),
    },
  };
});

const SCOPE = { headers: { "X-Peco-Space": "ws_a" } };
/** 借道已打开空间：能力根与 `X-Peco-Space` 指向同一个团队。 */
const TEAM_SCOPE: ResourceScope = { teamId: "t1", wsId: "ws_a" };

let servers: Record<string, unknown>;

function renderTab(scope: ResourceScope = TEAM_SCOPE) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <TeamMcpTab scope={scope} />
    </QueryClientProvider>,
  );
}

async function testButtons() {
  return screen.findAllByRole("button", { name: /试连/ });
}

beforeEach(() => {
  servers = {
    filesystem: { command: "npx", args: ["-y", "@mcp/filesystem"] },
    docs: { url: "https://example.com/sse" },
  };
  get.mockReset().mockImplementation(async (url: string) => {
    if (url === "/mcp") return { data: { mcpServers: servers } };
    throw new Error(`unexpected GET ${url}`);
  });
  put.mockReset().mockImplementation(async (_url: string, body: unknown) => {
    servers = (body as { mcpServers: Record<string, unknown> }).mcpServers;
    return { data: { success: true, message: "MCP config updated" } };
  });
  post.mockReset();
});

describe("团队 MCP tab 装配", () => {
  it("没有已打开空间时按团队寻址：列表与试连都发 X-Peco-Team", async () => {
    const TEAM = { headers: { "X-Peco-Team": "t1" } };
    post.mockResolvedValue({
      data: {
        success: true,
        server: "filesystem",
        transport: "stdio",
        tool_count: 1,
        tools: ["read_file"],
        message: "connected",
        duration_ms: 5,
      },
    });
    renderTab({ teamId: "t1", wsId: null });

    fireEvent.click((await testButtons())[0]);

    expect(await screen.findByText("连接成功 · 1 个工具 · 5ms")).toBeTruthy();
    expect(get).toHaveBeenCalledWith("/mcp", TEAM);
    expect(post).toHaveBeenCalledWith("/mcp/filesystem/test", undefined, TEAM);
  });

  it("试连成功显示工具数与耗时", async () => {
    post.mockResolvedValue({
      data: {
        success: true,
        server: "filesystem",
        transport: "stdio",
        tool_count: 3,
        tools: ["read_file", "write_file", "list"],
        message: "connected",
        duration_ms: 213,
      },
    });
    renderTab();

    fireEvent.click((await testButtons())[0]);

    expect(
      await screen.findByText("连接成功 · 3 个工具 · 213ms"),
    ).toBeInTheDocument();
    expect(post).toHaveBeenCalledWith("/mcp/filesystem/test", undefined, SCOPE);
  });

  it("试连失败显示服务端那句话并用 danger 色", async () => {
    post.mockResolvedValue({
      data: {
        success: false,
        server: "filesystem",
        transport: "stdio",
        tool_count: 0,
        tools: [],
        message: "spawn npx ENOENT",
        duration_ms: 12,
        error_type: "spawn_failed",
      },
    });
    renderTab();

    fireEvent.click((await testButtons())[0]);

    const line = await screen.findByText("连接失败 · spawn npx ENOENT");
    expect(line.className).toContain("text-danger");
  });

  it("试连请求本身失败（404 / daemon 不在）也落在同一行", async () => {
    post.mockRejectedValue(
      new ApiError("MCP server 'filesystem' not found in configuration", {
        status: 404,
      }),
    );
    renderTab();

    fireEvent.click((await testButtons())[0]);

    const line = await screen.findByText(/连接失败 · MCP server 'filesystem'/);
    expect(line.className).toContain("text-danger");
  });

  it("试连请求中所有试连按钮都禁用，点不动第二个", async () => {
    let finish: (value: unknown) => void = () => {};
    post.mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    renderTab();

    fireEvent.click((await testButtons())[0]);
    expect(await screen.findByText("试连中…")).toBeInTheDocument();

    const buttons = await testButtons();
    expect(buttons[0]).toBeDisabled();
    expect(buttons[1]).toBeDisabled();
    fireEvent.click(buttons[1]);
    expect(post).toHaveBeenCalledTimes(1);

    finish({
      data: {
        success: true,
        server: "filesystem",
        tool_count: 1,
        tools: ["read_file"],
        message: "connected",
        duration_ms: 40,
      },
    });
    expect(
      await screen.findByText("连接成功 · 1 个工具 · 40ms"),
    ).toBeInTheDocument();
  });

  it("卡片摆出状态 / transport / 地址 / 超时与重试，整张卡不是按钮", async () => {
    servers = {
      filesystem: {
        transport: "stdio",
        command: "npx",
        timeoutSecs: 30,
        maxRetries: 2,
      },
      docs: { transport: "sse", url: "https://example.com/sse" },
      archive: {
        transport: "sse",
        url: "https://example.com/sse",
        enabled: false,
      },
    };
    renderTab();

    expect(await screen.findByText("filesystem")).toBeInTheDocument();
    expect(screen.getByText("stdio")).toBeInTheDocument();
    expect(screen.getByText("npx")).toBeInTheDocument();
    expect(screen.getByText("· 30s")).toBeInTheDocument();
    expect(screen.getByText("· 重试 2 次")).toBeInTheDocument();

    // sse 认 url，不摆 command。
    expect(screen.getAllByText("https://example.com/sse")).toHaveLength(2);

    // 状态不只靠那个点：读屏能读到「已启用 / 已停用」。
    expect(screen.getAllByText("已启用")).toHaveLength(2);
    expect(screen.getByText("已停用")).toBeInTheDocument();

    // 卡不进编辑器：没有「返回列表」，也没有把整张卡做成按钮。
    expect(screen.queryByRole("button", { name: /返回列表/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /filesystem/ })).toBeNull();
  });

  it("配置缺字段或不是对象时卡片照常渲染", async () => {
    servers = {
      bare: {},
      half: { url: "http://x/y" },
      broken: 42,
    };
    renderTab();

    expect(await screen.findByText("bare")).toBeInTheDocument();
    // 没有 transport 时按 url 兜底，不把 undefined 摆到卡上。
    expect(screen.getByText("http://x/y")).toBeInTheDocument();
    expect(screen.getByText("broken")).toBeInTheDocument();
    expect(screen.queryByText("undefined")).toBeNull();
  });

  it("「添加MCP」弹表单，提交后只加这一项并保留其余 server", async () => {
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "＋ 添加MCP" }));

    fireEvent.change(screen.getByLabelText("名称（必填）"), {
      target: { value: "weather" },
    });
    fireEvent.change(screen.getByLabelText("命令（必填）"), {
      target: { value: "uvx" },
    });
    fireEvent.change(screen.getByLabelText("参数（每行一个）"), {
      target: { value: "weather-mcp\n--port 8080" },
    });
    fireEvent.change(screen.getByLabelText("环境变量（KEY=VALUE，每行一个）"), {
      target: { value: "API_KEY='secret'" },
    });
    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    await waitFor(() => expect(put).toHaveBeenCalledTimes(1));
    expect(put.mock.calls[0][0]).toBe("/mcp");
    expect(put.mock.calls[0][1]).toEqual({
      mcpServers: {
        ...servers,
        weather: {
          transport: "stdio",
          enabled: true,
          command: "uvx",
          args: ["weather-mcp", "--port 8080"],
          env: { API_KEY: "secret" },
          timeoutSecs: 30,
          maxRetries: 3,
        },
      },
    });
    expect(put.mock.calls[0][2]).toEqual(SCOPE);

    // 弹窗收起，新卡随着重新拉取摆出来。
    await waitFor(() =>
      expect(screen.queryByText("添加 MCP server")).toBeNull(),
    );
    expect(await screen.findByText("weather")).toBeInTheDocument();
  });

  it("名称重复时挡住提交，不再发 PUT", async () => {
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "＋ 添加MCP" }));
    fireEvent.change(screen.getByLabelText("名称（必填）"), {
      target: { value: "filesystem" },
    });
    fireEvent.change(screen.getByLabelText("命令（必填）"), {
      target: { value: "npx" },
    });

    expect(await screen.findByText("该名称已存在")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "添加" })).toBeDisabled();
    expect(put).not.toHaveBeenCalled();
  });

  it("必填命令没填时「添加」不可点", async () => {
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "＋ 添加MCP" }));
    fireEvent.change(screen.getByLabelText("名称（必填）"), {
      target: { value: "weather" },
    });

    expect(screen.getByRole("button", { name: "添加" })).toBeDisabled();
  });

  it("添加时顶层其它键原样带回去", async () => {
    const whole = { mcpServers: servers, settings: { log: true } };
    get.mockImplementation(async (url: string) => {
      if (url === "/mcp") return { data: whole };
      throw new Error(`unexpected GET ${url}`);
    });
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "＋ 添加MCP" }));
    fireEvent.change(screen.getByLabelText("名称（必填）"), {
      target: { value: "weather" },
    });
    fireEvent.change(screen.getByLabelText("命令（必填）"), {
      target: { value: "uvx" },
    });
    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    await waitFor(() => expect(put).toHaveBeenCalledTimes(1));
    expect(put.mock.calls[0][1]).toMatchObject({
      settings: { log: true },
      mcpServers: { weather: { command: "uvx" } },
    });
  });

  it("空配置只留一句空态，不再摆操作说明", async () => {
    servers = {};
    renderTab();

    expect(await screen.findByText("这里什么也没有")).toBeInTheDocument();
    expect(screen.queryByText(/可以直接粘一份/)).toBeNull();
    expect(screen.getByRole("button", { name: "＋ 添加MCP" })).toBeEnabled();
  });
});
