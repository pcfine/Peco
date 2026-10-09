// pecoChatStore 附件发送通路 — 文档随 file_ids 开流、控制通道不吞附件、快照映射
//
// 回归重点：POST /stream/query 控制通道只承载文本 —— 带附件的消息必须绕开它
// 走开流路径，否则文档会被静默丢弃。

import { beforeEach, describe, expect, it, vi } from "vitest";
import { usePecoChatStore, __resetPecoStreamForTests } from "../pecoChatStore";
import { snapshotToMessages } from "../../components/chat/ChatView";
import { useAuthStore } from "../../stores/authStore";

vi.mock("@/api/peco", () => ({
  pecoStreamUrl: (m: string, imageIds: string[] = [], fileIds: string[] = []) =>
    `/api/peco/stream?message=${encodeURIComponent(m)}` +
    (imageIds.length > 0 ? `&image_ids=${imageIds.join(",")}` : "") +
    (fileIds.length > 0 ? `&file_ids=${fileIds.join(",")}` : ""),
  uploadPecoImage: vi.fn(),
  uploadPecoFile: vi.fn(),
  pecoAttachUrl: () => "/api/peco/stream",
  queryPecoStream: vi.fn(async () => undefined),
  cancelPecoStream: vi.fn(async () => ({ success: true })),
  clearPecoSession: vi.fn(async () => ({ success: true })),
  getPecoSession: vi.fn(),
}));

import { queryPecoStream } from "@/api/peco";
import type { ChatAttachment } from "../../components/chat/ChatView";

const DOC: ChatAttachment = { id: "f1", kind: "document", name: "笔记.txt" };
const IMG: ChatAttachment = {
  id: "a.png",
  kind: "image",
  name: "a.png",
  url: "/uploads/chat/u/a.png",
};

function frame(event: string, data: unknown): string {
  return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}

/** 服务端确认一轮结束（清 isStreaming，连接保持打开）。 */
function turnCompleteFrame(text: string): string {
  return frame("turn_complete", {
    text,
    usage: { input_tokens: 1, output_tokens: 1 },
    conversation_id: "c",
  });
}

/** 可手动推送事件、手动关闭的 SSE 流。 */
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

/** 全部 fetch 调用中命中 peco SSE 端点的那些。 */
function streamCalls(): { url: string; init?: RequestInit }[] {
  const mock = global.fetch as unknown as {
    mock: { calls: [string, RequestInit?][] };
  };
  return mock.mock.calls
    .map(([url, init]) => ({ url, init }))
    .filter((c) => c.url.startsWith("/api/peco/stream"));
}

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

describe("pecoChatStore 附件发送", () => {
  it("文档附件随 file_ids 开流（不经 POST 控制通道）", async () => {
    global.fetch = vi.fn(() => Promise.resolve(controllableStream().response));

    await usePecoChatStore
      .getState()
      .sendMessage("总结这个文档", "test-token", [DOC]);

    expect(queryPecoStream).not.toHaveBeenCalled();
    const calls = streamCalls();
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toContain("file_ids=f1");
    expect(calls[0].url).not.toContain("image_ids");

    // 乐观用户消息携带文档附件名（正文不进展示层）
    const lastUser = [...usePecoChatStore.getState().messages]
      .reverse()
      .find((m) => m.role === "user");
    expect(lastUser?.attachments).toEqual([{ name: "笔记.txt" }]);
    expect(lastUser?.images).toBeUndefined();
  });

  it("已有活连接时，带附件消息仍开新流 —— 控制通道分支不吞附件", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));

    await usePecoChatStore.getState().sendMessage("第一条", "test-token");
    s.push(turnCompleteFrame("第一条"));
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().isStreaming).toBe(false),
    );
    expect(streamCalls()).toHaveLength(1);

    await usePecoChatStore
      .getState()
      .sendMessage("换篇文档", "test-token", [DOC]);

    // 关键回归：已有活连接也不得走 POST —— 控制通道只承载文本
    expect(queryPecoStream).not.toHaveBeenCalled();
    const calls = streamCalls();
    expect(calls).toHaveLength(2);
    expect(calls[1].url).toContain("message=");
    expect(calls[1].url).toContain("file_ids=f1");
  });

  it("附件-only（无文本）也发起发送", async () => {
    global.fetch = vi.fn(() => Promise.resolve(controllableStream().response));

    await usePecoChatStore.getState().sendMessage("", "test-token", [DOC]);

    expect(streamCalls()).toHaveLength(1);
    const messages = usePecoChatStore.getState().messages;
    expect(messages[0]).toMatchObject({
      role: "user",
      content: "",
      attachments: [{ name: "笔记.txt" }],
    });
    expect(usePecoChatStore.getState().isStreaming).toBe(true);
  });

  it("图片 + 文档混排：image_ids 与 file_ids 分路携带", async () => {
    global.fetch = vi.fn(() => Promise.resolve(controllableStream().response));

    await usePecoChatStore
      .getState()
      .sendMessage("看这张图并读这份文档", "test-token", [IMG, DOC]);

    const calls = streamCalls();
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toContain("image_ids=a.png");
    expect(calls[0].url).toContain("file_ids=f1");

    const lastUser = [...usePecoChatStore.getState().messages]
      .reverse()
      .find((m) => m.role === "user");
    expect(lastUser?.images).toEqual(["/uploads/chat/u/a.png"]);
    expect(lastUser?.attachments).toEqual([{ name: "笔记.txt" }]);
  });
});

describe("pecoChatStore 快照附件映射", () => {
  it("snapshotToMessages 把 DTO attachments 映射到用户消息", () => {
    const messages = snapshotToMessages([
      {
        turn_index: 0,
        messages: [
          {
            role: "user",
            content: "总结这个文档",
            attachments: [{ name: "笔记.txt" }],
            timestamp_ms: 1,
          },
          { role: "assistant", content: "要点如下", timestamp_ms: 2 },
        ],
      },
    ]);

    expect(messages[0].attachments).toEqual([{ name: "笔记.txt" }]);
    expect(messages[0].content).toBe("总结这个文档");
  });

  it("无附件的用户消息 attachments 保持缺省", () => {
    const messages = snapshotToMessages([
      {
        turn_index: 0,
        messages: [{ role: "user", content: "纯文本", timestamp_ms: 1 }],
      },
    ]);

    expect(messages[0].attachments).toBeUndefined();
  });
});
