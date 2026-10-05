// pecoChatStore 连接生命周期单测 — 常驻单连接、POST 投递、占位消息、附着/取消
//
// 回归重点：一个窗口任意时刻至多一条 SSE。服务端把同一 run 的事件广播给
// 每个订阅者，多开一条就会让同一批 delta 被消费两次（界面文字成对重复）。

import { beforeEach, describe, expect, it, vi } from "vitest";
import { usePecoChatStore, __resetPecoStreamForTests } from "../pecoChatStore";
import { snapshotToMessages } from "../../components/chat/ChatView";
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
    oldestLoadedTurn: null,
    hasMore: false,
    totalTurns: 0,
    historyRevision: 0,
    loadingEarlier: false,
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

// ── 分页状态机（turn 级分页 + revision 失效） ────────────────────────────

/** 造单轮：user + assistant 各一条，turn_index 取全局位置号。 */
const turn = (turn_index: number, text: string) => ({
  turn_index,
  messages: [
    { role: "user", content: text, timestamp_ms: turn_index * 10 },
    { role: "assistant", content: `${text}-答`, timestamp_ms: turn_index * 10 + 1 },
  ],
});

/** 造分页快照（按需携带 context_metrics / total_turns / has_more / pinned_summary）。 */
const pagedSnapshot = (opts: {
  turns: ReturnType<typeof turn>[];
  compaction_count?: number;
  total_turns?: number;
  has_more?: boolean;
  pinned_summary?: string;
  turn_in_flight?: boolean;
}) => ({
  conversation_id: "u1-private-session",
  turns: opts.turns,
  total_usage: { input_tokens: 10, output_tokens: 5 },
  is_running: true,
  turn_in_flight: opts.turn_in_flight ?? false,
  ...(opts.pinned_summary ? { pinned_summary: opts.pinned_summary } : {}),
  ...(opts.compaction_count !== undefined
    ? { context_metrics: { compaction_count: opts.compaction_count } }
    : {}),
  ...(opts.total_turns !== undefined ? { total_turns: opts.total_turns } : {}),
  ...(opts.has_more !== undefined ? { has_more: opts.has_more } : {}),
});

const pinnedMsg = () => ({
  id: "pinned-summary",
  role: "assistant" as const,
  content: "更早的对话已归档为摘要，仍在模型上下文中",
  turnIndex: 0,
  isNotice: true,
  summary: "<earlier_context_summary>…</earlier_context_summary>",
});

