// pecoChatStore 连接生命周期单测 — 常驻单连接、POST 投递、占位消息、附着/取消
//
// 回归重点：一个窗口任意时刻至多一条 SSE。服务端把同一 run 的事件广播给
// 每个订阅者，多开一条就会让同一批 delta 被消费两次（界面文字成对重复）。

import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  usePecoChatStore,
  __resetPecoStreamForTests,
  mergeQueuedText,
  settleQueue,
} from "../pecoChatStore";
import { snapshotToMessages } from "../../components/chat/ChatView";
import { useAuthStore } from "../../stores/authStore";

vi.mock("@/api/peco", () => ({
  pecoStreamUrl: (m: string, imageIds: string[] = [], fileIds: string[] = []) =>
    `/api/peco/stream?message=${encodeURIComponent(m)}` +
    (imageIds.length > 0 ? `&image_ids=${imageIds.join(",")}` : "") +
    (fileIds.length > 0 ? `&file_ids=${fileIds.join(",")}` : ""),
  uploadPecoImage: vi.fn(),
  pecoAttachUrl: () => "/api/peco/stream",
  queryPecoStream: vi.fn(async () => undefined),
  cancelPecoStream: vi.fn(async () => ({ success: true })),
  pausePecoStream: vi.fn(async () => ({ success: true })),
  resumePecoStream: vi.fn(async () => ({ success: true })),
  clearPecoSession: vi.fn(async () => ({ success: true })),
  getPecoSession: vi.fn(),
}));

import {
  getPecoSession,
  cancelPecoStream,
  queryPecoStream,
  pausePecoStream,
  resumePecoStream,
} from "@/api/peco";

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
    paused: false,
    queued: [],
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

  it("带图消息开新流并在 URL 携带 image_ids（不经 POST 控制通道）", async () => {
    global.fetch = vi.fn(() => Promise.resolve(controllableStream().response));

    await usePecoChatStore.getState().sendMessage("看这张图", "test-token", [
      {
        id: "a.png",
        kind: "image",
        name: "a.png",
        url: "/uploads/chat/u/a.png",
      },
    ]);

    // 图片无法经控制通道投递 —— 不得走 POST 复用路径
    expect(queryPecoStream).not.toHaveBeenCalled();
    const calls = streamCalls();
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toContain("image_ids=a.png");

    // 乐观用户消息携带渲染 URL
    const lastUser = [...usePecoChatStore.getState().messages]
      .reverse()
      .find((m) => m.role === "user");
    expect(lastUser?.images).toEqual(["/uploads/chat/u/a.png"]);
  });

  it("存在活连接时，带图消息仍开新流而非复用 POST", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));

    await usePecoChatStore.getState().sendMessage("第一条", "test-token");
    s.push(turnCompleteFrame("第一条"));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().isStreaming).toBe(false),
    );
    expect(streamCalls()).toHaveLength(1);

    global.fetch = vi.fn(() => Promise.resolve(controllableStream().response));
    await usePecoChatStore.getState().sendMessage("看图", "test-token", [
      {
        id: "b.png",
        kind: "image",
        name: "b.png",
        url: "/uploads/chat/u/b.png",
      },
    ]);

    expect(queryPecoStream).not.toHaveBeenCalled();
    expect(streamCalls()).toHaveLength(1);
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
    // 连接断开后收不到服务端终态事件 → 本地给结论宿主补「取消」状态
    const messages = usePecoChatStore.getState().messages;
    expect(messages[messages.length - 1]?.turnResult?.status).toBe("cancelled");
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
    {
      role: "assistant",
      content: `${text}-答`,
      timestamp_ms: turn_index * 10 + 1,
    },
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
      usePecoChatStore
        .getState()
        .messages.filter((m) => m.id === "pinned-summary"),
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
    expect(messages.filter((m) => m.content === "我刚发的问题")).toHaveLength(
      1,
    );
    // 本地残留的 assistant 文本必须消失（快照权威版本是「我刚发的问题-答」）
    expect(messages.filter((m) => m.content === "我刚得到的回答")).toHaveLength(
      0,
    );
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

