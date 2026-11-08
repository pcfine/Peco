import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { SpaceLiveTasksTab } from "@/components/spaces/SpaceLiveTasksTab";
import { ApiError } from "@/lib/api";
import type { LiveTaskView, Task } from "@/types/daemon";

// 实时面板的两条数据路：TaskHub 快照（实时）与台账（摘要回落）。
// agent 与 prompt 只能从台账行补，故两个 mock 必须同时在场。

const mocks = vi.hoisted(() => ({
  live: vi.fn<() => Promise<LiveTaskView[]>>(async () => []),
  tasks: vi.fn<() => Promise<Task[]>>(async () => []),
  cancel: vi.fn<
    (wsId: string, taskId: string) => Promise<{ cancelled: boolean }>
  >(async () => ({ cancelled: true })),
}));

vi.mock("@/api/spaces", () => ({
  spaceLiveTasks: () => mocks.live(),
  spaceTasks: () => mocks.tasks(),
  cancelSpaceTask: (wsId: string, taskId: string) => mocks.cancel(wsId, taskId),
}));

function liveTask(overrides: Partial<LiveTaskView> = {}): LiveTaskView {
  return {
    id: "t_1",
    kind: "echo",
    state: "working",
    state_detail: null,
    progress: { phase: null, current: 3, total: 10 },
    output: {
      tail: Array.from(new TextEncoder().encode("先跑完这一段")),
      start_offset: 0,
      end_offset: 18,
    },
    outcome: null,
    artifacts: [],
    created_at: "2026-11-05T10:00:00Z",
    updated_at: "2026-11-05T10:00:20Z",
    ...overrides,
  };
}

function ledgerTask(overrides: Partial<Task> = {}): Task {
  return {
    id: "d_t_1",
    task_id: "t_1",
    target_ws_id: "ws_a",
    target_work_dir: "/tmp/spaces/ws_a/work",
    target_team_id: "research",
    origin_ws_id: "ws_peco",
    origin_kind: "dispatch",
    agent_name: "coder",
    task_state: "working",
    wake_state: "delivered",
    wake_skip_reason: null,
    created_at: "2026-11-05 10:00:00",
    terminal_at: null,
    result_digest: null,
    prompt_digest: "把合同条款整理成表",
    ...overrides,
  };
}

function renderTab() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <SpaceLiveTasksTab wsId="ws_a" />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  mocks.live.mockReset();
  mocks.tasks.mockReset();
  mocks.cancel.mockReset();
  mocks.live.mockResolvedValue([]);
  mocks.tasks.mockResolvedValue([]);
  mocks.cancel.mockResolvedValue({ cancelled: true });
});

