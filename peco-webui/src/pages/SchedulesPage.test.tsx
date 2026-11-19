import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { SchedulesPage } from "@/pages/SchedulesPage";
import { SCHEDULES_EMPTY_HINT } from "@/components/schedules/ScheduleList";
import { toast } from "sonner";
import { useSpaceStore } from "@/stores/spaceStore";
import { formatCreatedAt } from "@/lib/format";
// 走被 mock 的同一个模块：`instanceof` 判据要求与组件拿到的是同一个类。
import { ApiError } from "@/lib/api";
import type { ScheduleRun, ScheduleView, Space } from "@/types/daemon";

// 只替换 axios 实例：请求体、路径与空间头都在这里看得见，接口层与查询层照原样跑。
const get = vi.fn();
const post = vi.fn();
const patch = vi.fn();
const del = vi.fn();

vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
    api: {
      get: (url: string, config?: unknown) => get(url, config),
      post: (url: string, body?: unknown, config?: unknown) =>
        post(url, body, config),
      patch: (url: string, body?: unknown, config?: unknown) =>
        patch(url, body, config),
      delete: (url: string, config?: unknown) => del(url, config),
    },
  };
});

const SPACE: Space = {
  ws_id: "ws_a",
  work_dir: "/tmp/spaces/a/work",
  team_id: "research",
  label: "空间 A",
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
};

const NEXT_RUN = "2026-11-06 01:00:00";

function schedule(overrides: Partial<ScheduleView> = {}): ScheduleView {
  return {
    id: "s1",
    name: "每日早报",
    target_ws_id: "ws_a",
    target_label: "空间 A",
    prompt: "把昨天的进展整理成一段话",
    cron_expr: "0 9 * * *",
    enabled: true,
    next_run_at: NEXT_RUN,
    last_run_at: "2026-11-05 01:00:00",
    last_status: "completed",
    created_at: "2026-11-01 00:00:00",
    updated_at: "2026-11-01 00:00:00",
    ...overrides,
  };
}

function run(overrides: Partial<ScheduleRun> = {}): ScheduleRun {
  return {
    id: "r1",
    schedule_id: "s1",
    trigger_kind: "cron",
    scheduled_for: "2026-11-05 01:00:00",
    status: "completed",
    attempts: 1,
    conversation_id: "conv_a",
    started_at: "2026-11-05 01:00:03",
    finished_at: "2026-11-05 01:02:03",
    result_digest: "三份进展已归档",
    error: null,
    notify_state: "sent",
    notified_at: "2026-11-05 01:02:04",
    ...overrides,
  };
}

let rows: ScheduleView[];
let runs: ScheduleRun[];

const previewCalls = () =>
  post.mock.calls.filter(([url]) => url === "/schedules/preview");

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={["/schedules"]}>
        <SchedulesPage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

/** Radix 的菜单在 jsdom 里用键盘开：指针事件要 `pointerType`，jsdom 给不出。 */
async function openRowMenu(name: string) {
  const trigger = await screen.findByRole("button", {
    name: `${name} 的操作`,
  });
  fireEvent.keyDown(trigger, { key: "Enter" });
  return screen.findByRole("menu");
}

async function openCreateDialog() {
  fireEvent.click(await screen.findByRole("button", { name: "新建定时任务" }));
  return screen.findByRole("dialog");
}

beforeEach(() => {
  rows = [];
  runs = [];
  useSpaceStore.setState({ currentWsId: "ws_a" });
  vi.mocked(toast.success).mockClear();
  vi.mocked(toast.error).mockClear();

  get.mockReset().mockImplementation(async (url: string) => {
    if (url === "/spaces") return { data: [SPACE] };
    if (url === "/schedules") return { data: rows };
    if (url === "/schedules/s1/runs") return { data: runs };
    throw new Error(`unexpected GET ${url}`);
  });

  post.mockReset().mockImplementation(async (url: string) => {
    if (url === "/schedules/preview")
      return { data: { next: [NEXT_RUN, "2026-11-07 01:00:00"] } };
    if (url === "/schedules") return { data: schedule({ id: "s_new" }) };
    throw new Error(`unexpected POST ${url}`);
  });

  patch.mockReset().mockImplementation(async (url: string) => {
    if (url !== "/schedules/s1") throw new Error(`unexpected PATCH ${url}`);
    return { data: schedule({ enabled: false }) };
  });

  del.mockReset().mockImplementation(async (url: string) => {
    if (url !== "/schedules/s1") throw new Error(`unexpected DELETE ${url}`);
    return { data: { removed: true } };
  });
});

