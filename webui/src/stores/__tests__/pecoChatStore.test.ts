// pecoChatStore 连接生命周期单测 — 常驻单连接、POST 投递、占位消息、附着/取消
//
// 回归重点：一个窗口任意时刻至多一条 SSE。服务端把同一 run 的事件广播给
// 每个订阅者，多开一条就会让同一批 delta 被消费两次（界面文字成对重复）。

import { beforeEach, describe, expect, it, vi } from "vitest";
import { usePecoChatStore, __resetPecoStreamForTests } from "../pecoChatStore";
import { useAuthStore } from "../../stores/authStore";

vi.mock("@/api/peco", () => ({
  pecoStreamUrl: (m: string) =>
    `/api/peco/stream?message=${encodeURIComponent(m)}`,
  pecoAttachUrl: () => "/api/peco/stream",
  queryPecoStream: vi.fn(async () => undefined),
  cancelPecoStream: vi.fn(async () => ({ success: true })),
  clearPecoSession: vi.fn(async () => ({ success: true })),
  getPecoSession: vi.fn(),
}));

import { getPecoSession, cancelPecoStream, queryPecoStream } from "@/api/peco";

function frame(event: string, data: unknown): string {
  return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}

function sseResponse(frames: string[], keepOpen = false): Response {
  const encoder = new TextEncoder();
  const stream = new ReadableStream({
    start(controller) {
      for (const f of frames) controller.enqueue(encoder.encode(f));
      if (!keepOpen) controller.close();
      // keepOpen=true：流保持挂起（abortStream 测试用）
    },
  });
  return new Response(stream, { status: 200 });
}

/** 可手动推送事件、手动关闭的 SSE 流（跨轮次复用连接的场景）。 */
function controllableStream() {
  const encoder = new TextEncoder();
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const stream = new ReadableStream<Uint8Array>({
    start(c) {
      controller = c;
    },
  });
  return {
    response: new Response(stream, { status: 200 }),
    push: (...frames: string[]) => {
      for (const f of frames) controller.enqueue(encoder.encode(f));
    },
    close: () => controller.close(),
  };
}

/** 服务端确认一轮结束（清 isStreaming，连接保持打开）。 */
function turnCompleteFrame(text: string): string {
  return frame("turn_complete", {
    text,
    usage: { input_tokens: 1, output_tokens: 1 },
    conversation_id: "c",
  });
}

/** 全部 fetch 调用中命中 peco SSE 端点的那些。 */
function streamCalls(): { url: string; init?: RequestInit }[] {
  const mock = global.fetch as unknown as {
    mock: { calls: [string, RequestInit?][] };
  };
  return mock.mock.calls
    .map(([url, init]) => ({ url, init }))
    .filter((c) => c.url.startsWith("/api/peco/stream"));
}

const snapshot = (turnInFlight: boolean) => ({
  conversation_id: "u1-private-session",
  turns: [
    {
      turn_index: 0,
      messages: [
        { role: "user", content: "上一轮提问", timestamp_ms: 1 },
        { role: "assistant", content: "上一轮回答", timestamp_ms: 2 },
      ],
    },
  ],
  total_usage: { input_tokens: 10, output_tokens: 5 },
  // run 已注册（停靠等输入）时 is_running 为真而 turn_in_flight 为假 ——
  // 客户端只按后者决定是否附着，否则会挂出一条永不填充的空占位
  is_running: true,
  turn_in_flight: turnInFlight,
});

const resetStore = () => {
  __resetPecoStreamForTests();
  usePecoChatStore.setState({
    loaded: false,
    loading: false,
    messages: [],
    sessionKey: 0,
    isStreaming: false,
    error: null,
    usage: null,
  });
};

beforeEach(() => {
  vi.clearAllMocks();
  resetStore();
  useAuthStore.setState({ token: "test-token" });
  global.fetch = vi.fn();
});

