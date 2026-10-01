// 回归：发送后 query 瞬间消失，任务完成后才出现。
//
// 根因：服务端快照只暴露 committed_turns（在途轮次不在其中，见
// peco/handler.rs 的 SessionSnapshotResponse 组装），而 refreshSession 的
// 「保留旧页」合并分支只认有 id 的消息，sendMessage 造的乐观 userMsg 恒无
// id —— 于是任何一次「本轮尚未落盘」时的追平（意外断流自愈 / 切窗获焦）
// 都会把刚发的 query 抹掉，直到该轮 committed 后才由快照带回。
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@/api/peco", () => ({
  pecoStreamUrl: (m: string) => `/api/peco/stream?message=${encodeURIComponent(m)}`,
  pecoAttachUrl: () => "/api/peco/stream",
  queryPecoStream: vi.fn(async () => undefined),
  cancelPecoStream: vi.fn(async () => ({ success: true })),
  clearPecoSession: vi.fn(async () => ({ success: true })),
  getPecoSession: vi.fn(),
}));

import { usePecoChatStore, __resetPecoStreamForTests } from "../pecoChatStore";
import { snapshotToMessages } from "../../components/chat/ChatView";
import { useAuthStore } from "../../stores/authStore";
import { getPecoSession } from "@/api/peco";

const turn = (turn_index: number, text: string) => ({
  turn_index,
  messages: [
    { role: "user", content: text, timestamp_ms: turn_index * 10 },
    { role: "assistant", content: `${text}-答`, timestamp_ms: turn_index * 10 + 1 },
  ],
});

const pagedSnapshot = (opts: {
  turns: ReturnType<typeof turn>[];
  compaction_count?: number;
  total_turns?: number;
  has_more?: boolean;
  turn_in_flight?: boolean;
  inflight_user_input?: string;
}) => ({
  conversation_id: "u1-private-session",
  turns: opts.turns,
  total_usage: { input_tokens: 10, output_tokens: 5 },
  is_running: true,
  turn_in_flight: opts.turn_in_flight ?? false,
  ...(opts.inflight_user_input !== undefined
    ? { inflight_user_input: opts.inflight_user_input }
    : {}),
  ...(opts.compaction_count !== undefined
    ? { context_metrics: { compaction_count: opts.compaction_count } }
    : {}),
  ...(opts.total_turns !== undefined ? { total_turns: opts.total_turns } : {}),
  ...(opts.has_more !== undefined ? { has_more: opts.has_more } : {}),
});

function controllableStream() {
  const encoder = new TextEncoder();
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const stream = new ReadableStream<Uint8Array>({ start(c) { controller = c; } });
  return {
    response: new Response(stream, { status: 200 }),
    push: (...frames: string[]) => { for (const f of frames) controller.enqueue(encoder.encode(f)); },
    close: () => controller.close(),
  };
}