describe("pecoChatStore 分页", () => {
  it("revision 不变时保留旧页，仅 turn 段级替换尾部窗口", async () => {
    // 旧页（turn 0）已加载 + 尾部（turn 2）已加载，处于翻页中途
    usePecoChatStore.setState({
      loaded: true,
      messages: [
        { id: "turn-0-0", role: "user", content: "旧页提问", turnIndex: 0 },
        { id: "turn-2-0", role: "user", content: "旧尾部提问", turnIndex: 2 },
      ],
      oldestLoadedTurn: 0,
      hasMore: true,
      totalTurns: 5,
      historyRevision: 0,
    });

    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(2, "新2"), turn(3, "新3"), turn(4, "新4")],
        compaction_count: 0,
        total_turns: 5,
        has_more: true,
      }) as never,
    );

    await usePecoChatStore.getState().refresh();

    const messages = usePecoChatStore.getState().messages;
    // 旧页（turnIndex < 尾部窗口起点 2）保留
    expect(messages).toContainEqual(
      expect.objectContaining({ content: "旧页提问", turnIndex: 0 }),
    );
    // 尾部被权威替换：旧的尾部文本消失，新的就位
    expect(messages).not.toContainEqual(
      expect.objectContaining({ content: "旧尾部提问" }),
    );
    expect(messages).toContainEqual(
      expect.objectContaining({ content: "新4", turnIndex: 4 }),
    );
    expect(usePecoChatStore.getState().oldestLoadedTurn).toBe(0);
    // 已加载到 turn 0：前面没有可加载的轮 → hasMore 必须为 false。
    // （尾页 has_more 恒真，是「尾窗口之前还有轮」而非「本端还有未加载轮」。）
    expect(usePecoChatStore.getState().hasMore).toBe(false);
    expect(usePecoChatStore.getState().totalTurns).toBe(5);
  });

  it("revision 变化（compaction）时丢弃旧页，只保留尾部窗口", async () => {
    usePecoChatStore.setState({
      loaded: true,
      messages: [
        { id: "turn-0-0", role: "user", content: "旧页提问", turnIndex: 0 },
        { id: "turn-2-0", role: "user", content: "旧尾部提问", turnIndex: 2 },
      ],
      oldestLoadedTurn: 0,
      hasMore: true,
      totalTurns: 5,
      historyRevision: 0,
    });

    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(1, "压缩后1"), turn(2, "压缩后2")],
        compaction_count: 1, // 位置号整体前移，旧页游标全部失效
        total_turns: 2,
        has_more: false,
      }) as never,
    );

    await usePecoChatStore.getState().refresh();

    const messages = usePecoChatStore.getState().messages;
    expect(messages).not.toContainEqual(
      expect.objectContaining({ content: "旧页提问" }),
    );
    expect(messages).toContainEqual(
      expect.objectContaining({ content: "压缩后2", turnIndex: 2 }),
    );
    expect(usePecoChatStore.getState().oldestLoadedTurn).toBe(1);
    expect(usePecoChatStore.getState().historyRevision).toBe(1);
  });

  it("clear 重置全部分页状态", async () => {
    usePecoChatStore.setState({
      loaded: true,
      messages: [{ id: "turn-0-0", role: "user", content: "x", turnIndex: 0 }],
      oldestLoadedTurn: 0,
      hasMore: true,
      totalTurns: 5,
      historyRevision: 2,
      loadingEarlier: true,
    });

    await usePecoChatStore.getState().clear();

    const s = usePecoChatStore.getState();
    expect(s.messages).toEqual([]);
    expect(s.oldestLoadedTurn).toBeNull();
    expect(s.hasMore).toBe(false);
    expect(s.totalTurns).toBe(0);
    expect(s.historyRevision).toBe(0);
    expect(s.loadingEarlier).toBe(false);
  });

  it("loadEarlier 前置合并更早轮并保持 pinned 摘要恒在最前、恰好一条", async () => {
    usePecoChatStore.setState({
      loaded: true,
      messages: [
        pinnedMsg(),
        ...snapshotToMessages([turn(3, "尾部3"), turn(4, "尾部4")]),
      ],
      oldestLoadedTurn: 3,
      hasMore: true,
      totalTurns: 6,
      historyRevision: 0,
    });

    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(0, "更早0"), turn(1, "更早1"), turn(2, "更早2")],
        compaction_count: 0,
        total_turns: 6,
        has_more: false,
      }) as never,
    );

    await usePecoChatStore.getState().loadEarlier();

    const messages = usePecoChatStore.getState().messages;
    // pinned 摘要恰好一条，恒在最前（与 turn 0 撞号不重复、不丢失）
    expect(messages.filter((m) => m.id === "pinned-summary")).toHaveLength(1);
    expect(messages[0].id).toBe("pinned-summary");
    // 更早的轮插在 pinned 之后、尾部之前，位置号连续
    expect(messages).toContainEqual(
      expect.objectContaining({ content: "更早0", turnIndex: 0 }),
    );
    expect(messages).toContainEqual(
      expect.objectContaining({ content: "尾部4", turnIndex: 4 }),
    );
    expect(messages.slice(1).map((m) => m.turnIndex)).toEqual([
      0, 0, 1, 1, 2, 2, 3, 3, 4, 4,
    ]);

    expect(usePecoChatStore.getState().oldestLoadedTurn).toBe(0);
    expect(usePecoChatStore.getState().hasMore).toBe(false);
    expect(usePecoChatStore.getState().totalTurns).toBe(6);
  });

  it("loadEarlier 在流式中 / 无更多 / 无已加载轮时直接返回，不发请求", async () => {
    usePecoChatStore.setState({
      isStreaming: true,
      hasMore: true,
      oldestLoadedTurn: 3,
    });
    await usePecoChatStore.getState().loadEarlier();
    expect(getPecoSession).not.toHaveBeenCalled();

    usePecoChatStore.setState({
      isStreaming: false,
      hasMore: false,
      oldestLoadedTurn: 3,
    });
    await usePecoChatStore.getState().loadEarlier();
    expect(getPecoSession).not.toHaveBeenCalled();

    usePecoChatStore.setState({
      isStreaming: false,
      hasMore: true,
      oldestLoadedTurn: null,
    });
    await usePecoChatStore.getState().loadEarlier();
    expect(getPecoSession).not.toHaveBeenCalled();
  });

  it("loadEarlier 用 before=oldestLoadedTurn 翻页", async () => {
    usePecoChatStore.setState({
      loaded: true,
      messages: snapshotToMessages([turn(3, "尾部3")]),
      oldestLoadedTurn: 3,
      hasMore: true,
      totalTurns: 5,
      historyRevision: 0,
    });

    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(0, "更早0")],
        compaction_count: 0,
        total_turns: 5,
        has_more: false,
      }) as never,
    );

    await usePecoChatStore.getState().loadEarlier();

    expect(getPecoSession).toHaveBeenCalledWith({
      turns: 3,
      before: 3,
    });
  });

  // 回归：refreshSession 的「保留旧页」合并必须先剥除旧 pinned 分隔线，
  // 否则它 turnIndex 恒为 0，会被 < tailStart 选中，与尾部窗口新带的那条重复。
  it("refresh 追平后 pinned 摘要恒为一条（尾部窗口权威）", async () => {
    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(3, "t3"), turn(4, "t4"), turn(5, "t5")],
        compaction_count: 0,
        total_turns: 6,
        has_more: true,
        pinned_summary: "<earlier_context_summary>…</earlier_context_summary>",
      }) as never,
    );

    await usePecoChatStore.getState().load();
    expect(
      usePecoChatStore.getState().messages.filter((m) => m.id === "pinned-summary"),
    ).toHaveLength(1);

    // 窗口获焦追平：再次 refreshSession 走「保留旧页」分支
    await usePecoChatStore.getState().refresh();

    const messages = usePecoChatStore.getState().messages;
    expect(messages.filter((m) => m.id === "pinned-summary")).toHaveLength(1);
    expect(messages[0].id).toBe("pinned-summary");
    expect(messages.map((m) => m.turnIndex)).toEqual([0, 3, 3, 4, 4, 5, 5]);
  });

  it("loadEarlier 后再 refresh 仍恰好一条 pinned", async () => {
    usePecoChatStore.setState({
      loaded: true,
      messages: [
        pinnedMsg(),
        ...snapshotToMessages([turn(0, "更早0"), turn(1, "更早1")]),
        ...snapshotToMessages([turn(3, "尾部3"), turn(4, "尾部4")]),
      ],
      oldestLoadedTurn: 0,
      hasMore: false,
      totalTurns: 5,
      historyRevision: 0,
    });

    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(3, "尾部3"), turn(4, "尾部4")],
        compaction_count: 0,
        total_turns: 5,
        has_more: false,
        pinned_summary: "<earlier_context_summary>…</earlier_context_summary>",
      }) as never,
    );

    await usePecoChatStore.getState().refresh();

    const messages = usePecoChatStore.getState().messages;
    expect(messages.filter((m) => m.id === "pinned-summary")).toHaveLength(1);
    expect(messages[0].id).toBe("pinned-summary");
    // 更早的已加载轮保留，尾部被权威替换
    expect(messages).toContainEqual(
      expect.objectContaining({ content: "更早0", turnIndex: 0 }),
    );
    expect(messages).toContainEqual(
      expect.objectContaining({ content: "尾部4", turnIndex: 4 }),
    );
    expect(usePecoChatStore.getState().oldestLoadedTurn).toBe(0);
  });

  it("refresh 丢弃流式残留（无 id、turnIndex 0），已完成的轮不重复", async () => {
    // 复现：≥3 轮会话发一条消息、流式到完成 → 本地残留（无 id、turnIndex 0）
    // 仍在 messages 尾部；切走切回触发 refresh 时若被当旧页保留，就会与尾部
    // 窗口里的同一条轮重复渲染，且错位到更早轮之前。
    usePecoChatStore.setState({
      loaded: true,
      messages: [
        ...snapshotToMessages([turn(3, "t3"), turn(4, "t4")]),
        // sendMessage 追加的形状：无 id、turnIndex 恒 0
        { role: "user", content: "我刚发的问题", turnIndex: 0 },
        { role: "assistant", content: "我刚得到的回答", turnIndex: 0 },
      ],
      oldestLoadedTurn: 3,
      hasMore: true,
      totalTurns: 5,
      historyRevision: 0,
    });

    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(4, "t4"), turn(5, "t5"), turn(6, "我刚发的问题")],
        compaction_count: 0,
        total_turns: 7,
        has_more: true,
      }) as never,
    );

    await usePecoChatStore.getState().refresh();

    const messages = usePecoChatStore.getState().messages;
    // 刚发的那轮只由尾部窗口权威提供一份 —— 本地残留必须被丢弃
    expect(messages.filter((m) => m.content === "我刚发的问题")).toHaveLength(1);
    // 本地残留的 assistant 文本必须消失（快照权威版本是「我刚发的问题-答」）
    expect(messages.filter((m) => m.content === "我刚得到的回答")).toHaveLength(0);
    // 且必须落在尾部（turnIndex 6），不得错位到更早轮之前
    const idx = messages.findIndex((m) => m.content === "我刚发的问题");
    expect(messages[idx].turnIndex).toBe(6);
    // 更早已加载轮（turn 3）仍保留
    expect(messages).toContainEqual(
      expect.objectContaining({ content: "t3", turnIndex: 3 }),
    );
  });

  it("翻到底（oldestLoadedTurn=0）后 refresh，hasMore 不假真", async () => {
    usePecoChatStore.setState({
      loaded: true,
      messages: snapshotToMessages([
        turn(0, "t0"),
        turn(1, "t1"),
        turn(2, "t2"),
        turn(3, "t3"),
        turn(4, "t4"),
      ]),
      oldestLoadedTurn: 0,
      hasMore: false,
      totalTurns: 5,
      historyRevision: 0,
    });

    // 尾页 has_more 恒为 true（total 5 > PAGE_TURNS 3），不能据此判定本端还有旧页
    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(2, "t2"), turn(3, "t3"), turn(4, "t4")],
        compaction_count: 0,
        total_turns: 5,
        has_more: true,
      }) as never,
    );

    await usePecoChatStore.getState().refresh();

    const state = usePecoChatStore.getState();
    expect(state.oldestLoadedTurn).toBe(0);
    expect(state.hasMore).toBe(false);
  });
});