describe("pecoChatStore load() reattach", () => {
  it("turn_in_flight=true 时追加占位并重新附着，delta 落在占位消息上", async () => {
    vi.mocked(getPecoSession).mockResolvedValue(snapshot(true) as never);
    global.fetch = vi.fn(() =>
      Promise.resolve(
        sseResponse([
          frame("text_delta", {
            content: "进行中的新回复",
            conversation_id: "c",
          }),
          frame("done", {
            usage: { input_tokens: 1, output_tokens: 1 },
            conversation_id: "c",
          }),
        ]),
      ),
    );

    await usePecoChatStore.getState().load();

    // 附着请求不带 message 参数
    expect(global.fetch).toHaveBeenCalledWith(
      "/api/peco/stream",
      expect.objectContaining({
        headers: { Authorization: "Bearer test-token" },
      }),
    );

    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().isStreaming).toBe(false),
    );

    const messages = usePecoChatStore.getState().messages;
    // 末条是流式新产生的 assistant 占位（delta 应用其上），而非并入上一轮回答
    expect(messages[messages.length - 1]).toMatchObject({
      role: "assistant",
      content: "进行中的新回复",
    });
    expect(messages[messages.length - 2]).toMatchObject({
      role: "assistant",
      content: "上一轮回答",
    });
  });

  it("run 已注册但停靠（turn_in_flight=false）时不附着", async () => {
    vi.mocked(getPecoSession).mockResolvedValue(snapshot(false) as never);

    await usePecoChatStore.getState().load();

    expect(global.fetch).not.toHaveBeenCalled();
    expect(usePecoChatStore.getState().isStreaming).toBe(false);
  });

  it("无 token 时不发起附着请求", async () => {
    useAuthStore.setState({ token: null });
    vi.mocked(getPecoSession).mockResolvedValue(snapshot(true) as never);

    await usePecoChatStore.getState().load();

    expect(global.fetch).not.toHaveBeenCalled();
  });
});

