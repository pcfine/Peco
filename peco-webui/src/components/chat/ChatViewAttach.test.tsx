import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { TooltipProvider } from "@/components/ui/tooltip";
import {
  CANCEL_ABORT_MS,
  ChatView,
  applyTurnSnapshot,
  type ChatMessage,
  type ChatViewProps,
} from "@/components/chat/ChatView";
import { toast } from "sonner";
import type { TurnSnapshotData } from "@/types/chat";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

// 「切走再回来」不是重发而是重新订阅：这里把订阅流握在测试手里，逐帧喂给
// ChatView，验证快照替换、seq 去重、done 的收尾顺序，以及「停止」改走 cancel。

const LIVE_URL = "/api/chat/%40master/conversations/conv_1/stream/live?ws=ws_a";
const CONV = "conv_1";

/** 一帧：与服务端同构的 `event:` / `id:` / `data:` + 分发空行。 */
function frame(event: string, id: number, data: unknown): string {
  return `event: ${event}\nid: ${id}\ndata: ${JSON.stringify(data)}\n\n`;
}

function snapshotFrame(
  seq: number,
  text: string,
  extra: Partial<TurnSnapshotData> = {},
): string {
  return frame("turn_snapshot", seq, {
    conversation_id: CONV,
    seq,
    phase: "running",
    text,
    reasoning: "",
    tool_calls: [],
    agent_calls: [],
    usage: { input_tokens: 3, output_tokens: 4 },
    ...extra,
  });
}

const doneFrame = (seq: number) =>
  frame("done", seq, {
    usage: { input_tokens: 3, output_tokens: 9 },
    conversation_id: CONV,
  });

/** 订阅流交给测试：`push` 继续发帧，`close` 提前结束（没发 done = 断在半路）。 */
function controlledStream() {
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const body = new ReadableStream<Uint8Array>({
    start(next) {
      controller = next;
    },
  });
  const encoder = new TextEncoder();
  return {
    response: new Response(body, { status: 200 }),
    push: async (text: string) => {
      await act(async () => {
        controller.enqueue(encoder.encode(text));
      });
    },
    close: async () => {
      await act(async () => {
        controller.close();
      });
    },
  };
}

const fetchMock = vi.fn<typeof fetch>();

function renderView(overrides: Partial<ChatViewProps> = {}) {
  return render(
    <TooltipProvider>
      <ChatView
        mode="internal"
        streamUrl={() => "/api/chat/%40master/conversations/conv_1/stream"}
        liveStreamUrl={() => LIVE_URL}
        historySettled
        initialMessages={[]}
        {...overrides}
      />
    </TooltipProvider>,
  );
}

