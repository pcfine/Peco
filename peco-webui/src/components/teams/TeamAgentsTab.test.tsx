import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TeamAgentsTab } from "@/components/teams/TeamAgentsTab";
import { ApiError } from "@/lib/api";
import type {
  AgentDetail,
  AgentListItem,
  CreateAgentRequest,
  UpdateAgentRequest,
} from "@/types/daemon";
import type { SkillInfo } from "@/types/daemon";
import type { KnowledgeBase } from "@/types/daemon";

// 装配页的写入口全靠这四个模块，接口层不 mock 就测不到「发了什么请求体」。
const listAgents = vi.fn<(wsId: string) => Promise<AgentListItem[]>>();
const getAgent = vi.fn<(wsId: string, id: string) => Promise<AgentDetail>>();
const createAgent =
  vi.fn<(wsId: string, body: CreateAgentRequest) => Promise<AgentDetail>>();
const updateAgent =
  vi.fn<
    (wsId: string, id: string, body: UpdateAgentRequest) => Promise<AgentDetail>
  >();
const deleteAgent =
  vi.fn<(wsId: string, id: string) => Promise<{ success: boolean }>>();
const listSkills = vi.fn<(wsId: string) => Promise<SkillInfo[]>>();
const getMcpConfig = vi.fn<(wsId: string) => Promise<{ mcpServers: object }>>();
const listKnowledgeBases = vi.fn<(wsId: string) => Promise<KnowledgeBase[]>>();

vi.mock("@/api/agents", () => ({
  listAgents: (wsId: string) => listAgents(wsId),
  getAgent: (wsId: string, id: string) => getAgent(wsId, id),
  createAgent: (wsId: string, body: CreateAgentRequest) =>
    createAgent(wsId, body),
  updateAgent: (wsId: string, id: string, body: UpdateAgentRequest) =>
    updateAgent(wsId, id, body),
  deleteAgent: (wsId: string, id: string) => deleteAgent(wsId, id),
}));

vi.mock("@/api/skills", () => ({
  listSkills: (wsId: string) => listSkills(wsId),
  getSkillMd: async () => ({ name: "review", content: "" }),
}));

vi.mock("@/api/mcp", () => ({
  getMcpConfig: (wsId: string) => getMcpConfig(wsId),
}));

vi.mock("@/api/knowledge", () => ({
  listKnowledgeBases: (wsId: string) => listKnowledgeBases(wsId),
}));

function agentDetail(overrides: Partial<AgentDetail> = {}): AgentDetail {
  return {
    id: "a_master",
    name: "@master",
    description: "总管",
    system_prompt: "你是总管",
    model: "deepseek-flash",
    provider: "deepseek",
    icon: "🤖",
    background_color: "",
    status: "idle",
    tools: ["shell"],
    mcp_servers: [],
    skills: [],
    knowledge_bases: [],
    temperature: 0.7,
    max_tokens: null,
    stream: true,
    reasoning_effort: null,
    max_iterations: 500,
    supports_images: false,
    created_at: "",
    updated_at: "",
    ...overrides,
  };
}

const MASTER = agentDetail();
const HELPER = agentDetail({
  id: "a_helper",
  name: "助手",
  description: "干活的",
  system_prompt: "你是助手",
  tools: [],
  temperature: null,
  max_iterations: 200,
});

function listItem(detail: AgentDetail, isMaster: boolean): AgentListItem {
  return {
    id: detail.id,
    name: detail.name,
    description: detail.description,
    model: detail.model,
    provider: detail.provider,
    icon: detail.icon,
    background_color: detail.background_color,
    status: detail.status,
    tools: detail.tools,
    knowledge_bases: detail.knowledge_bases,
    is_master: isMaster,
    created_at: detail.created_at,
  };
}

let details: Map<string, AgentDetail>;
let rows: AgentListItem[];

function renderTab() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <TeamAgentsTab wsId="ws_a" />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  details = new Map([
    [MASTER.id, MASTER],
    [HELPER.id, HELPER],
  ]);
  rows = [listItem(MASTER, true), listItem(HELPER, false)];

  listAgents.mockReset().mockImplementation(async () => rows);
  getAgent
    .mockReset()
    .mockImplementation(async (_wsId, id) => details.get(id) as AgentDetail);
  createAgent.mockReset().mockImplementation(async (_wsId, body) => {
    const created = agentDetail({
      id: `a_${body.name}`,
      name: body.name,
      description: body.description ?? "",
      system_prompt: body.system_prompt ?? "",
      model: body.model ?? null,
      provider: body.provider ?? null,
      icon: body.icon ?? "🤖",
      tools: [],
      temperature: null,
      max_iterations: 500,
    });
    details.set(created.id, created);
    rows = [...rows, listItem(created, false)];
    return created;
  });
  updateAgent.mockReset().mockImplementation(async (_wsId, id, body) => {
    const next = { ...(details.get(id) as AgentDetail), ...body };
    details.set(id, next);
    return next;
  });
  deleteAgent.mockReset().mockResolvedValue({ success: true });
  listSkills
    .mockReset()
    .mockResolvedValue([{ name: "review", description: "代码审查" }]);
  getMcpConfig.mockReset().mockResolvedValue({
    mcpServers: { filesystem: {} },
  });
  listKnowledgeBases.mockReset().mockResolvedValue([
    {
      id: "kb_1",
      name: "handbook",
      description: "手册",
      backend: "lancedb",
      embedding_model: "bge",
      document_count: 1,
      chunk_count: 2,
      created_at: "",
      updated_at: "",
    },
  ]);
});

