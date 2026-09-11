// pecoChatStore 附着/取消语义单测 — is_running 重新附着、占位消息、服务端取消

import { beforeEach, describe, expect, it, vi } from "vitest";
import { usePecoChatStore } from "../pecoChatStore";
import { useAuthStore } from "../../stores/authStore";

vi.mock("@/api/peco", () => ({
  pecoStreamUrl: (m: string) =>
    `/api/peco/stream?message=${encodeURIComponent(m)}`,
  pecoAttachUrl: () => "/api/peco/stream",
  cancelPecoStream: vi.fn(async () => ({ success: true })),
  clearPecoSession: vi.fn(async () => ({ success: true })),
  getPecoSession: vi.fn(),
}));

import { getPecoSession, cancelPecoStream } from "@/api/peco";

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

const snapshot = (isRunning: boolean) => ({
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
  is_running: isRunning,
});

const resetStore = () =>
  usePecoChatStore.setState({
    loaded: false,
    loading: false,
    messages: [],
    sessionKey: 0,
    isStreaming: false,
    error: null,
    usage: null,
  });

beforeEach(() => {
  vi.clearAllMocks();
  resetStore();
  useAuthStore.setState({ token: "test-token" });
  global.fetch = vi.fn();
});

describe("pecoChatStore load() reattach", () => {
  it("is_running=true 时追加占位并重新附着，delta 落在占位消息上", async () => {
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

  it("is_running=false 时不发起附着请求", async () => {
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

describe("pecoChatStore abortStream()", () => {
  it("调用服务端取消并复位 isStreaming", async () => {
    vi.mocked(getPecoSession).mockResolvedValue(snapshot(true) as never);
    global.fetch = vi.fn(() => Promise.resolve(sseResponse([], true)));

    await usePecoChatStore.getState().load();
    expect(usePecoChatStore.getState().isStreaming).toBe(true);

    usePecoChatStore.getState().abortStream();

    expect(cancelPecoStream).toHaveBeenCalledTimes(1);
    expect(usePecoChatStore.getState().isStreaming).toBe(false);
  });
});

describe("pecoChatStore refresh()", () => {
  it("追平其他窗口的进度：恢复新落盘轮次并附着进行中任务", async () => {
    // 初始载入时无进行中任务
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
});