describe("pecoChatStore 暂停/恢复", () => {
  it("looper_state 事件置位/复位 paused，终态事件复位", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));

    await usePecoChatStore.getState().sendMessage("问", "test-token");
    expect(usePecoChatStore.getState().paused).toBe(false);

    // 服务端在边界检查点落地暂停 → 事件回填
    s.push(frame("looper_state", { paused: true, conversation_id: "c" }));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().paused).toBe(true),
    );

    // 恢复
    s.push(frame("looper_state", { paused: false, conversation_id: "c" }));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().paused).toBe(false),
    );

    // 暂停后再收尾：终态事件必须复位 paused，避免「恢复」按钮残留
    s.push(frame("looper_state", { paused: true, conversation_id: "c" }));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().paused).toBe(true),
    );
    s.push(turnCompleteFrame("答"));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().isStreaming).toBe(false),
    );
    expect(usePecoChatStore.getState().paused).toBe(false);
  });

  it("非流式时 looper_state 不改 paused（门控与增量一致）", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));

    await usePecoChatStore.getState().sendMessage("问", "test-token");
    s.push(turnCompleteFrame("答"));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().isStreaming).toBe(false),
    );

    // 其他窗口的 run 状态事件：本窗口无在途轮次，不得置位
    s.push(frame("looper_state", { paused: true, conversation_id: "c" }));
    await new Promise((r) => setTimeout(r, 20));
    expect(usePecoChatStore.getState().paused).toBe(false);
  });

  it("pause()/resume() 调对应 API 且不断连、不改 isStreaming", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));

    await usePecoChatStore.getState().sendMessage("问", "test-token");
    expect(usePecoChatStore.getState().isStreaming).toBe(true);

    usePecoChatStore.getState().pause();
    expect(pausePecoStream).toHaveBeenCalledTimes(1);
    // 暂停不等于取消：连接与在途态保持
    expect(cancelPecoStream).not.toHaveBeenCalled();
    expect(usePecoChatStore.getState().isStreaming).toBe(true);
    expect(streamCalls()[0].init?.signal?.aborted).toBe(false);
    // 不乐观置位 —— 只认 SSE 事件
    expect(usePecoChatStore.getState().paused).toBe(false);

    usePecoChatStore.getState().resume();
    expect(resumePecoStream).toHaveBeenCalledTimes(1);
    expect(usePecoChatStore.getState().isStreaming).toBe(true);
  });

  it("服务端拒绝（409）时静默，不影响在途态", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));

    await usePecoChatStore.getState().sendMessage("问", "test-token");
    vi.mocked(pausePecoStream).mockRejectedValueOnce({
      response: { status: 409 },
    });

    usePecoChatStore.getState().pause();
    await new Promise((r) => setTimeout(r, 20));
    expect(usePecoChatStore.getState().isStreaming).toBe(true);
    expect(usePecoChatStore.getState().paused).toBe(false);
    expect(usePecoChatStore.getState().error).toBeNull();
  });

  it("刷新/重附着从快照恢复 paused，直接呈现恢复入口", async () => {
    // 服务端暂停后在途轮仍在：快照 paused=true 且 turn_in_flight=true
    vi.mocked(getPecoSession).mockResolvedValue({
      ...snapshot(true),
      paused: true,
    } as never);
    global.fetch = vi.fn(() => Promise.resolve(controllableStream().response));

    await usePecoChatStore.getState().load();

    // 重连没有「跨越 Paused 的转换」事件，快照是暂停态的唯一来源
    expect(usePecoChatStore.getState().paused).toBe(true);
    expect(usePecoChatStore.getState().isStreaming).toBe(true);
  });

  it("快照 paused=false 时不残留暂停态", async () => {
    vi.mocked(getPecoSession).mockResolvedValue({
      ...snapshot(true),
      paused: false,
    } as never);
    global.fetch = vi.fn(() => Promise.resolve(controllableStream().response));

    await usePecoChatStore.getState().load();

    expect(usePecoChatStore.getState().paused).toBe(false);
    expect(usePecoChatStore.getState().isStreaming).toBe(true);
  });
});

