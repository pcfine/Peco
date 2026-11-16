import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ApiError, type ResourceScope } from "@/lib/api";
import { TeamSkillsTab } from "@/components/teams/TeamSkillsTab";
import type { AgentDetail, AgentListItem, SkillInfo } from "@/types/daemon";

// 只替换 axios 实例：PUT / DELETE 的 URL、body 与空间头都在这里看得见。
const get = vi.fn();
const put = vi.fn();
const del = vi.fn();

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
    api: {
      get: (url: string, config?: unknown) => get(url, config),
      put: (url: string, body: unknown, config?: unknown) =>
        put(url, body, config),
      post: () => Promise.reject(new Error("unexpected POST")),
      delete: (url: string, config?: unknown) => del(url, config),
    },
  };
});

const SCOPE = { headers: { "X-Peco-Space": "ws_a" } };
/** 借道已打开空间：能力根与 `X-Peco-Space` 指向同一个团队。 */
const TEAM_SCOPE: ResourceScope = { teamId: "t1", wsId: "ws_a" };

const REVIEW_MD =
  "---\nname: review\ndescription: 代码审查\n---\n\nreview 的正文\n";
const TRANSLATE_MD =
  "---\nname: translate\ndescription: 翻译\n---\n\ntranslate 的正文\n";

let rows: SkillInfo[];
let contents: Map<string, string>;
let agents: AgentListItem[];
let details: Map<string, AgentDetail>;

function agentDetail(overrides: Partial<AgentDetail>): AgentDetail {
  return {
    id: "a_master",
    name: "@master",
    description: "总管",
    system_prompt: "你是总管",
    model: "deepseek-flash",
    provider: "deepseek",
    status: "idle",
    tools: [],
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

function listItem(detail: AgentDetail, isMaster: boolean): AgentListItem {
  return {
    id: detail.id,
    name: detail.name,
    description: detail.description,
    model: detail.model,
    provider: detail.provider,
    status: detail.status,
    tools: detail.tools,
    mcp_servers: detail.mcp_servers,
    skills: detail.skills,
    knowledge_bases: detail.knowledge_bases,
    is_master: isMaster,
    created_at: detail.created_at,
  };
}

/** 卡是入口：点卡直接进编辑器，SKILL.md 的源码框就在里面。 */
async function openEditor(name = "review") {
  fireEvent.click(
    await screen.findByRole("button", { name: new RegExp(`^${name}`) }),
  );
  await screen.findByRole("button", { name: "保存修改" });
}

function renderTab(scope: ResourceScope = TEAM_SCOPE) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <TeamSkillsTab scope={scope} />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  rows = [
    { name: "review", description: "代码审查" },
    { name: "translate", description: "翻译" },
  ];
  contents = new Map([
    ["review", REVIEW_MD],
    ["translate", TRANSLATE_MD],
  ]);
  details = new Map([
    ["a_master", agentDetail({})],
    [
      "a_helper",
      agentDetail({ id: "a_helper", name: "助手", skills: ["review"] }),
    ],
  ]);
  agents = [
    listItem(details.get("a_master")!, true),
    listItem(details.get("a_helper")!, false),
  ];

  get.mockReset().mockImplementation(async (url: string) => {
    if (url === "/skills") return { data: rows };
    if (url === "/agents") return { data: agents };
    if (url.startsWith("/agents/")) {
      const id = url.slice("/agents/".length);
      return { data: details.get(id) };
    }
    // 编辑器打开时先拉目录清单；测试里都按「没有资源文件」处理。
    if (url.endsWith("/files")) return { data: { files: [] } };
    if (url.startsWith("/skills/")) {
      const name = url.slice("/skills/".length);
      const content = contents.get(name);
      if (content === undefined)
        throw new ApiError("skill not found", { status: 404 });
      return { data: { name, content } };
    }
    throw new Error(`unexpected GET ${url}`);
  });

  put.mockReset().mockImplementation(async (url: string, body: unknown) => {
    const name = url.slice("/skills/".length);
    const { content } = body as { content: string; files: unknown[] };
    contents.set(name, content);
    if (!rows.some((row) => row.name === name)) {
      rows = [...rows, { name, description: "" }];
    }
    return { data: { success: true, message: `Skill '${name}' saved` } };
  });

  del.mockReset().mockImplementation(async (url: string) => {
    const name = url.slice("/skills/".length);
    contents.delete(name);
    rows = rows.filter((row) => row.name !== name);
    return { data: { success: true, message: `Skill '${name}' deleted` } };
  });
});