describe("定时任务页的空态", () => {
  it("说清定时任务是什么，右上保留新建入口", async () => {
    renderPage();

    expect(await screen.findByText(SCHEDULES_EMPTY_HINT)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "新建定时任务" }),
    ).toBeInTheDocument();
    // 空态不摆点不动的控件：没有任务就没有开关可拨。
    expect(screen.queryAllByRole("switch")).toHaveLength(0);
  });
});

describe("定时任务列表行", () => {
  it("给出名称、目标空间、cron 原文与人读、下次运行；停用的行写「已停用」", async () => {
    rows = [
      schedule(),
      schedule({
        id: "s2",
        name: "周报",
        cron_expr: "0 9 * * 1",
        enabled: false,
        next_run_at: null,
        last_status: null,
      }),
    ];
    renderPage();

    expect(await screen.findByText("每日早报")).toBeInTheDocument();
    expect(screen.getByText("0 9 * * *")).toBeInTheDocument();
    expect(screen.getByText("每天 9:00")).toBeInTheDocument();
    expect(screen.getByText("每周一 9:00")).toBeInTheDocument();
    expect(screen.getAllByText("空间 A")).toHaveLength(2);

    // 绝对时间在 title 上，按本机时区呈现；相对时间随运行时刻变，不写死。
    expect(screen.getByTitle(formatCreatedAt(NEXT_RUN))).toBeInTheDocument();
    expect(screen.getAllByText("已停用")).toHaveLength(1);
    expect(screen.getByText("未运行")).toBeInTheDocument();
    expect(screen.getByText("已完成")).toBeInTheDocument();
  });

  it("行内开关即时 PATCH，只带被改的那一个字段", async () => {
    rows = [schedule()];
    renderPage();

    fireEvent.click(
      await screen.findByRole("switch", { name: "启用定时任务 每日早报" }),
    );

    await waitFor(() =>
      expect(patch).toHaveBeenCalledWith(
        "/schedules/s1",
        { enabled: false },
        undefined,
      ),
    );
  });

  it("删除要二次确认，文案写明历史记录一并删除", async () => {
    rows = [schedule()];
    renderPage();

    const menu = await openRowMenu("每日早报");
    fireEvent.click(
      within(menu).getByRole("menuitem", { name: "删除定时任务" }),
    );

    const dialog = await screen.findByRole("dialog");
    expect(
      within(dialog).getByText(/运行历史记录一并删除/),
    ).toBeInTheDocument();
    fireEvent.click(
      within(dialog).getByRole("button", { name: "删除定时任务" }),
    );
    await waitFor(() =>
      expect(del).toHaveBeenCalledWith("/schedules/s1", undefined),
    );
  });
});

describe("运行历史", () => {
  it("展开后按状态说话：pending 报第几次重试，completed 给「去看这一轮」", async () => {
    rows = [schedule()];
    runs = [
      run({
        id: "r_pending",
        status: "pending",
        attempts: 2,
        finished_at: null,
        result_digest: null,
      }),
      run({ id: "r_done" }),
    ];
    renderPage();

    const menu = await openRowMenu("每日早报");
    fireEvent.click(within(menu).getByRole("menuitem", { name: "运行历史" }));

    expect(
      await screen.findByText("等待重试（第 2/10 次，目标会话忙）"),
    ).toBeInTheDocument();
    expect(screen.getByText("三份进展已归档")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "去看这一轮" })).toHaveAttribute(
      "href",
      "/spaces/ws_a",
    );
    expect(get).toHaveBeenCalledWith("/schedules/s1/runs", {
      params: { limit: 20 },
    });
  });

  it("跳过成因翻成人话，不把后端的标识符原样丢给用户", async () => {
    rows = [schedule()];
    runs = [
      run({
        id: "r_prev",
        status: "skipped",
        result_digest: null,
        error: "previous_running",
        notify_state: "none",
        notified_at: null,
      }),
      run({
        id: "r_missed",
        status: "skipped",
        result_digest: null,
        error: "missed",
        notify_state: "none",
        notified_at: null,
      }),
    ];
    renderPage();

    const menu = await openRowMenu("每日早报");
    fireEvent.click(within(menu).getByRole("menuitem", { name: "运行历史" }));

    expect(
      await screen.findByText("上一次运行还没结束，本轮跳过"),
    ).toBeInTheDocument();
    expect(screen.getByText("错过触发窗口，本轮未执行")).toBeInTheDocument();
    expect(screen.queryByText(/previous_running/)).not.toBeInTheDocument();
    expect(screen.queryByText(/missed/)).not.toBeInTheDocument();
  });
});