describe("pecoChatStore 在途轮发送（steering）", () => {
  async function startTurn() {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    await usePecoChatStore.getState().sendMessage("第一条", "test-token");
    s.push(frame("text_delta", { content: "旧", conversation_id: "c" }));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().messages.at(-1)?.content).toBe("旧"),
    );
    return s;
  }

  it("在途发送只进排队区：POST 投递、不插占位、不改 messages", async () => {
    const s = await startTurn();
    const before = usePecoChatStore.getState().messages.length;

    // 暂停中同样可发送（排队不影响暂停态）
    s.push(frame("looper_state", { paused: true, conversation_id: "c" }));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().paused).toBe(true),
    );

    await usePecoChatStore.getState().sendMessage("补充", "test-token");

    expect(queryPecoStream).toHaveBeenCalledWith("补充", [], []);
    const st = usePecoChatStore.getState();
    expect(st.queued).toHaveLength(1);
    expect(st.queued[0].text).toBe("补充");
    expect(st.messages).toHaveLength(before);
    expect(st.isStreaming).toBe(true);
    expect(st.paused).toBe(true);

    // 在途附件经同一控制通道投递（不再断连重开 SSE）
    await usePecoChatStore.getState().sendMessage("看图", "test-token", [
      {
        id: "b.png",
        kind: "image",
        name: "b.png",
        url: "/uploads/chat/u/b.png",
      },
    ]);
    expect(queryPecoStream).toHaveBeenLastCalledWith("看图", ["b.png"], []);
    expect(usePecoChatStore.getState().queued).toHaveLength(2);
  });

  it("user_steer：排队项落成 user 气泡 + 新 assistant 占位，旧输出留在旧气泡", async () => {
    const s = await startTurn();
    await usePecoChatStore.getState().sendMessage("补充", "test-token");

    s.push(
      frame("user_steer", { count: 1, text: "补充", conversation_id: "c" }),
    );

    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().queued).toHaveLength(0),
    );
    let msgs = usePecoChatStore.getState().messages;
    expect(msgs.at(-2)).toMatchObject({ role: "user", content: "补充" });
    expect(msgs.at(-1)).toMatchObject({ role: "assistant", content: "" });

    // 新输出落在新占位，旧气泡内容不被改写
    s.push(frame("text_delta", { content: "新", conversation_id: "c" }));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().messages.at(-1)?.content).toBe("新"),
    );
    msgs = usePecoChatStore.getState().messages;
    expect(msgs.find((m) => m.content === "旧")).toBeTruthy();
    expect(usePecoChatStore.getState().paused).toBe(false);
  });

  it("多条排队项按 FIFO 合并成一条 user 消息", async () => {
    const s = await startTurn();
    await usePecoChatStore.getState().sendMessage("a", "test-token");
    await usePecoChatStore.getState().sendMessage("b", "test-token");

    s.push(
      frame("user_steer", { count: 2, text: "a\nb", conversation_id: "c" }),
    );
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().queued).toHaveLength(0),
    );

    const msgs = usePecoChatStore.getState().messages;
    expect(msgs.at(-2)).toMatchObject({ role: "user", content: "a\nb" });
    expect(msgs.at(-1)).toMatchObject({ role: "assistant", content: "" });
  });

  it("steer 气泡文本取服务端下发值，不再本地合并排队项", async () => {
    const s = await startTurn();
    await usePecoChatStore.getState().sendMessage("A", "test-token");
    await usePecoChatStore.getState().sendMessage("B", "test-token");

    // 故意与本地合并结果（"A\nB"）不同：本地合并回潮时这条断言会红
    s.push(
      frame("user_steer", {
        count: 2,
        text: "SERVER-A + B",
        conversation_id: "c",
      }),
    );
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().queued).toHaveLength(0),
    );

    const msgs = usePecoChatStore.getState().messages;
    expect(msgs.at(-2)).toMatchObject({
      role: "user",
      content: "SERVER-A + B",
    });
    expect(msgs.at(-1)).toMatchObject({ role: "assistant", content: "" });
  });

  it("user_steer 不带 text（旧服务端）时回退本地合并文本，且不落错", async () => {
    const s = await startTurn();
    await usePecoChatStore.getState().sendMessage("A", "test-token");
    await usePecoChatStore.getState().sendMessage("B", "test-token");

    // 旧服务端只下发 count：气泡内容若取不到 text 就会是 undefined，
    // 渲染层 `.trim()` 抛 TypeError，整条消息列表一起白屏。
    s.push(frame("user_steer", { count: 2, conversation_id: "c" }));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().queued).toHaveLength(0),
    );

    const st = usePecoChatStore.getState();
    expect(st.error).toBeNull();
    expect(st.messages.at(-2)).toMatchObject({
      role: "user",
      content: "A\nB",
      steered: true,
    });
    expect(st.messages.at(-1)).toMatchObject({
      role: "assistant",
      content: "",
    });
  });

  it("本轮收尾时仍有排队项：保持流式态并起下一轮占位（兜底）", async () => {
    const s = await startTurn();
    await usePecoChatStore.getState().sendMessage("补充", "test-token");

    s.push(turnCompleteFrame("旧"));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().queued).toHaveLength(0),
    );

    const st = usePecoChatStore.getState();
    expect(st.isStreaming).toBe(true);
    const msgs = st.messages;
    expect(msgs.at(-2)).toMatchObject({ role: "user", content: "补充" });
    expect(msgs.at(-1)).toMatchObject({ role: "assistant", content: "" });

    // 兜底轮的新输出落在占位
    s.push(frame("text_delta", { content: "下一轮", conversation_id: "c" }));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().messages.at(-1)?.content).toBe(
        "下一轮",
      ),
    );
  });

  it("兜底轮的合并气泡不标记 steered，耗时从落气泡时刻起算", async () => {
    // 这一轮的起点就是兜底落下来的 user 气泡：若标了 steered，本轮起点会退到
    // 上一轮的 user 时间戳，把上一轮时长也算进本轮。
    const t0 = 1_700_000_000_000;
    const now = vi.spyOn(Date, "now").mockReturnValue(t0);
    try {
      const s = await startTurn();

      // 排队时刻晚于本轮开始 10s：用户在本轮进行中敲下这句话
      const sentAt = t0 + 10_000;
      now.mockReturnValue(sentAt);
      await usePecoChatStore.getState().sendMessage("补充", "test-token");
      expect(usePecoChatStore.getState().queued[0]?.sentAt).toBe(sentAt);

      const fallbackAt = t0 + 30_000;
      now.mockReturnValue(fallbackAt);
      s.push(turnCompleteFrame("旧"));
      await vi.waitFor(() =>
        expect(usePecoChatStore.getState().queued).toHaveLength(0),
      );

      const fallbackMsg = usePecoChatStore.getState().messages.at(-2);
      expect(fallbackMsg).toMatchObject({ role: "user", content: "补充" });
      expect(fallbackMsg?.steered).toBeFalsy();
      expect(fallbackMsg?.timestampMs).toBe(fallbackAt);
      expect(fallbackMsg?.timestampMs).not.toBe(sentAt);
    } finally {
      now.mockRestore();
    }
  });

  it("非取消 error 且仍有排队项：同样按兜底轮保持流式态", async () => {
    const s = await startTurn();
    await usePecoChatStore.getState().sendMessage("补充", "test-token");

    s.push(frame("error", { message: "上游超时", conversation_id: "c" }));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().queued).toHaveLength(0),
    );

    const st = usePecoChatStore.getState();
    expect(st.isStreaming).toBe(true);
    expect(st.messages.at(-2)).toMatchObject({
      role: "user",
      content: "补充",
    });
    expect(st.messages.at(-1)).toMatchObject({
      role: "assistant",
      content: "",
    });
  });

  it("error.cancelled=true 走取消分支：不兜底成下一轮、不留排队项", async () => {
    const s = await startTurn();
    await usePecoChatStore.getState().sendMessage("补充", "test-token");
    const before = usePecoChatStore.getState().messages.length;

    // 文案本身不像取消：判定必须认服务端字段，否则会误按失败兜底起下一轮
    s.push(
      frame("error", {
        message: "上游超时",
        cancelled: true,
        conversation_id: "c",
      }),
    );
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().isStreaming).toBe(false),
    );

    const st = usePecoChatStore.getState();
    expect(st.queued).toHaveLength(0);
    // 失败气泡是唯一新增，没有兜底轮的 user 气泡与占位
    expect(st.messages).toHaveLength(before + 1);
    expect(st.messages.at(-1)).toMatchObject({
      role: "assistant",
      content: "上游超时",
      isError: true,
    });
  });

  it("取消类 error 清空排队项并回到终态（旧服务端无 cancelled 字段）", async () => {
    const s = await startTurn();
    await usePecoChatStore.getState().sendMessage("补充", "test-token");

    // 字段缺省 → 回退文案识别，仍须按取消处理
    s.push(frame("error", { message: "对话已被取消", conversation_id: "c" }));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().isStreaming).toBe(false),
    );
    expect(usePecoChatStore.getState().queued).toHaveLength(0);
  });

  it("POST 非 404 失败：排队项落回 user 气泡并暴露错误", async () => {
    await startTurn();
    vi.mocked(queryPecoStream).mockRejectedValueOnce({
      response: { status: 500 },
    });

    await usePecoChatStore.getState().sendMessage("补充", "test-token");

    // 输入框已被 handleSend 清空：撤回排队项后必须把它落回列表，否则输入彻底消失
    const st = usePecoChatStore.getState();
    expect(st.queued).toHaveLength(0);
    expect(st.isStreaming).toBe(false);
    expect(st.error).toBeTruthy();
    expect(st.messages.at(-1)).toMatchObject({ role: "user", content: "补充" });
  });

  it("连接已失效时继续发送：旧排队项落成 user 气泡，新消息正常追加", async () => {
    // 在途态尚存但连接已没了（极窄竞态）：排队项服务端已收下，本地必须留回显
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    usePecoChatStore.setState({
      isStreaming: true,
      queued: [
        { id: "queued-1", text: "旧排队", sentAt: 1 },
        { id: "queued-2", text: "旧排队二", sentAt: 2 },
      ],
    });

    await usePecoChatStore.getState().sendMessage("新消息", "test-token");

    const st = usePecoChatStore.getState();
    expect(st.queued).toHaveLength(0);
    expect(st.messages).toHaveLength(3);
    expect(st.messages[0]).toMatchObject({
      role: "user",
      content: "旧排队\n旧排队二",
    });
    expect(st.messages[1]).toMatchObject({ role: "user", content: "新消息" });
    expect(st.messages[2]).toMatchObject({ role: "assistant", content: "" });
    expect(st.isStreaming).toBe(true);
  });

  it("暂停 → 恢复 → 切分：排队项保持暂停态，恢复后 user_steer 才切分", async () => {
    const s = await startTurn();

    s.push(frame("looper_state", { paused: true, conversation_id: "c" }));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().paused).toBe(true),
    );

    await usePecoChatStore.getState().sendMessage("补充", "test-token");
    expect(usePecoChatStore.getState().queued).toHaveLength(1);
    // 排队不改暂停态：暂停由服务端权威回填，客户端不乐观置位
    expect(usePecoChatStore.getState().paused).toBe(true);

    s.push(frame("looper_state", { paused: false, conversation_id: "c" }));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().paused).toBe(false),
    );

    s.push(
      frame("user_steer", { count: 1, text: "补充", conversation_id: "c" }),
    );
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().queued).toHaveLength(0),
    );

    const st = usePecoChatStore.getState();
    expect(st.messages.at(-2)).toMatchObject({
      role: "user",
      content: "补充",
      steered: true,
    });
    expect(st.messages.at(-1)).toMatchObject({
      role: "assistant",
      content: "",
    });
    expect(st.paused).toBe(false);
  });

  it("连续两次 user_steer 各切一次：气泡按到达顺序落位", async () => {
    const s = await startTurn();
    await usePecoChatStore.getState().sendMessage("a", "test-token");
    await usePecoChatStore.getState().sendMessage("b", "test-token");

    s.push(
      frame("user_steer", { count: 1, text: "a", conversation_id: "c" }),
      frame("user_steer", { count: 1, text: "b", conversation_id: "c" }),
    );
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().queued).toHaveLength(0),
    );

    const msgs = usePecoChatStore.getState().messages;
    expect(msgs.slice(-4).map((m) => [m.role, m.content])).toEqual([
      ["user", "a"],
      ["assistant", ""],
      ["user", "b"],
      ["assistant", ""],
    ]);
  });
});

