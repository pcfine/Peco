import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useTaskEvents } from "@/hooks/useTaskEvents";
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

function Harness() {
  useTaskEvents();
  return null;
}

describe("useTaskEvents", () => {
  beforeEach(() => {
    mocks.handlers = null;
  });

  it("台账事件到达时失效文件树", () => {
    const client = new QueryClient();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    render(
      <QueryClientProvider client={client}>
        <Harness />
      </QueryClientProvider>,
    );

    expect(mocks.handlers).not.toBeNull();
    act(() => {
      mocks.handlers?.onTask(TASK_EVENT);
    });

    expect(invalidate).toHaveBeenCalledWith({ queryKey: ["space-files"] });
  });
});