describe("团队 skills tab 装配", () => {
  it("没有已打开空间时按团队寻址：列表与写删都发 X-Peco-Team", async () => {
    const TEAM = { headers: { "X-Peco-Team": "t1" } };
    renderTab({ teamId: "t1", wsId: null });

    expect(
      await screen.findByRole("button", { name: /^review/ }),
    ).toBeInTheDocument();
    expect(get).toHaveBeenCalledWith("/skills", TEAM);
    // 删除确认要列「删掉会让谁缺能力」，agent 列表也得按同一个团队取。
    expect(get).toHaveBeenCalledWith("/agents", TEAM);

    await openEditor();
    fireEvent.change(screen.getByLabelText("SKILL.md 内容"), {
      target: {
        value: "---\nname: review\ndescription: 代码审查\n---\n\n新正文\n",
      },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存修改" }));
    await waitFor(() => expect(put).toHaveBeenCalledTimes(1));
    expect(put.mock.calls[0][2]).toEqual(TEAM);

    fireEvent.click(screen.getByRole("button", { name: "返回列表" }));
    fireEvent.click(await screen.findByRole("button", { name: "删除 review" }));
    const confirm = await screen.findByRole("button", { name: "确认删除" });
    await waitFor(() => expect(confirm).toBeEnabled());
    fireEvent.click(confirm);
    await waitFor(() =>
      expect(del).toHaveBeenCalledWith("/skills/review", TEAM),
    );
  });

  it("新建调 PUT /skills/{name}，body 带 content 与空 files", async () => {
    const created = "---\nname: code-review\ndescription: 审\n---\n\n正文\n";
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "＋ 新建技能" }));
    const dialog = within(await screen.findByRole("dialog"));
    fireEvent.change(dialog.getByLabelText("名称（必填）"), {
      target: { value: "code-review" },
    });
    fireEvent.change(dialog.getByLabelText("内容（SKILL.md）"), {
      target: { value: created },
    });
    fireEvent.click(dialog.getByRole("button", { name: "创建" }));

    await waitFor(() => expect(put).toHaveBeenCalledTimes(1));
    expect(put.mock.calls[0][0]).toBe("/skills/code-review");
    expect(put.mock.calls[0][1]).toEqual({ content: created, files: [] });
    expect(put.mock.calls[0][2]).toEqual(SCOPE);

    // 建完即选中：预览直接是刚写进去的那份。
    expect(await screen.findByText("正文")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("非法名称就地说明原因，不发请求", async () => {
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "＋ 新建技能" }));
    const dialog = within(await screen.findByRole("dialog"));
    fireEvent.change(dialog.getByLabelText("名称（必填）"), {
      target: { value: "Code_Review" },
    });
    fireEvent.change(dialog.getByLabelText("内容（SKILL.md）"), {
      target: { value: REVIEW_MD },
    });

    expect(
      dialog.getByText(/只有小写字母、数字和连字符可用/),
    ).toBeInTheDocument();
    expect(dialog.getByRole("button", { name: "创建" })).toBeDisabled();
    expect(put).not.toHaveBeenCalled();
  });

  it("编辑保存走同一个 PUT，成功后留在编辑器里", async () => {
    const next = "---\nname: review\ndescription: 代码审查\n---\n\n新正文\n";
    renderTab();

    await openEditor();
    const box = screen.getByLabelText("SKILL.md 内容");
    expect(box).toHaveValue(REVIEW_MD);

    fireEvent.change(box, { target: { value: next } });
    fireEvent.click(screen.getByRole("button", { name: "保存修改" }));

    await waitFor(() => expect(put).toHaveBeenCalledTimes(1));
    expect(put.mock.calls[0][1]).toEqual({ content: next, files: [] });
    expect(put.mock.calls[0][2]).toEqual(SCOPE);

    // 多文件编辑下保存不跳走，否则会打断其它缓冲。
    expect(screen.getByLabelText("SKILL.md 内容")).toHaveValue(next);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "保存修改" })).toBeDisabled(),
    );
  });

  it("保存被后端拒绝时把原文摆在编辑器上方，草稿不丢", async () => {
    put.mockRejectedValue(
      new ApiError("SKILL.md must start with '---'", { status: 400 }),
    );
    renderTab();

    await openEditor();
    fireEvent.change(screen.getByLabelText("SKILL.md 内容"), {
      target: { value: "没有 frontmatter" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存修改" }));

    expect(
      await screen.findByText(/SKILL\.md must start with '---'/),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("SKILL.md 内容")).toHaveValue(
      "没有 frontmatter",
    );
  });

  it("删除要过二次确认，并在确认框里列出引用了它的 agent", async () => {
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "删除 review" }));
    expect(del).not.toHaveBeenCalled();

    expect(
      await screen.findByText("删除后这些 agent 会缺能力："),
    ).toBeInTheDocument();
    expect(screen.getByText("助手")).toBeInTheDocument();
    expect(screen.queryByText("@master")).toBeNull();

    const confirm = screen.getByRole("button", { name: "确认删除" });
    await waitFor(() => expect(confirm).toBeEnabled());
    fireEvent.click(confirm);

    await waitFor(() =>
      expect(del).toHaveBeenCalledWith("/skills/review", SCOPE),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("删掉的技能从卡片里消失，剩下的那个点进去读的是它自己的正文", async () => {
    renderTab();

    fireEvent.click(
      await screen.findByRole("button", { name: "删除 translate" }),
    );
    const confirm = await screen.findByRole("button", { name: "确认删除" });
    await waitFor(() => expect(confirm).toBeEnabled());
    fireEvent.click(confirm);

    await waitFor(() =>
      expect(del).toHaveBeenCalledWith("/skills/translate", SCOPE),
    );
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: /^translate/ })).toBeNull(),
    );

    fireEvent.click(await screen.findByRole("button", { name: /^review/ }));
    expect(await screen.findByText("review 的正文")).toBeInTheDocument();
    expect(screen.queryByText("translate 的正文")).toBeNull();
  });

  it("删掉最后一个技能后只剩新建入口与空态文案", async () => {
    rows = [{ name: "review", description: "代码审查" }];
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "删除 review" }));
    const confirm = await screen.findByRole("button", { name: "确认删除" });
    await waitFor(() => expect(confirm).toBeEnabled());
    fireEvent.click(confirm);

    await waitFor(() =>
      expect(screen.queryByRole("button", { name: /^review/ })).toBeNull(),
    );
    expect(
      screen.getByRole("button", { name: "＋ 新建技能" }),
    ).toBeInTheDocument();
    expect(screen.getByText("这里什么也没有")).toBeInTheDocument();
  });

  it("取消删除不发请求", async () => {
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: "删除 review" }));
    await screen.findByText("删除后这些 agent 会缺能力：");
    fireEvent.click(screen.getByRole("button", { name: "取消" }));

    expect(screen.queryByRole("dialog")).toBeNull();
    expect(del).not.toHaveBeenCalled();
  });

  it("列表默认是卡片栅格，没点卡之前没有源码框", async () => {
    renderTab();

    const card = await screen.findByRole("button", { name: /^review/ });
    expect(screen.getByText("Skill")).toBeInTheDocument();
    expect(screen.getByText("2")).toBeInTheDocument();
    // 卡上只留名字，描述不再铺出来。
    expect(within(card).queryByText("代码审查")).toBeNull();
    expect(screen.queryByLabelText("SKILL.md 内容")).toBeNull();
    expect(screen.queryByRole("button", { name: "保存修改" })).toBeNull();
    expect(screen.queryByRole("button", { name: "返回列表" })).toBeNull();
  });

  it("点卡片进编辑器，点「返回列表」回到卡片栅格", async () => {
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: /^translate/ }));
    expect(
      await screen.findByRole("button", { name: "返回列表" }),
    ).toBeInTheDocument();
    expect(await screen.findByText("translate 的正文")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "返回列表" }));

    expect(
      await screen.findByRole("button", { name: /^translate/ }),
    ).toBeInTheDocument();
    expect(screen.queryByText("translate 的正文")).toBeNull();
    expect(screen.queryByRole("button", { name: "返回列表" })).toBeNull();
  });

  it("编辑时预览跟着草稿走：首屏给磁盘内容，改动后按防抖更新", async () => {
    renderTab();
    await openEditor();

    // 首屏不闪空：进编辑态的第一帧就是磁盘上的正文。
    expect(screen.getByText("review 的正文")).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("SKILL.md 内容"), {
      target: { value: "---\nname: review\n---\n\n改过的正文\n" },
    });

    expect(await screen.findByText("改过的正文")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText("review 的正文")).toBeNull());
    // 预览是草稿的投影，没发请求。
    expect(put).not.toHaveBeenCalled();
  });

  it("卡内删除不会顺带打开编辑器", async () => {
    renderTab();
    await screen.findByRole("button", { name: /^review/ });

    fireEvent.click(screen.getByRole("button", { name: "删除 review" }));

    expect(screen.queryByRole("button", { name: "返回列表" })).toBeNull();
    expect(screen.queryByLabelText("SKILL.md 内容")).toBeNull();
  });
});
