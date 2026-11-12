import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, waitFor } from "@testing-library/react";
import {
  useQuery,
  QueryClient,
  QueryClientProvider,
} from "@tanstack/react-query";
import { useTaskEvents } from "@/hooks/useTaskEvents";
import { SPACE_MASTER_AGENT } from "@/queries/spaceChat";
import type { TaskStreamHandlers } from "@/lib/events";
import type { TaskEvent } from "@/types/daemon";

// 台账事件是「哪一行变了」的通知：收到就该把受影响的面板都标脏。
// 这里只验失效清单 —— 重取本身是 react-query 的事。

const mocks = vi.hoisted(() => ({
  handlers: null as TaskStreamHandlers | null,
}));

vi.mock("@/lib/events", () => ({
  openTaskStream: (
    _wsId: string | null,
    handlers: TaskStreamHandlers,
  ): (() => void) => {
    mocks.handlers = handlers;
    return () => {};
  },
}));

const TASK_EVENT: TaskEvent = {
  ws_id: "ws_a",
  task_id: "t_1",
  task_state: "completed",
  wake_state: "delivered",
  wake_skip_reason: null,
  origin_kind: "dispatch",
  created_at: "2026-11-05 10:00:00",
  terminal_at: "2026-11-05 10:00:05",
};

/** 唤醒轮跑完的载荷：只有 ws_id 与 conversation_id 有意义，其余是空串或 null。 */
const WAKE_TURN_DONE_EVENT: TaskEvent = {
  kind: "wake_turn_done",
  conversation_id: "conv_1",
  dispatch_id: null,
  ws_id: "ws_a",
  task_id: "",
  task_state: "",
  wake_state: "awaiting",
  wake_skip_reason: null,
  origin_kind: "",
  created_at: "",
  terminal_at: null,
};

function Harness() {
  useTaskEvents();
  return null;
}

/** 按真实 queryKey 形状挂一条历史：只有键对得上，失效才会真的把它重取一遍。 */
function HistoryProbe({
  convId,
  fetchHistory,
}: {
  convId: string;
  fetchHistory: () => Promise<never[]>;
}) {
  useQuery({
    queryKey: ["messages", SPACE_MASTER_AGENT, convId],
    queryFn: fetchHistory,
  });
  return null;
}

function renderHarness(client: QueryClient, children?: React.ReactNode) {
  return render(
    <QueryClientProvider client={client}>
      <Harness />
      {children}
    </QueryClientProvider>,
  );
}

describe("useTaskEvents", () => {
  beforeEach(() => {
    mocks.handlers = null;
  });

  it("台账事件到达时失效文件树", () => {
    const client = new QueryClient();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    renderHarness(client);

    expect(mocks.handlers).not.toBeNull();
    act(() => {
      mocks.handlers?.onTask(TASK_EVENT);
    });

    expect(invalidate).toHaveBeenCalledWith({ queryKey: ["space-files"] });
  });

  it("唤醒轮跑完时重取该会话的历史", async () => {
    const client = new QueryClient();
    const fetchHistory = vi.fn(async () => []);
    renderHarness(
      client,
      <HistoryProbe convId="conv_1" fetchHistory={fetchHistory} />,
    );

    await waitFor(() => expect(fetchHistory).toHaveBeenCalledTimes(1));
    act(() => {
      mocks.handlers?.onTask(WAKE_TURN_DONE_EVENT);
    });

    await waitFor(() => expect(fetchHistory).toHaveBeenCalledTimes(2));
  });

  it.each([
    ["缺省", { ...TASK_EVENT }],
    ["显式 null", { ...TASK_EVENT, conversation_id: null }],
  ])("没有会话 id 的事件（%s）不碰消息缓存", (_label, event) => {
    const client = new QueryClient();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    renderHarness(client);

    act(() => {
      mocks.handlers?.onTask(event);
    });

    // 台账面板照旧失效：否则下面那句「消息没被失效」只是因为整条通路没跑起来。
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ["space-files"] });
    expect(
      invalidate.mock.calls.filter(
        ([filters]) => filters?.queryKey?.[0] === "messages",
      ),
    ).toEqual([]);
  });
});