describe("pecoChatStore 常驻连接", () => {
  it("第二条消息复用同一条连接走 POST，不再新开 SSE", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));

    await usePecoChatStore.getState().sendMessage("第一条", "test-token");
    expect(streamCalls()).toHaveLength(1);
    expect(streamCalls()[0].url).toContain("message=");

    // 服务端确认本轮结束：连接保持打开（正常轮次不发 done）
    s.push(turnCompleteFrame("第一条"));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().isStreaming).toBe(false),
    );

    await usePecoChatStore.getState().sendMessage("第二条", "test-token");

    // 关键回归断言：仍然只有一条 SSE 连接，第二条消息经控制通道投递
    expect(streamCalls()).toHaveLength(1);
    expect(queryPecoStream).toHaveBeenCalledWith("第二条");
  });

  it("同一条连接上的增量只应用一次（不成对重复）", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));

    await usePecoChatStore.getState().sendMessage("问", "test-token");
    s.push(
      frame("text_delta", { content: "先看真实状态，", conversation_id: "c" }),
      frame("text_delta", { content: "不靠记忆猜。", conversation_id: "c" }),
    );

    await vi.waitFor(() => {
      const last = usePecoChatStore.getState().messages.at(-1);
      expect(last?.content).toBe("先看真实状态，不靠记忆猜。");
    });
  });

  it("截断重试在占位气泡之前插横幅，重试的增量仍落进气泡", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));

    await usePecoChatStore.getState().sendMessage("问", "test-token");
    s.push(
      frame("text_delta", { content: "part", conversation_id: "c" }),
      frame("truncation_retry", {
        attempt: 1,
        limit: 1,
        output_tokens: 4096,
        retry_budget: 32768,
        conversation_id: "c",
      }),
      frame("text_delta", { content: "done", conversation_id: "c" }),
    );

    await vi.waitFor(() => {
      const msgs = usePecoChatStore.getState().messages;
      expect(msgs.at(-1)?.content).toBe("partdone");
    });

    const msgs = usePecoChatStore.getState().messages;
    const notice = msgs.find((m) => m.isNotice);
    expect(notice?.content).toContain("1/1");
    // 横幅不在末尾 —— 末尾留给重试后继续生长的回答气泡
    expect(msgs.at(-1)?.isNotice).toBeUndefined();
  });

  it("终态事件后迟到的增量被门控丢弃", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));

    await usePecoChatStore.getState().sendMessage("问", "test-token");
    s.push(
      frame("text_delta", { content: "答案", conversation_id: "c" }),
      turnCompleteFrame("答案"),
    );
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().isStreaming).toBe(false),
    );

    // 其他窗口的轮次 / 迟到的重复增量：连接仍在收，但本窗口无在途轮次
    s.push(
      frame("text_delta", { content: "别人的增量", conversation_id: "c" }),
    );

    await new Promise((r) => setTimeout(r, 20));
    const last = usePecoChatStore.getState().messages.at(-1);
    expect(last?.content).toBe("答案");
  });

  it("POST 404 时回落到带 message 的引导路径并重开连接", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    await usePecoChatStore.getState().sendMessage("第一条", "test-token");
    s.push(turnCompleteFrame("第一条"));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().isStreaming).toBe(false),
    );

    const firstSignal = streamCalls()[0].init?.signal;
    vi.mocked(queryPecoStream).mockRejectedValueOnce({
      response: { status: 404 },
    });

    await usePecoChatStore.getState().sendMessage("第二条", "test-token");

    const calls = streamCalls();
    expect(calls).toHaveLength(2);
    expect(calls[1].url).toContain("message=");
    expect(firstSignal?.aborted).toBe(true);
  });

  it("POST 409（背压）时撤回占位气泡并报错，不新开连接", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    await usePecoChatStore.getState().sendMessage("第一条", "test-token");
    s.push(turnCompleteFrame("第一条"));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().isStreaming).toBe(false),
    );

    vi.mocked(queryPecoStream).mockRejectedValueOnce({
      response: { status: 409 },
    });
    await usePecoChatStore.getState().sendMessage("第二条", "test-token");

    expect(streamCalls()).toHaveLength(1);
    const state = usePecoChatStore.getState();
    expect(state.isStreaming).toBe(false);
    expect(state.error).toBeTruthy();
    // 用户消息保留（可重发），空的 assistant 占位被撤回
    expect(state.messages.at(-1)).toMatchObject({
      role: "user",
      content: "第二条",
    });
  });

  it("连接关闭后下一次发送回到引导路径", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    await usePecoChatStore.getState().sendMessage("第一条", "test-token");

    // 服务端收尾：done + 关流
    s.push(
      frame("done", {
        usage: { input_tokens: 1, output_tokens: 1 },
        conversation_id: "c",
      }),
      turnCompleteFrame("第一条"),
    );
    s.close();
    await vi.waitFor(() => expect(streamCalls()).toHaveLength(1));

    global.fetch = vi.fn(() => Promise.resolve(sseResponse([], true)));
    await usePecoChatStore.getState().sendMessage("第二条", "test-token");

    expect(streamCalls()).toHaveLength(1);
    expect(streamCalls()[0].url).toContain("message=");
    expect(queryPecoStream).not.toHaveBeenCalled();
  });

  it("意外断流（未收到 done）触发一次性重附着", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    vi.mocked(getPecoSession).mockResolvedValue(snapshot(false) as never);

    await usePecoChatStore.getState().sendMessage("问", "test-token");
    s.close(); // 无 done 的断流

    await vi.waitFor(() => expect(getPecoSession).toHaveBeenCalledTimes(1));
    // 追平结果无在途轮次 → 不再重开连接（run 停靠）
    expect(streamCalls()).toHaveLength(1);
  });
});

describe("pecoChatStore abortStream()", () => {
  it("调用服务端取消、断开连接并复位 isStreaming", async () => {
    vi.mocked(getPecoSession).mockResolvedValue(snapshot(true) as never);
    global.fetch = vi.fn(() => Promise.resolve(sseResponse([], true)));

    await usePecoChatStore.getState().load();
    expect(usePecoChatStore.getState().isStreaming).toBe(true);

    usePecoChatStore.getState().abortStream();

    expect(cancelPecoStream).toHaveBeenCalledTimes(1);
    expect(usePecoChatStore.getState().isStreaming).toBe(false);
    expect(streamCalls()[0].init?.signal?.aborted).toBe(true);
  });
});