describe("settleQueue 队列清空唯一登记点", () => {
  const carried = [
    { id: "queued-1", text: "A", sentAt: 1 },
    { id: "queued-2", text: "B", sentAt: 2 },
  ];
  const messages = [
    { role: "assistant" as const, content: "旧", turnIndex: 0 },
  ];

  it("discard：只清队列，messages 原样透传（不加气泡、不加占位）", () => {
    expect(settleQueue("discard", carried, messages)).toEqual({
      messages,
      queued: [],
    });
  });

  it("next-turn：合并成一条 fallback 气泡 + 空占位，并清空队列", () => {
    const out = settleQueue("next-turn", carried, messages);

    expect(out.messages.map((m) => [m.role, m.content])).toEqual([
      ["assistant", "旧"],
      ["user", "A\nB"],
      ["assistant", ""],
    ]);
    // 兜底气泡是下一轮自己的起点，不能标 steered（否则耗时起点会退到上一轮）
    expect(out.messages[1].steered).toBeUndefined();
    expect(out.queued).toEqual([]);
  });

  it("carry-only：只落 fallback 气泡，占位留给调用方自己追加", () => {
    const out = settleQueue("carry-only", carried, messages);

    // 长度精确 +1：这里若也落占位就会是 +2，成为没有内容落进去的孤儿气泡
    expect(out.messages).toHaveLength(messages.length + 1);
    expect(out.messages.map((m) => [m.role, m.content])).toEqual([
      ["assistant", "旧"],
      ["user", "A\nB"],
    ]);
    expect(out.queued).toEqual([]);
  });

  it("carry-only：无 carried 时 messages 原样透传，只清空队列", () => {
    const out = settleQueue("carry-only", [], messages);

    expect(out.messages).toBe(messages);
    expect(out.queued).toEqual([]);
  });
});