describe("团队 agents tab 装配", () => {
  it("填名称提交后调用 POST，列表出现新 agent 并被选中", async () => {
    renderTab();
    const createButtons = await screen.findAllByRole("button", {
      name: "＋ 新建 agent",
    });
    fireEvent.click(createButtons[0]);

    // 编辑器也有「描述」，查询要限定在对话框里。
    const dialog = within(await screen.findByRole("dialog"));
    fireEvent.change(dialog.getByLabelText("名称（必填）"), {
      target: { value: "审查员" },
    });
    fireEvent.change(dialog.getByLabelText("描述"), {
      target: { value: "看代码" },
    });
    fireEvent.click(dialog.getByRole("button", { name: "创建" }));

    await waitFor(() => expect(createAgent).toHaveBeenCalledTimes(1));
    expect(createAgent).toHaveBeenCalledWith("ws_a", {
      name: "审查员",
      description: "看代码",
    });
    expect(await screen.findByText("审查员")).toBeInTheDocument();
  });

  it("只改描述时 PATCH body 里没有 system_prompt", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: /助手/ }));

    fireEvent.change(await screen.findByLabelText("描述"), {
      target: { value: "改了描述" },
    });
    expect(screen.getByText("未保存")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(updateAgent).toHaveBeenCalledTimes(1));
    const [wsId, agentId, body] = updateAgent.mock.calls[0];
    expect(wsId).toBe("ws_a");
    expect(agentId).toBe("a_helper");
    expect(body).toEqual({ description: "改了描述" });
    expect(body).not.toHaveProperty("system_prompt");
    await waitFor(() => expect(screen.queryByText("未保存")).toBeNull());
  });

  it("没有改动时保存按钮禁用，改一个字才可用", async () => {
    renderTab();
    // 详情还在路上时编辑器是骨架屏，等它落地再看按钮。
    await screen.findByLabelText("名称");

    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();

    fireEvent.change(screen.getByLabelText("名称"), {
      target: { value: "助手二号" },
    });

    expect(screen.getByRole("button", { name: "保存" })).toBeEnabled();
  });

  it("切换 agent 丢弃上一个的草稿", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: /助手/ }));
    fireEvent.change(await screen.findByLabelText("描述"), {
      target: { value: "草稿" },
    });

    fireEvent.click(screen.getByRole("button", { name: /master/ }));
    await screen.findByDisplayValue("@master");
    fireEvent.click(screen.getByRole("button", { name: /助手/ }));

    expect(await screen.findByDisplayValue("干活的")).toBeInTheDocument();
  });

  it("@master 没有删除入口", async () => {
    renderTab();
    await screen.findByRole("button", { name: /@master/ });

    expect(screen.queryByRole("button", { name: "删除" })).toBeNull();
  });

  it("普通 agent 删除要过二次确认", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: /助手/ }));

    fireEvent.click(await screen.findByRole("button", { name: "删除" }));
    expect(deleteAgent).not.toHaveBeenCalled();
    expect(screen.getByText(/删除 agent「助手」/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "确认删除" }));

    await waitFor(() =>
      expect(deleteAgent).toHaveBeenCalledWith("ws_a", "a_helper"),
    );
  });

  it("删除被后端拒绝时把错误原文摆在确认框里", async () => {
    deleteAgent.mockRejectedValue(
      new ApiError(
        "'@master' is reserved: each space has exactly one master agent",
        {
          status: 400,
          code: "reserved_agent",
        },
      ),
    );
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: /助手/ }));

    fireEvent.click(await screen.findByRole("button", { name: "删除" }));
    fireEvent.click(screen.getByRole("button", { name: "确认删除" }));

    expect(await screen.findByText(/is reserved/)).toBeInTheDocument();
  });

  it("名称清空时不提交，只把原因摆在按钮旁", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: /助手/ }));

    fireEvent.change(await screen.findByLabelText("名称"), {
      target: { value: "  " },
    });

    expect(screen.getByText("名称不能为空")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
    expect(updateAgent).not.toHaveBeenCalled();
  });

  it("勾选 skill 后 PATCH body 的 skills 正确", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: /助手/ }));

    const chip = await screen.findByRole("button", { name: "review" });
    expect(chip).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(chip);
    expect(chip).toHaveAttribute("aria-pressed", "true");

    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(updateAgent).toHaveBeenCalledTimes(1));
    expect(updateAgent.mock.calls[0][2]).toEqual({ skills: ["review"] });
  });

  it("空列表给出引导与新建入口", async () => {
    rows = [];
    renderTab();

    expect(
      await screen.findByText("这个团队还没有 agent。"),
    ).toBeInTheDocument();
    expect(
      screen.getAllByRole("button", { name: "＋ 新建 agent" }).length,
    ).toBeGreaterThan(0);
  });
});