describe("pecoChatStore refresh()", () => {
  it("追平其他窗口的进度：恢复新落盘轮次并附着进行中任务", async () => {
    // 初始载入时无在途轮次
    vi.mocked(getPecoSession).mockResolvedValue(snapshot(false) as never);
    await usePecoChatStore.getState().load();
    expect(global.fetch).not.toHaveBeenCalled();

    // 另一窗口发起了任务，且有一轮已落盘 + 一轮进行中
    const caughtUp = {
      ...snapshot(true),
      turns: [
        ...snapshot(true).turns,
        {
          turn_index: 1,
          messages: [
            { role: "user", content: "别处窗口的提问", timestamp_ms: 3 },
            { role: "assistant", content: "已落盘的回答", timestamp_ms: 4 },
          ],
        },
      ],
    };
    vi.mocked(getPecoSession).mockResolvedValue(caughtUp as never);
    global.fetch = vi.fn(() =>
      Promise.resolve(
        sseResponse([
          frame("text_delta", {
            content: "接上的增量",
            conversation_id: "c",
          }),
          frame("done", {
            usage: { input_tokens: 1, output_tokens: 1 },
            conversation_id: "c",
          }),
        ]),
      ),
    );

    await usePecoChatStore.getState().refresh();

    // 纯附着 URL（无 message 参数）
    expect(global.fetch).toHaveBeenCalledWith(
      "/api/peco/stream",
      expect.anything(),
    );

    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().isStreaming).toBe(false),
    );

    const messages = usePecoChatStore.getState().messages;
    // 快照整表替换：新落盘轮次可见；末条为进行中占位（已收到增量）
    expect(messages).toContainEqual(
      expect.objectContaining({ content: "已落盘的回答" }),
    );
    expect(messages[messages.length - 1]).toMatchObject({
      role: "assistant",
      content: "接上的增量",
    });
  });

  it("本窗口流式中跳过刷新", async () => {
    usePecoChatStore.setState({ isStreaming: true });
    vi.mocked(getPecoSession).mockResolvedValue(snapshot(true) as never);

    await usePecoChatStore.getState().refresh();

    expect(getPecoSession).not.toHaveBeenCalled();
    expect(usePecoChatStore.getState().isStreaming).toBe(true);
  });

  it("无 token 时跳过刷新", async () => {
    useAuthStore.setState({ token: null });

    await usePecoChatStore.getState().refresh();

    expect(getPecoSession).not.toHaveBeenCalled();
  });

  it("已有连接时空闲追平不新开连接", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    await usePecoChatStore.getState().sendMessage("问", "test-token");
    s.push(turnCompleteFrame("答"));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().isStreaming).toBe(false),
    );

    vi.mocked(getPecoSession).mockResolvedValue(snapshot(false) as never);
    await usePecoChatStore.getState().refresh();

    expect(getPecoSession).toHaveBeenCalledTimes(1);
    expect(streamCalls()).toHaveLength(1);
  });

  it("快照在途期间发出的消息不被整表替换抹掉", async () => {
    let resolveSnap!: (v: unknown) => void;
    vi.mocked(getPecoSession).mockReturnValue(
      new Promise((r) => {
        resolveSnap = r;
      }) as never,
    );

    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));

    const refreshing = usePecoChatStore.getState().refresh();
    await usePecoChatStore.getState().sendMessage("刚发的消息", "test-token");
    resolveSnap(snapshot(false));
    await refreshing;

    const messages = usePecoChatStore.getState().messages;
    expect(messages).toContainEqual(
      expect.objectContaining({ role: "user", content: "刚发的消息" }),
    );
    expect(usePecoChatStore.getState().isStreaming).toBe(true);
  });
});
