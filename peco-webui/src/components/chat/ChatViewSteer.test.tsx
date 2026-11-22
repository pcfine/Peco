import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { TooltipProvider } from "@/components/ui/tooltip";
import { ChatView, type ChatViewProps } from "@/components/chat/ChatView";
import { toast } from "sonner";
import type { TurnSnapshotData } from "@/types/chat";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

// 在途轮的注入通路：输入不再另开一条流抢在途 run，而是排进当前轮的 pending
// 队列。流都由测试手里推，才分得清「另开了流」与「只是投递了」。
const CONV = "conv_1";
const LIVE_URL = "/api/chat/%40master/conversations/conv_1/stream/live";

function frame(event: string, id: number, data: unknown): string {
  return `event: ${event}\nid: ${id}\ndata: ${JSON.stringify(data)}\n\n`;
}

function snapshotFrame(seq: number, extra: Partial<TurnSnapshotData> = {}) {
  return frame("turn_snapshot", seq, {
    conversation_id: CONV,
    seq,
    phase: "running",
    text: "",
    reasoning: "",
    tool_calls: [],
    agent_calls: [],
    usage: { input_tokens: 1, output_tokens: 2 },
    ...extra,
  });
}

const deltaFrame = (id: number, content: string) =>
  frame("text_delta", id, { content, conversation_id: CONV });

const steerFrame = (id: number, count: number, text: string) =>
  frame("user_steer", id, { count, text, conversation_id: CONV });

const doneFrame = (id: number) =>
  frame("done", id, {
    usage: { input_tokens: 1, output_tokens: 2 },
    conversation_id: CONV,
  });

/** 交给测试推进的响应流：`push` 继续发帧。 */
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
  };
}

const fetchMock = vi.fn<typeof fetch>();

function renderView(overrides: Partial<ChatViewProps> = {}) {
  return render(
    <TooltipProvider>
      <ChatView
        mode="internal"
        streamUrl={() => "/api/chat/%40master/conversations/conv_1/stream"}
        initialMessages={[]}
        {...overrides}
      />
    </TooltipProvider>,
  );
}

/** 输入一句并以回车发出。 */
function send(text: string) {
  const textbox = screen.getByRole("textbox");
  fireEvent.change(textbox, { target: { value: text } });
  fireEvent.keyDown(textbox, { key: "Enter" });
}

function queuedBlock(container: HTMLElement): Element | null {
  return container.querySelector("[data-slot='queued-inputs']");
}

beforeEach(() => {
  vi.stubGlobal("fetch", fetchMock);
  fetchMock.mockReset();
  vi.mocked(toast.error).mockClear();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("在途轮注入", () => {
  it("在途仍能发：调用 steerTurn 而不是另开流，投递成功后队列出现原文", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const steerTurn = vi.fn(async () => "accepted" as const);
    const { container } = renderView({ steerTurn });

    send("先跑一遍测试");
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));

    // 这一轮还没收尾，输入框不该被禁用
    expect(screen.getByRole("textbox")).not.toBeDisabled();
    send("顺便看下 lint");

    await waitFor(() =>
      expect(steerTurn).toHaveBeenCalledWith("顺便看下 lint", []),
    );
    // 没有第二条流：这条只是投进了在途轮的队列
    expect(fetchMock).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(queuedBlock(container)).not.toBeNull());
    expect(screen.getByText("顺便看下 lint")).toBeInTheDocument();
    expect(screen.getByText("将在下一次模型请求时送达")).toBeInTheDocument();
    expect(screen.getByRole("textbox")).toHaveValue("");
  });

  it("投递被 409 挡回：不硬发，这条在轮收尾后补成普通消息", async () => {
    // 收尾后补发的那条是新的一条流，各给一个未被读过 body 的响应
    const stream = controlledStream();
    const replayed = controlledStream();
    const streams = [stream, replayed];
    fetchMock.mockImplementation(
      async () => streams[fetchMock.mock.calls.length - 1].response,
    );
    const steerTurn = vi.fn(async () => "no_running_task" as const);
    const streamUrl = vi.fn((_message: string) => "/api/chat/conv_1/stream");
    const { container } = renderView({ steerTurn, streamUrl });

    send("开始");
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    send("补一句");
    await waitFor(() => expect(steerTurn).toHaveBeenCalledTimes(1));
    // 轮还没落下来：不许当场重发，也不许把气泡撤掉
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(container.querySelector("[data-slot='user-bubble']")).not.toBeNull();

    await stream.push(doneFrame(1));

    // 收尾之后由普通发送接住：第二条流带的就是刚才被挡回的正文
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    expect(streamUrl.mock.calls[1][0]).toBe("补一句");
    expect(
      container.querySelectorAll("[data-slot='user-bubble']"),
    ).toHaveLength(2);
  });

  it("投递异常：输入与附件还给用户，并提示重试", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const steerTurn = vi.fn(async () => {
      throw new Error("boom");
    });
    const { container } = renderView({ steerTurn });

    send("开始");
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    send("补一句");

    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith("发送失败，请重试"),
    );
    expect(screen.getByRole("textbox")).toHaveValue("补一句");
    expect(queuedBlock(container)).toBeNull();
  });

  it("user_steer：队列清空、气泡切分，后续增量落在新宿主上", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const steerTurn = vi.fn(async () => "accepted" as const);
    const { container } = renderView({ steerTurn });

    send("开始");
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(deltaFrame(1, "第一段"));
    await waitFor(() => expect(container.textContent).toContain("第一段"));

    send("补一句");
    await waitFor(() => expect(queuedBlock(container)).not.toBeNull());

    // 服务端把这条并进了当前轮
    await stream.push(steerFrame(2, 1, "补一句"));
    await waitFor(() => expect(queuedBlock(container)).toBeNull());
    const userBubbles = container.querySelectorAll("[data-slot='user-bubble']");
    expect(userBubbles).toHaveLength(2);
    expect(userBubbles[1].textContent).toContain("补一句");

    await stream.push(deltaFrame(3, "第二段"));
    await waitFor(() => expect(container.textContent).toContain("第二段"));
    const results = [
      ...container.querySelectorAll("[data-slot='assistant-result']"),
    ];
    // 旧气泡保留已产出内容，新宿主接住之后的增量
    expect(results).toHaveLength(2);
    expect(results[0].textContent).toContain("第一段");
    expect(results[1].textContent).toContain("第二段");
  });

  it("本地队列没有对应条目时用服务端下发的文本兜底", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const steerTurn = vi.fn(async () => "accepted" as const);
    const { container } = renderView({ steerTurn });

    send("开始");
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    // 别的客户端注入了这一轮：本地队列里一条都没有
    await stream.push(steerFrame(1, 1, "别处注入的一句"));

    await waitFor(() =>
      expect(container.textContent).toContain("别处注入的一句"),
    );
    const userBubbles = container.querySelectorAll("[data-slot='user-bubble']");
    expect(userBubbles).toHaveLength(2);
  });
});