describe("立即运行", () => {
  it("启动失败时报失败，不谎报「已开始运行」", async () => {
    rows = [schedule()];
    post.mockImplementation(async (url: string) => {
      if (url === "/schedules/s1/run")
        return { data: { run_id: "r1", status: "failed" } };
      throw new Error(`unexpected POST ${url}`);
    });
    renderPage();

    const menu = await openRowMenu("每日早报");
    fireEvent.click(within(menu).getByRole("menuitem", { name: "立即运行" }));

    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith("启动失败，打开运行历史看原因"),
    );
    expect(toast.success).not.toHaveBeenCalledWith("已开始运行");
  });
});

describe("新建表单", () => {
  it("提交 POST /schedules，body 带四个字段与启用开关，目标空间默认取当前空间", async () => {
    renderPage();
    const dialog = await openCreateDialog();

    expect(
      within(dialog).getByRole("combobox", { name: "目标空间" }),
    ).toHaveTextContent("空间 A");
    fireEvent.change(within(dialog).getByLabelText("名称（必填）"), {
      target: { value: "  早报  " },
    });
    fireEvent.change(within(dialog).getByLabelText("任务内容（必填）"), {
      target: { value: "整理昨天的进展" },
    });
    fireEvent.change(within(dialog).getByLabelText(/执行计划/), {
      target: { value: "0 9 * * 1" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "新建" }));

    await waitFor(() => expect(post).toHaveBeenCalled());
    expect(post.mock.calls.filter(([url]) => url === "/schedules")).toEqual([
      [
        "/schedules",
        {
          name: "早报",
          target_ws_id: "ws_a",
          prompt: "整理昨天的进展",
          cron_expr: "0 9 * * 1",
          enabled: true,
        },
        undefined,
      ],
    ]);
  });

  it("服务端 invalid_cron 的 details 原样落在执行计划字段下方", async () => {
    const details = "第 5 段（周）超出范围：1-7 之外的值";
    post.mockImplementation(async (url: string) => {
      if (url === "/schedules/preview") return { data: { next: [NEXT_RUN] } };
      throw new ApiError(details, { status: 400, code: "invalid_cron" });
    });
    renderPage();
    const dialog = await openCreateDialog();

    fireEvent.change(within(dialog).getByLabelText("名称（必填）"), {
      target: { value: "早报" },
    });
    fireEvent.change(within(dialog).getByLabelText("任务内容（必填）"), {
      target: { value: "整理昨天的进展" },
    });
    fireEvent.change(within(dialog).getByLabelText(/执行计划/), {
      target: { value: "0 9 * * 9" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "新建" }));

    expect(await within(dialog).findByText(details)).toBeInTheDocument();
    expect(within(dialog).getByLabelText(/执行计划/)).toHaveAttribute(
      "aria-invalid",
      "true",
    );
    // 错归在哪个字段，焦点就在哪儿：留在提交按钮上等于让用户自己找。
    expect(within(dialog).getByLabelText(/执行计划/)).toHaveFocus();
  });

  it("执行计划停手 300ms 才问一次 preview，逐键输入只发一条", async () => {
    renderPage();
    const dialog = await openCreateDialog();
    const cron = within(dialog).getByLabelText(/执行计划/);

    vi.useFakeTimers();
    try {
      fireEvent.change(cron, { target: { value: "0" } });
      fireEvent.change(cron, { target: { value: "0 9" } });
      fireEvent.change(cron, { target: { value: "0 9 * * *" } });
      expect(previewCalls()).toHaveLength(0);

      await act(async () => {
        vi.advanceTimersByTime(299);
      });
      expect(previewCalls()).toHaveLength(0);

      await act(async () => {
        vi.advanceTimersByTime(1);
      });
      expect(previewCalls()).toHaveLength(1);
      expect(previewCalls()[0][1]).toEqual({ cron_expr: "0 9 * * *" });
    } finally {
      vi.useRealTimers();
    }

    // 预览结果按本机时区摊开，最多 3 条。
    expect(
      await within(dialog).findByText(formatCreatedAt(NEXT_RUN)),
    ).toBeInTheDocument();
    expect(
      within(dialog).getByText(/下次 2 次运行（本机时区）/),
    ).toBeInTheDocument();
  });
});