// 与服务端 crates/peco-core/src/session/types.rs 的 merge_contents /
// strip_merge_markers 对照：期望字符串是手算的「合并 → strip」结果。
// 逐字对齐是 refreshSession 去重判据的前提（文本不等 → 重复渲染）。
describe("mergeQueuedText 对齐服务端合并口径", () => {
  it("两条普通文本以单个换行连接", () => {
    expect(mergeQueuedText(["A", "B"])).toBe("A\nB");
  });

  it("首尾空行由标记接缝吸收，行内空白保留", () => {
    // merge: "---\n\n A \n\n---\n B " → strip 只去 trim 为空的整行
    expect(mergeQueuedText(["\n A \n", " B "])).toBe(" A \n B ");
  });

  it("文本自带的 --- 行同样被当作分段标记剥离", () => {
    // merge: "---\nA\n---\nC\n---\nB" → 三段，中间标记行消失
    expect(mergeQueuedText(["A\n---\nC", "B"])).toBe("A\nC\nB");
  });

  it("单条文本逐字保留（服务端单条不 strip）", () => {
    expect(mergeQueuedText(["A\n---\nB"])).toBe("A\n---\nB");
  });

  it("附件-only 排队项（空文本）不参与展示文本", () => {
    // merge: "---\n\n---\nB" → 空段被丢弃
    expect(mergeQueuedText(["", "B"])).toBe("B");
  });
});