describe("发送撞上服务端未接上的在途轮", () => {
  it("409 conversation_busy：改为 steering 投递，而不是让用户先停止", async () => {
    fetchMock.mockResolvedValue(
      new Response(
        JSON.stringify({ code: "conversation_busy", error: "busy" }),
        { status: 409 },
      ),
    );
    const steerTurn = vi.fn(async () => "accepted" as const);
    const { container } = renderView({ steerTurn });

    send("继续处理这个任务");

    await waitFor(() =>
      expect(steerTurn).toHaveBeenCalledWith("继续处理这个任务", []),
    );
    // 只发了一次：没有再开第二条流去抢在途轮
    expect(fetchMock).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(queuedBlock(container)).not.toBeNull());
    expect(screen.getByRole("textbox")).toHaveValue("");
    expect(toast.error).not.toHaveBeenCalledWith("这一轮还在跑，先停止再发");
  });

  it("steering 也失败（轮在 409 与投递之间收尾）：维持原来的失败提示与输入", async () => {
    fetchMock.mockResolvedValue(
      new Response(
        JSON.stringify({ code: "conversation_busy", error: "busy" }),
        { status: 409 },
      ),
    );
    const steerTurn = vi.fn(async () => "no_running_task" as const);
    renderView({ steerTurn });

    send("再试一次");

    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith("这一轮还在跑，先停止再发"),
    );
    expect(screen.getByRole("textbox")).toHaveValue("再试一次");
  });
});

describe("attach 接上的轮", () => {
  it("收到 user_steer：队列清空、气泡切分、增量落到新宿主", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const steerTurn = vi.fn(async () => "accepted" as const);
    const { container } = renderView({
      steerTurn,
      liveStreamUrl: () => LIVE_URL,
      historySettled: true,
      initialMessages: [
        {
          id: "m1",
          role: "user",
          content: "开始跑",
          turnIndex: 0,
          timestampMs: Date.now(),
        },
      ],
    });

    // 切走再回来：这一轮由重新订阅接上，本地没有发出去的那条流
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(snapshotFrame(1));
    await stream.push(deltaFrame(2, "第一段"));
    await waitFor(() => expect(container.textContent).toContain("第一段"));

    send("补一句");
    await waitFor(() => expect(queuedBlock(container)).not.toBeNull());

    // 服务端把这条并进了这一轮
    await stream.push(steerFrame(3, 1, "补一句"));

    await waitFor(() => expect(queuedBlock(container)).toBeNull());
    // 没有第二条流：这条只是被注入，没有另开一轮
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const userBubbles = container.querySelectorAll("[data-slot='user-bubble']");
    expect(userBubbles).toHaveLength(2);
    expect(userBubbles[1].textContent).toContain("补一句");

    // 新宿主接住注入之后的增量：旧气泡保留已产出的内容
    await stream.push(deltaFrame(4, "第二段"));
    await waitFor(() => expect(container.textContent).toContain("第二段"));
    const results = [
      ...container.querySelectorAll("[data-slot='assistant-result']"),
    ];
    expect(results).toHaveLength(2);
    expect(results[0].textContent).toContain("第一段");
    expect(results[1].textContent).toContain("第二段");
  });
});

describe("暂停入口", () => {
  it("唤醒轮（audience=headless）不给暂停按钮", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    renderView({
      liveStreamUrl: () => LIVE_URL,
      historySettled: true,
      onPause: () => {},
      onResume: () => {},
    });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(snapshotFrame(5, { audience: "headless" }));

    // 这一轮确实在跑（否则「没有暂停按钮」可能只是因为不在流式态）
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument(),
    );
    expect(screen.queryByRole("button", { name: "暂停" })).toBeNull();
  });

  it("live 轮照旧给暂停按钮", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    renderView({
      liveStreamUrl: () => LIVE_URL,
      historySettled: true,
      onPause: () => {},
      onResume: () => {},
    });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(snapshotFrame(5, { audience: "live" }));

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "暂停" })).toBeInTheDocument(),
    );
  });

  it("没接 steerTurn 时在途仍禁用输入，也不给发送入口", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    renderView();

    send("开始");
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));

    expect(screen.getByRole("textbox")).toBeDisabled();
    expect(screen.queryByRole("button", { name: "发送" })).toBeNull();
    // 在途另开一条流依然被挡在 handleSend 之外
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});
