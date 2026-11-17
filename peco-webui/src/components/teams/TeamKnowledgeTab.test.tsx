import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TeamKnowledgeTab } from "@/components/teams/TeamKnowledgeTab";
import type { ResourceScope } from "@/lib/api";
import type { KnowledgeBase, TeamDetail } from "@/types/daemon";

// 只替换 axios 实例：POST 的 body 与空间头都在这里看得见，接口层照原样跑。
const get = vi.fn();
const post = vi.fn();

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
    api: {
      get: (url: string, config?: unknown) => get(url, config),
      post: (url: string, body: unknown, config?: unknown) =>
        post(url, body, config),
      put: () => Promise.reject(new Error("unexpected PUT")),
      delete: () => Promise.reject(new Error("unexpected DELETE")),
    },
  };
});

const SCOPE = { headers: { "X-Peco-Space": "ws_a" } };
/** 借道已打开空间：能力根与 `X-Peco-Space` 指向同一个团队。 */
const TEAM_SCOPE: ResourceScope = { teamId: "t1", wsId: "ws_a" };

const DETAIL = {
  id: "team_a",
  display_name: "Team A",
  template_version: 1,
  icon: "",
  root: "/tmp/team_a",
  has_master: true,
  agents: [],
  skills: [],
  mcp_servers: [],
  knowledge_bases: [],
  spaces: [],
} satisfies TeamDetail;

let rows: KnowledgeBase[];

function kb(name: string): KnowledgeBase {
  return {
    id: `kb_${name}`,
    name,
    description: "",
    backend: "lancedb",
    embedding_model: "bge-base-zh-v1.5",
    document_count: 0,
    chunk_count: 0,
    created_at: "",
    updated_at: "",
  };
}

function renderTab(scope: ResourceScope = TEAM_SCOPE) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <TeamKnowledgeTab scope={scope} detail={DETAIL} />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  rows = [];
  get.mockReset().mockImplementation(async (url: string) => {
    if (url === "/knowledge") return { data: rows };
    throw new Error(`unexpected GET ${url}`);
  });
  post.mockReset().mockImplementation(async (url: string, body: unknown) => {
    if (url !== "/knowledge") throw new Error(`unexpected POST ${url}`);
    const created = kb((body as { name: string }).name);
    rows = [...rows, created];
    return { data: created };
  });
});

describe("团队知识库 tab 新建", () => {
  it("没有已打开空间时按团队寻址：列表与建库都发 X-Peco-Team", async () => {
    const TEAM = { headers: { "X-Peco-Team": "t1" } };
    renderTab({ teamId: "t1", wsId: null });

    expect(await screen.findByText("这个团队还没有知识库")).toBeInTheDocument();
    expect(get).toHaveBeenCalledWith("/knowledge", TEAM);

    fireEvent.click(
      await screen.findByRole("button", { name: "＋ 新建知识库" }),
    );
    fireEvent.change(screen.getByLabelText("名称（必填）"), {
      target: { value: "handbook" },
    });
    fireEvent.click(screen.getByRole("button", { name: "创建" }));

    await waitFor(() => expect(post).toHaveBeenCalledTimes(1));
    expect(post.mock.calls[0][2]).toEqual(TEAM);
  });

  it("空态下也有新建入口，创建后卡片随即摆出来", async () => {
    renderTab();

    expect(await screen.findByText("这个团队还没有知识库")).toBeInTheDocument();

    fireEvent.click(
      await screen.findByRole("button", { name: "＋ 新建知识库" }),
    );
    fireEvent.change(screen.getByLabelText("名称（必填）"), {
      target: { value: "project-docs" },
    });
    fireEvent.change(screen.getByLabelText("描述"), {
      target: { value: "项目文档" },
    });
    fireEvent.click(screen.getByRole("button", { name: "创建" }));

    await waitFor(() => expect(post).toHaveBeenCalledTimes(1));
    expect(post.mock.calls[0][0]).toBe("/knowledge");
    expect(post.mock.calls[0][1]).toEqual({
      name: "project-docs",
      description: "项目文档",
    });
    expect(post.mock.calls[0][2]).toEqual(SCOPE);

    expect(await screen.findByText("project-docs")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText("新建知识库")).toBeNull());
  });

  it("名称不合规时就地提示，创建按钮不可点", async () => {
    renderTab();

    fireEvent.click(
      await screen.findByRole("button", { name: "＋ 新建知识库" }),
    );
    fireEvent.change(screen.getByLabelText("名称（必填）"), {
      target: { value: "-bad" },
    });

    expect(
      await screen.findByText(/需以字母或数字开头和结尾/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "创建" })).toBeDisabled();
    expect(post).not.toHaveBeenCalled();
  });
});
