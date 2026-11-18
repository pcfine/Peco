import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { NewSpaceForm } from "@/components/spaces/NewSpaceForm";

// 「显示名称」写 marker 的 `name`，不是库里的 `label`：请求体退回 `label` 就等于
// 用户填的名字只进台账，界面仍按目录末段称呼这个空间。

const createSpace = vi.fn(async (body: unknown) => {
  void body;
  return {
    ws_id: "ws_new",
    work_dir: "/tmp/new",
    team_id: "research",
    agents: [],
  };
});
const createTeam = vi.fn(async () => ({
  team_id: "legal",
  display_name: "法务",
  agent_count: 0,
  skill_count: 0,
  mcp_count: 0,
  space_count: 0,
  reserved: false,
}));

vi.mock("@/api/spaces", () => ({
  listSpaces: async () => [],
  createSpace: (body: unknown) => createSpace(body),
}));

vi.mock("@/api/teams", () => ({
  listTeams: async () => [
    {
      team_id: "research",
      display_name: "研究",
      agent_count: 0,
      skill_count: 0,
      mcp_count: 0,
      space_count: 0,
      reserved: false,
    },
  ],
  createTeam: () => createTeam(),
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));

function renderForm(onCreated = vi.fn()) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <NewSpaceForm lockedTeamId="research" onCreated={onCreated} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return onCreated;
}

/** 填目录与显示名称再提交。 */
async function submitWith({
  workDir,
  name,
}: {
  workDir: string;
  name: string;
}) {
  fireEvent.change(screen.getByLabelText("工作目录"), {
    target: { value: workDir },
  });
  fireEvent.change(screen.getByLabelText("显示名称"), {
    target: { value: name },
  });
  fireEvent.click(screen.getByRole("button", { name: "创建并打开" }));
}

describe("新建空间表单", () => {
  it("「显示名称」提交为 name，不带 label", async () => {
    const onCreated = renderForm();
    await submitWith({ workDir: "/tmp/new", name: " 客户合同审阅 " });

    await waitFor(() => expect(onCreated).toHaveBeenCalledWith("ws_new"));
    const body = createSpace.mock.lastCall?.[0] as Record<string, unknown>;
    expect(body).toMatchObject({
      work_dir: "/tmp/new",
      team_id: "research",
      name: "客户合同审阅",
    });
    // 两个概念不能混：提交体里不该出现 label。
    expect(body).not.toHaveProperty("label");
  });

  it("显示名留空时 name 缺省，不写空串", async () => {
    const onCreated = renderForm();
    await submitWith({ workDir: "/tmp/new", name: "   " });

    await waitFor(() => expect(onCreated).toHaveBeenCalledWith("ws_new"));
    const body = createSpace.mock.lastCall?.[0] as Record<string, unknown>;
    expect(body.name).toBeUndefined();
    expect(body).not.toHaveProperty("label");
  });
});