const frame = (event: string, data: unknown) =>
  `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;

const seedLoadedWindow = () =>
  usePecoChatStore.setState({
    loaded: true,
    loading: false,
    messages: snapshotToMessages([turn(2, "t2"), turn(3, "t3"), turn(4, "t4")]),
    sessionKey: 0,
    isStreaming: false,
    error: null,
    usage: null,
    oldestLoadedTurn: 2,
    hasMore: true,
    totalTurns: 5,
    historyRevision: 0,
    loadingEarlier: false,
  });

beforeEach(() => {
  vi.clearAllMocks();
  __resetPecoStreamForTests();
  usePecoChatStore.setState({
    loaded: false, loading: false, messages: [], sessionKey: 0, isStreaming: false,
    error: null, usage: null, oldestLoadedTurn: null, hasMore: false,
    totalTurns: 0, historyRevision: 0, loadingEarlier: false,
  });
  useAuthStore.setState({ token: "test-token" });
  global.fetch = vi.fn();
});

const QUERY = "我刚发的问题";

describe("pecoChatStore 保留在途轮次的本地 query", () => {
  it("意外断流（无 done）自愈追平时，刚发的 query 不被抹掉", async () => {
    seedLoadedWindow();
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    // 本轮尚未 committed —— 快照尾部只到 turn 4，且服务端仍报在途
    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(3, "t3"), turn(4, "t4")],
        compaction_count: 0, total_turns: 5, has_more: true, turn_in_flight: true,
      }) as never,
    );

    await usePecoChatStore.getState().sendMessage(QUERY, "test-token");
    expect(usePecoChatStore.getState().messages.some((m) => m.content === QUERY)).toBe(true);

    // 意外断流：无 done、未 abort
    s.close();
    await vi.waitFor(() => expect(getPecoSession).toHaveBeenCalled());
    await new Promise((r) => setTimeout(r, 30));

    const msgs = usePecoChatStore.getState().messages;
    expect(msgs.some((m) => m.content === QUERY)).toBe(true);
    // 顺序：历史轮在前、刚发的 query 落在尾部
    expect(msgs[msgs.length - 2]).toMatchObject({ role: "user", content: QUERY });
  });

  it("收到 turn_complete 后（本轮落盘滞后）的获焦追平，query 不消失", async () => {
    seedLoadedWindow();
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(3, "t3"), turn(4, "t4")],
        compaction_count: 0, total_turns: 5, has_more: true, turn_in_flight: false,
      }) as never,
    );

    await usePecoChatStore.getState().sendMessage(QUERY, "test-token");
    s.push(frame("turn_complete", { text: "答", usage: { input_tokens: 1, output_tokens: 1 }, conversation_id: "c" }));
    await vi.waitFor(() => expect(usePecoChatStore.getState().isStreaming).toBe(false));

    await usePecoChatStore.getState().refresh();
    expect(usePecoChatStore.getState().messages.some((m) => m.content === QUERY)).toBe(true);
  });

  it("整页刷新（内存全清）后，在途轮的 query 由快照 inflight_user_input 恢复", async () => {
    // 冷启动：不 seed —— 模拟 F5 后前端内存全清，一切以服务端快照为准
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(3, "t3"), turn(4, "t4")],
        compaction_count: 0, total_turns: 5, has_more: true, turn_in_flight: true,
        inflight_user_input: QUERY,
      }) as never,
    );

    await usePecoChatStore.getState().load();

    const msgs = usePecoChatStore.getState().messages;
    // query 在历史轮之后、assistant 占位之前 —— 恰是它被发出时的位置
    expect(msgs.filter((m) => m.content === QUERY)).toHaveLength(1);
    expect(msgs[msgs.length - 2]).toMatchObject({ role: "user", content: QUERY });
    expect(msgs[msgs.length - 1]).toMatchObject({ role: "assistant", content: "" });
  });

  it("有本地乐观副本时，快照 inflight_user_input 不造成重复 query", async () => {
    seedLoadedWindow();
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(3, "t3"), turn(4, "t4")],
        compaction_count: 0, total_turns: 5, has_more: true, turn_in_flight: true,
        inflight_user_input: QUERY,
      }) as never,
    );

    await usePecoChatStore.getState().sendMessage(QUERY, "test-token");
    // 多轮任务的轮间窗口：turn_complete 已到、本轮仍在途，此刻获焦追平
    s.push(frame("turn_complete", { text: "答", usage: { input_tokens: 1, output_tokens: 1 }, conversation_id: "c" }));
    await vi.waitFor(() => expect(usePecoChatStore.getState().isStreaming).toBe(false));

    await usePecoChatStore.getState().refresh();
    const msgs = usePecoChatStore.getState().messages;
    // 本地副本与快照字段并存 → 只能渲染一条
    expect(msgs.filter((m) => m.content === QUERY)).toHaveLength(1);
    expect(msgs[msgs.length - 2]).toMatchObject({ role: "user", content: QUERY });
  });

  it("本轮已 committed 时以快照为准：本地乐观副本被去重、不重复渲染", async () => {
    seedLoadedWindow();
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    // 快照已含本轮（turn 5）—— 本地乐观副本必须被丢弃
    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(3, "t3"), turn(4, "t4"), turn(5, QUERY)],
        compaction_count: 0, total_turns: 6, has_more: true, turn_in_flight: false,
      }) as never,
    );

    await usePecoChatStore.getState().sendMessage(QUERY, "test-token");
    s.push(frame("turn_complete", { text: "答", usage: { input_tokens: 1, output_tokens: 1 }, conversation_id: "c" }));
    await vi.waitFor(() => expect(usePecoChatStore.getState().isStreaming).toBe(false));

    await usePecoChatStore.getState().refresh();
    const msgs = usePecoChatStore.getState().messages;
    // 恰好一条 —— 本地无 id 副本被快照权威版本取代，未成对重复
    expect(msgs.filter((m) => m.content === QUERY)).toHaveLength(1);
    // 且落在快照的 turn 位置（5），未错位
    expect(msgs.find((m) => m.content === QUERY)?.turnIndex).toBe(5);
  });

  it("重复发送同一句话（文本已在尾部窗口出现）时，在途那条仍恢复、不误判为已落盘", async () => {
    // 冷启动。历史里已有「继续」，本轮用户又发「继续」—— 若去重拿整个窗口比对，
    // 在途轮会被历史里的同文本吃掉，query 再次消失（回归自 —— 见 git 记录）。
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(2, "继续"), turn(3, "帮我查X"), turn(4, "结果呢")],
        compaction_count: 0, total_turns: 5, has_more: true, turn_in_flight: true,
        inflight_user_input: "继续",
      }) as never,
    );

    await usePecoChatStore.getState().load();
    const msgs = usePecoChatStore.getState().messages;
    // 历史那条 + 在途那条，共两条；在途的落在 assistant 占位之前
    expect(msgs.filter((m) => m.content === "继续")).toHaveLength(2);
    expect(msgs[msgs.length - 2]).toMatchObject({ role: "user", content: "继续" });
    expect(msgs[msgs.length - 1]).toMatchObject({ role: "assistant", content: "" });
  });

  it("连着跑完两轮再追平：快照已含两轮，不得成对重复", async () => {
    // 本地尾部跨多轮（上一轮 turn_complete 后没追平就又发了一条）：只保留最新一段。
    seedLoadedWindow();
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(4, "t4"), turn(5, "第一轮"), turn(6, "第二轮")],
        compaction_count: 0, total_turns: 7, has_more: true, turn_in_flight: false,
      }) as never,
    );
    const done = frame("turn_complete", {
      text: "答", usage: { input_tokens: 1, output_tokens: 1 }, conversation_id: "c",
    });

    await usePecoChatStore.getState().sendMessage("第一轮", "test-token");
    s.push(done);
    await vi.waitFor(() => expect(usePecoChatStore.getState().isStreaming).toBe(false));
    await usePecoChatStore.getState().sendMessage("第二轮", "test-token");
    s.push(done);
    await vi.waitFor(() => expect(usePecoChatStore.getState().isStreaming).toBe(false));

    await usePecoChatStore.getState().refresh();
    const msgs = usePecoChatStore.getState().messages;
    expect(msgs.filter((m) => m.content === "第一轮")).toHaveLength(1);
    expect(msgs.filter((m) => m.content === "第二轮")).toHaveLength(1);
    // 两轮都取快照权威版本（有 id），未留下本地副本
    expect(msgs.find((m) => m.content === "第二轮")?.turnIndex).toBe(6);
  });

  it("本地尾部跨两轮、只有上一轮落盘时：上一轮取快照、最末一轮取本地，各一条", async () => {
    seedLoadedWindow();
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));
    vi.mocked(getPecoSession).mockResolvedValue(
      pagedSnapshot({
        turns: [turn(4, "t4"), turn(5, "第一轮")],
        compaction_count: 0, total_turns: 6, has_more: true, turn_in_flight: true,
        inflight_user_input: "第二轮",
      }) as never,
    );
    const done = frame("turn_complete", {
      text: "答", usage: { input_tokens: 1, output_tokens: 1 }, conversation_id: "c",
    });

    await usePecoChatStore.getState().sendMessage("第一轮", "test-token");
    s.push(done);
    await vi.waitFor(() => expect(usePecoChatStore.getState().isStreaming).toBe(false));
    await usePecoChatStore.getState().sendMessage("第二轮", "test-token");
    s.push(done);
    await vi.waitFor(() => expect(usePecoChatStore.getState().isStreaming).toBe(false));

    await usePecoChatStore.getState().refresh();
    const msgs = usePecoChatStore.getState().messages;
    // 第一轮已落盘 → 快照权威；第二轮仍在途 → 本地副本，且不与注入的同一句重复
    expect(msgs.filter((m) => m.content === "第一轮")).toHaveLength(1);
    expect(msgs.filter((m) => m.content === "第二轮")).toHaveLength(1);
    expect(msgs.find((m) => m.content === "第一轮")?.turnIndex).toBe(5);
    expect(msgs[msgs.length - 2]).toMatchObject({ role: "user", content: "第二轮" });
    expect(msgs[msgs.length - 1]).toMatchObject({ role: "assistant", content: "" });
  });
});