describe("实时任务面板", () => {
  it("标注实时，在飞任务带取消按钮，已完成的不带", async () => {
    mocks.live.mockResolvedValue([
      liveTask(),
      liveTask({
        id: "t_2",
        state: "completed",
        output: { tail: [], start_offset: 0, end_offset: 0 },
      }),
    ]);
    mocks.tasks.mockResolvedValue([
      ledgerTask(),
      ledgerTask({
        id: "d_t_2",
        task_id: "t_2",
        agent_name: "writer",
        prompt_digest: "把周报汇总",
      }),
    ]);
    renderTab();

    expect(await screen.findByTestId("task-fidelity")).toHaveTextContent(
      "实时",
    );
    // 实时行显示 prompt 摘要与 agent，不是只给一个 UUID。
    expect(screen.getByText("把合同条款整理成表")).toBeInTheDocument();
    expect(screen.getByText("coder")).toBeInTheDocument();
    expect(screen.getByText("writer")).toBeInTheDocument();

    expect(
      screen.getByRole("button", { name: "取消任务：把合同条款整理成表" }),
    ).toBeEnabled();
    expect(screen.getAllByRole("button", { name: /取消任务/ })).toHaveLength(1);
  });

  it("空间未打开（409）时回落台账并标注摘要", async () => {
    mocks.live.mockRejectedValue(
      new ApiError("空间还没打开", { status: 409, code: "space_not_open" }),
    );
    mocks.tasks.mockResolvedValue([ledgerTask({ task_state: "completed" })]);
    renderTab();

    const note = await screen.findByTestId("task-fidelity");
    expect(note).toHaveTextContent("摘要");
    expect(note).toHaveAttribute("data-fidelity", "digest");

    // 台账的展开区带 prompt 摘要；折叠行只给结果摘要，故按 agent 点开。
    fireEvent.click(screen.getByRole("button", { name: /coder/ }));
    expect(screen.getByText("Peco 让它做什么")).toBeInTheDocument();
  });

  it("空间重启后实时快照为空、台账有行：回落台账并标注摘要", async () => {
    // daemon 重启后空间自动恢复打开，Hub 里没有任何任务，历史只在台账里。
    mocks.live.mockResolvedValue([]);
    mocks.tasks.mockResolvedValue([
      ledgerTask({
        id: "d_1",
        task_id: "t_1",
        task_state: "completed",
        result_digest: "合同条款整理完了",
      }),
      ledgerTask({
        id: "d_2",
        task_id: "t_2",
        agent_name: "writer",
        task_state: "completed",
        result_digest: "周报汇总完了",
      }),
      ledgerTask({
        id: "d_3",
        task_id: "t_3",
        agent_name: "tester",
        task_state: "failed",
        result_digest: "用例跑完了",
      }),
      ledgerTask({
        id: "d_4",
        task_id: "t_4",
        agent_name: "reviewer",
        task_state: "completed",
        result_digest: "评审意见写完了",
      }),
    ]);
    const { container } = renderTab();

    const note = await screen.findByTestId("task-fidelity");
    expect(note).toHaveAttribute("data-fidelity", "digest");
    expect(note).toHaveTextContent("摘要");

    // 4 行台账一行都不能少，也不能退化成空态。
    expect(container.querySelectorAll("ul > li")).toHaveLength(4);
    expect(screen.getByText("评审意见写完了")).toBeInTheDocument();
    expect(screen.queryByText(/还没有任务/)).toBeNull();
  });

  it("实时快照与台账都为空时才显示空态，且不标摘要", async () => {
    mocks.live.mockResolvedValue([]);
    mocks.tasks.mockResolvedValue([]);
    renderTab();

    expect(await screen.findByText(/还没有任务/)).toBeInTheDocument();
    // 没有摘要可标时挂「摘要」，会让人以为结果被截断。
    expect(screen.queryByTestId("task-fidelity")).toBeNull();
  });

  it("轮询撞上 409（空间刚被关掉）时，不再拿上一份快照当实时", async () => {
    // 关空间发生在别的页面，本面板下一次轮询就会 409：此时 `data` 里还是关之前的
    // 快照，必须让 409 优先，否则面板会一直声称「实时」并显示已经没有的进度。
    mocks.live.mockResolvedValueOnce([liveTask()]);
    mocks.tasks.mockResolvedValue([ledgerTask({ task_state: "completed" })]);
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    render(
      <QueryClientProvider client={client}>
        <SpaceLiveTasksTab wsId="ws_a" />
      </QueryClientProvider>,
    );
    expect(await screen.findByText("把合同条款整理成表")).toBeInTheDocument();
    expect(screen.getByTestId("task-fidelity")).toHaveAttribute(
      "data-fidelity",
      "live",
    );

    mocks.live.mockRejectedValue(
      new ApiError("空间还没打开", { status: 409, code: "space_not_open" }),
    );
    await act(() =>
      client.refetchQueries({ queryKey: ["space-live-tasks", "ws_a"] }),
    );

    await waitFor(() =>
      expect(screen.getByTestId("task-fidelity")).toHaveAttribute(
        "data-fidelity",
        "digest",
      ),
    );
    // 旧的实时行（含那条 prompt 摘要）必须已经消失。
    expect(screen.queryByText("把合同条款整理成表")).toBeNull();
  });

  it("实时快照有行时只渲染实时行，不把台账里多出来的行混进来", async () => {
    mocks.live.mockResolvedValue([liveTask()]);
    mocks.tasks.mockResolvedValue([
      ledgerTask(),
      ledgerTask({
        id: "d_t_9",
        task_id: "t_9",
        agent_name: "historian",
        prompt_digest: "只在台账里的旧任务",
      }),
    ]);
    const { container } = renderTab();

    expect(await screen.findByTestId("task-fidelity")).toHaveAttribute(
      "data-fidelity",
      "live",
    );
    // 等到台账落地（实时行因此补上了 prompt 摘要）再数行数，否则这一断言会空过。
    expect(await screen.findByText("把合同条款整理成表")).toBeInTheDocument();
    expect(container.querySelectorAll("ul > li")).toHaveLength(1);
    expect(screen.queryByText("只在台账里的旧任务")).toBeNull();
    expect(screen.queryByText("historian")).toBeNull();
  });

  it("其他错误给重试，不回落到台账冒充成功", async () => {
    mocks.live.mockRejectedValue(
      new ApiError("连不上 daemon", { status: 500 }),
    );
    mocks.tasks.mockResolvedValue([ledgerTask()]);
    renderTab();

    expect(await screen.findByText("连不上 daemon")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
    expect(screen.queryByTestId("task-fidelity")).toBeNull();
  });

  it("台账行没有 prompt_digest 时显示未记录", async () => {
    mocks.live.mockRejectedValue(
      new ApiError("空间还没打开", { status: 409, code: "space_not_open" }),
    );
    mocks.tasks.mockResolvedValue([ledgerTask({ prompt_digest: null })]);
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: /coder/ }));
    expect(screen.getByText("Peco 让它做什么")).toBeInTheDocument();
    // 老行没有 prompt：展开区写「未记录」，而不是留白或拿 task_id 顶替。
    expect(screen.getByText("未记录")).toBeInTheDocument();
  });

  it("取消打这个空间的取消端点，请求期间按钮禁用", async () => {
    mocks.live.mockResolvedValue([liveTask()]);
    mocks.tasks.mockResolvedValue([ledgerTask()]);
    let release: (() => void) | undefined;
    mocks.cancel.mockImplementation(
      () =>
        new Promise((resolve) => {
          release = () => resolve({ cancelled: true });
        }),
    );
    renderTab();

    const cancelButton = await screen.findByRole("button", {
      name: "取消任务：把合同条款整理成表",
    });
    fireEvent.click(cancelButton);

    await waitFor(() =>
      expect(mocks.cancel).toHaveBeenCalledWith("ws_a", "t_1"),
    );
    expect(cancelButton).toBeDisabled();
    expect(cancelButton).toHaveAttribute("aria-busy", "true");

    release?.();
    await waitFor(() => expect(cancelButton).toBeEnabled());
  });

  it("展开行看输出尾巴、产物数量；终态看完整结果", async () => {
    mocks.live.mockResolvedValue([
      liveTask({
        artifacts: [
          {
            id: "a_1",
            kind: "file",
            offset: 0,
            append: true,
            last_chunk: true,
            payload: { tag: "file", payload: {} },
          },
        ],
      }),
      liveTask({
        id: "t_2",
        state: "completed",
        output: { tail: [], start_offset: 0, end_offset: 0 },
        outcome: {
          output: { tag: "echo", payload: "整理完了，共 12 行。" },
          error: null,
        },
      }),
    ]);
    mocks.tasks.mockResolvedValue([
      ledgerTask(),
      ledgerTask({
        id: "d_t_2",
        task_id: "t_2",
        agent_name: "writer",
        prompt_digest: "把周报汇总",
      }),
    ]);
    renderTab();

    fireEvent.click(await screen.findByRole("button", { name: /working/ }));
    expect(screen.getByText("先跑完这一段")).toBeInTheDocument();
    expect(screen.getByTestId("live-task-artifacts")).toHaveTextContent(
      "产物 1 个",
    );

    fireEvent.click(screen.getByRole("button", { name: /completed/ }));
    expect(screen.getByText("结果输出")).toBeInTheDocument();
    expect(screen.getByText("整理完了，共 12 行。")).toBeInTheDocument();
  });
});
