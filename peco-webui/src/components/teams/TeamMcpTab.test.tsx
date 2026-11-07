import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ApiError } from "@/lib/api";
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

let servers: Record<string, unknown>;

function renderTab() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <TeamMcpTab wsId="ws_a" />
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

  it("编辑面板里是格式化好的整份配置", async () => {
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "编辑配置" }));

    expect(screen.getByLabelText("mcpconfig.json")).toHaveValue(
      JSON.stringify({ mcpServers: servers }, null, 2),
    );
  });

  it("顶层其它键也进编辑面板，保存时不会顺手丢", async () => {
    const whole = { mcpServers: servers, settings: { log: true } };
    get.mockImplementation(async (url: string) => {
      if (url === "/mcp") return { data: whole };
      throw new Error(`unexpected GET ${url}`);
    });
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "编辑配置" }));

    expect(screen.getByLabelText("mcpconfig.json")).toHaveValue(
      JSON.stringify(whole, null, 2),
    );
  });

  it("JSON 语法错误就地提示，不发请求", async () => {
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "编辑配置" }));
    fireEvent.change(screen.getByLabelText("mcpconfig.json"), {
      target: { value: '{ "mcpServers": ' },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    expect(await screen.findByText(/JSON 语法错误/)).toBeInTheDocument();
    expect(put).not.toHaveBeenCalled();
  });

  it("保存以整份对象调到 PUT /mcp，成功后收起编辑并刷新列表", async () => {
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "编辑配置" }));
    const next = {
      mcpServers: {
        filesystem: { command: "uvx" },
        added: { url: "http://x" },
      },
    };
    fireEvent.change(screen.getByLabelText("mcpconfig.json"), {
      target: { value: JSON.stringify(next, null, 2) },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(put).toHaveBeenCalledTimes(1));
    expect(put.mock.calls[0][0]).toBe("/mcp");
    expect(put.mock.calls[0][1]).toEqual(next);
    expect(put.mock.calls[0][2]).toEqual(SCOPE);

    await waitFor(() =>
      expect(screen.queryByLabelText("mcpconfig.json")).toBeNull(),
    );
    expect(
      await screen.findByRole("button", { name: /added/ }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /docs/ })).toBeNull();
  });

  it("后端 400 的原文摆在编辑器里，面板不关", async () => {
    put.mockRejectedValue(
      new ApiError(
        "Invalid MCP config: server 'filesystem': stdio transport requires 'command'",
        { status: 400 },
      ),
    );
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "编辑配置" }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    expect(
      await screen.findByText(/stdio transport requires 'command'/),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("mcpconfig.json")).toBeInTheDocument();
  });

  it("撤销丢掉草稿，不发请求", async () => {
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "编辑配置" }));
    fireEvent.change(screen.getByLabelText("mcpconfig.json"), {
      target: { value: '{ "mcpServers": {} }' },
    });
    fireEvent.click(screen.getByRole("button", { name: "撤销" }));

    expect(screen.queryByLabelText("mcpconfig.json")).toBeNull();
    expect(put).not.toHaveBeenCalled();
  });

  it("空配置引导提到可以直接粘 JSON", async () => {
    servers = {};
    renderTab();

    expect(await screen.findByText(/可以直接粘一份/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "编辑配置" })).toBeEnabled();
  });
});