beforeEach(() => {
  vi.stubGlobal("fetch", fetchMock);
  fetchMock.mockReset();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("重新订阅在途轮", () => {
  it("快照替换 + 增量追加 + 乱序帧丢弃：渲染 ABC，settle 恰好一次", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const onTurnSettled = vi.fn();
    const onAttachSettled = vi.fn();
    const { container } = renderView({ onTurnSettled, onAttachSettled });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(snapshotFrame(5, "AB"));
    await stream.push(
      frame("text_delta", 6, { content: "C", conversation_id: CONV }),
    );
    // seq 4 已被快照（seq=5）覆盖：这一帧必须丢弃，否则正文变成 ABCX
    await stream.push(
      frame("text_delta", 4, { content: "X", conversation_id: CONV }),
    );
    await stream.push(
      frame("turn_complete", 7, {
        text: "ABC",
        usage: { input_tokens: 3, output_tokens: 9 },
        conversation_id: CONV,
      }),
    );
    await stream.push(doneFrame(8));

    await waitFor(() => expect(onTurnSettled).toHaveBeenCalledTimes(1));
    expect(onAttachSettled).toHaveBeenCalledTimes(1);
    expect(container.textContent).toContain("ABC");
    expect(container.textContent).not.toContain("ABCX");
  });

  it("done 先把流式态关掉，再回调 onTurnSettled", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const timeline: string[] = [];
    renderView({
      onStreamingChange: (streaming) =>
        timeline.push(streaming ? "streaming:true" : "streaming:false"),
      onTurnSettled: () => timeline.push("settled"),
    });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(snapshotFrame(5, "AB"));
    await stream.push(doneFrame(6));

    await waitFor(() => expect(timeline).toContain("settled"));
    expect(timeline).toContain("streaming:true");
    // 「已不再是流式」必须是 settle 之前最后一次被观察到的状态
    expect(timeline[timeline.indexOf("settled") - 1]).toBe("streaming:false");
  });

  it("挂载即 attach；卸载只断本地连接，绝不把服务端那轮当已取消", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const cancelTurn = vi.fn().mockResolvedValue("accepted");
    const { unmount } = renderView({ cancelTurn });

    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(LIVE_URL, expect.anything()),
    );
    await stream.push(snapshotFrame(5, "AB"));

    unmount();

    expect(cancelTurn).not.toHaveBeenCalled();
    expect(
      fetchMock.mock.calls.some(([url]) => String(url).includes("/cancel")),
    ).toBe(false);
  });

  it("error{cancelled:true} 之后 done：打「已取消」标（不在断流那一刻提前打）", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const { container } = renderView();

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(snapshotFrame(5, "AB"));
    await stream.push(
      frame("error", 6, {
        message: "对话已被取消",
        cancelled: true,
        conversation_id: CONV,
      }),
    );
    await stream.push(
      frame("turn_complete", 7, {
        text: "AB",
        usage: { input_tokens: 3, output_tokens: 4 },
        conversation_id: CONV,
      }),
    );
    await stream.push(doneFrame(8));

    // 两处都要带上取消标：错误气泡（服务端下发的那一刻）与正文气泡（done 收尾时）
    await waitFor(() =>
      expect(
        container.querySelectorAll("[data-status='cancelled']"),
      ).toHaveLength(2),
    );
  });

  it("204（无在途轮）什么都不做：不新建气泡、不建连接", async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 204 }));
    const onAttachSettled = vi.fn();
    const onTurnSettled = vi.fn();
    const { container } = renderView({ onAttachSettled, onTurnSettled });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await act(async () => {});
    expect(container.querySelector("[data-slot='assistant-turn']")).toBeNull();
    expect(onAttachSettled).not.toHaveBeenCalled();
    expect(onTurnSettled).not.toHaveBeenCalled();
  });

  it("可见性回到 true 时重新订阅（切走再回来）", async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 204 }));
    const { rerender } = renderView({ visible: false });
    await act(async () => {});
    expect(fetchMock).not.toHaveBeenCalled();

    rerender(
      <TooltipProvider>
        <ChatView
          mode="internal"
          streamUrl={() => "/api/chat/%40master/conversations/conv_1/stream"}
          liveStreamUrl={() => LIVE_URL}
          historySettled
          initialMessages={[]}
          visible
        />
      </TooltipProvider>,
    );
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
  });

  it("没等到 done 就断流：按退避重走 attach", async () => {
    const first = controlledStream();
    fetchMock.mockResolvedValueOnce(first.response);
    const onTurnSettled = vi.fn();
    renderView({ onTurnSettled });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await first.push(snapshotFrame(5, "AB"));
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
    await first.close();

    // retryDelay(1) = 1000ms：等到第二次订阅（204 即停手，不再往下试）
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2), {
      timeout: 3000,
    });
    expect(onTurnSettled).not.toHaveBeenCalled();
  });

  it("重连次数用尽：摆出可点重试入口，点了重新订阅", async () => {
    vi.useFakeTimers();
    fetchMock.mockImplementation(
      async () => new Response(null, { status: 500 }),
    );
    const { getByRole, getByText } = renderView();

    // 第一次尝试 + MAX_RETRIES 次退避重试
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    for (let i = 0; i < 6; i += 1) {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(15_000);
      });
    }
    expect(fetchMock).toHaveBeenCalledTimes(7);
    expect(getByText(/没能接上这一轮/)).toBeInTheDocument();

    fetchMock.mockImplementation(
      async () => new Response(null, { status: 204 }),
    );
    await act(async () => {
      fireEvent.click(getByRole("button", { name: "重试" }));
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(fetchMock).toHaveBeenCalledTimes(8);
  });
});

describe("停止改走 cancel", () => {
  it("cancel 返回 200：保持连接等 done，不调 abort，按钮禁用并标 aria-busy", async () => {
    const abortSpy = vi.spyOn(AbortController.prototype, "abort");
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const cancelTurn = vi.fn().mockResolvedValue("accepted");
    renderView({ cancelTurn });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(snapshotFrame(5, "AB"));
    fireEvent.click(await screen.findByRole("button", { name: "停止" }));

    await waitFor(() => expect(cancelTurn).toHaveBeenCalledTimes(1));
    const busy = screen.getByRole("button", { name: "正在取消" });
    expect(busy).toBeDisabled();
    expect(busy).toHaveAttribute("aria-busy", "true");
    expect(abortSpy).not.toHaveBeenCalled();
  });

  it("cancel 返回 409 no_running_task：只补历史，不打「已取消」标、不 abort", async () => {
    const abortSpy = vi.spyOn(AbortController.prototype, "abort");
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const cancelTurn = vi.fn().mockResolvedValue("no_running_task");
    const onTurnSettled = vi.fn();
    const { container } = renderView({ cancelTurn, onTurnSettled });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(snapshotFrame(5, "AB"));
    fireEvent.click(await screen.findByRole("button", { name: "停止" }));

    await waitFor(() => expect(onTurnSettled).toHaveBeenCalledTimes(1));
    expect(abortSpy).not.toHaveBeenCalled();
    expect(container.querySelector("[data-status='cancelled']")).toBeNull();
  });

  it("cancel 命令投递后 10s 没等到 done：退化为断开本地连接", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const cancelTurn = vi.fn().mockResolvedValue("accepted");
    const onTurnSettled = vi.fn();
    renderView({ cancelTurn, onTurnSettled });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(snapshotFrame(5, "AB"));
    vi.useFakeTimers();
    fireEvent.click(screen.getByRole("button", { name: "停止" }));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(CANCEL_ABORT_MS);
    });

    expect(onTurnSettled).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: "停止" })).toBeNull();
  });
});

describe("发送遇到在途轮（409 conversation_busy）", () => {
  it("提示「这一轮还在跑」，输入原样还回去、幽灵气泡撤回", async () => {
    fetchMock.mockResolvedValue(
      new Response(
        JSON.stringify({ code: "conversation_busy", error: "busy" }),
        {
          status: 409,
        },
      ),
    );
    // 本用例只看发送路径：不接订阅，免得两条 fetch 抢同一个假响应
    const { container } = renderView({ liveStreamUrl: undefined });

    const textbox = screen.getByRole("textbox");
    fireEvent.change(textbox, { target: { value: "再跑一次" } });
    fireEvent.keyDown(textbox, { key: "Enter" });

    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith("这一轮还在跑，先停止再发"),
    );
    // 不清空输入框：用户不必重打一遍
    expect(screen.getByRole("textbox")).toHaveValue("再跑一次");
    // 乐观插入的 user 气泡要撤掉，否则界面上多出一句「说过了」的话
    expect(container.querySelector("[data-slot='user-bubble']")).toBeNull();
  });
});

describe("applyTurnSnapshot", () => {
  const snapshot = (
    extra: Partial<TurnSnapshotData> = {},
  ): TurnSnapshotData => ({
    conversation_id: CONV,
    seq: 5,
    phase: "running",
    text: "新内容",
    reasoning: "",
    tool_calls: [],
    agent_calls: [],
    usage: { input_tokens: 0, output_tokens: 0 },
    ...extra,
  });

  it("没有在途气泡时新建，不覆盖历史里上一轮的回答", () => {
    const history: ChatMessage[] = [
      { id: "turn-0-0", role: "user", content: "上一问", turnIndex: 0 },
      { id: "turn-0-1", role: "assistant", content: "上一答", turnIndex: 0 },
    ];
    const next = applyTurnSnapshot(history, snapshot());
    expect(next).toHaveLength(3);
    expect(next[1].content).toBe("上一答");
    expect(next[2].content).toBe("新内容");
  });

  it("有在途气泡时替换（工具卡与推理一并按快照重建）", () => {
    const live: ChatMessage[] = [
      { id: "h1", role: "user", content: "问", turnIndex: 0 },
      { role: "assistant", content: "旧", reasoning: "旧推理", turnIndex: 0 },
    ];
    const next = applyTurnSnapshot(
      live,
      snapshot({
        reasoning: "新推理",
        tool_calls: [
          {
            id: "t1",
            name: "read_file",
            arguments: "{}",
            status: "done",
            result: "ok",
          },
        ],
      }),
    );
    expect(next).toHaveLength(2);
    expect(next[1].content).toBe("新内容");
    expect(next[1].reasoning).toBe("新推理");
    expect(next[1].toolCalls?.[0]).toMatchObject({ id: "t1", status: "done" });
  });
});
