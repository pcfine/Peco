// SSE 产物接线 + 登出清理（跨两个 store，故独立成文件）
//
// 回归重点：产物增量在 `applyStreamEvent` 的"本窗口是否有在途轮次"门控**之前**
// 落库 —— 连接是跨窗口广播扇出，别处窗口生成的产物也应即时进本窗口面板。

import { beforeEach, describe, expect, it, vi } from "vitest";
import api from "@/api/client";

vi.mock("@/api/peco", () => ({
  pecoStreamUrl: (m: string) => `/api/peco/stream?message=${encodeURIComponent(m)}`,
  pecoAttachUrl: () => "/api/peco/stream",
  queryPecoStream: vi.fn(async () => undefined),
  cancelPecoStream: vi.fn(async () => ({ success: true })),
  pausePecoStream: vi.fn(async () => ({ success: true })),
  resumePecoStream: vi.fn(async () => ({ success: true })),
  clearPecoSession: vi.fn(async () => ({ success: true })),
  getPecoSession: vi.fn(),
  listPecoArtifacts: vi.fn(),
}));

import { listPecoArtifacts } from "@/api/peco";
import { __testCacheStats, getArtifactObjectUrl, releaseAll } from "@/api/artifactBlob";
import { useAuthStore } from "../authStore";
import { __resetPecoStreamForTests, usePecoChatStore } from "../pecoChatStore";
import { usePecoArtifactStore } from "../pecoArtifactStore";
import type { ArtifactItem } from "@/types/artifact";

function item(id: string): ArtifactItem {
  return {
    id,
    conversation_id: "c1",
    kind: "image",
    mime: "image/png",
    title: `${id}.png`,
    size_bytes: 1024,
    url: `/api/artifacts/${id}/content`,
    download_url: `/api/artifacts/${id}/download`,
    created_at: "2026-10-20 12:00:00",
    available: true,
  };
}

function frame(event: string, data: unknown): string {
  return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}

function toolResultFrame(artifacts: ArtifactItem[]): string {
  return frame("tool_result", {
    id: "call-1",
    name: "write_work_file",
    result: "已写入",
    conversation_id: "c",
    artifacts,
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
  };
}

const artifactIds = () =>
  usePecoArtifactStore.getState().ingested.map((a) => a.id);

beforeEach(() => {
  vi.clearAllMocks();
  __resetPecoStreamForTests();
  usePecoArtifactStore.getState().__resetForTests();
  releaseAll();
  usePecoChatStore.setState({
    loaded: false,
    messages: [],
    isStreaming: false,
    paused: false,
    error: null,
    usage: null,
  });
  useAuthStore.setState({ token: "test-token", user: null });
  URL.createObjectURL = vi.fn(() => "blob:artifact-1");
  URL.revokeObjectURL = vi.fn();
  vi.spyOn(api, "get").mockResolvedValue({
    data: { size: 1024, type: "image/png" },
    headers: { "content-type": "image/png" },
  } as never);
});

describe("SSE → 产物 store", () => {
  it("流式中收到 tool_result 的产物即落进产物 store", async () => {
    const stream = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(stream.response));

    await usePecoChatStore.getState().sendMessage("画一张图", "test-token");
    stream.push(toolResultFrame([item("p1")]));

    await vi.waitFor(() => expect(artifactIds()).toEqual(["p1"]));
    // 变异点：删掉旁路的 ingest → 事件根本不落库
    expect(usePecoArtifactStore.getState().total).toBe(1);
  });

  it("本窗口无在途轮次时产物仍落库，但消息侧不受影响", async () => {
    const stream = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(stream.response));

    await usePecoChatStore.getState().sendMessage("问", "test-token");
    stream.push(
      frame("turn_complete", {
        text: "答",
        usage: { input_tokens: 1, output_tokens: 1 },
        conversation_id: "c",
      }),
    );
    await vi.waitFor(() =>
      expect(usePecoChatStore.getState().isStreaming).toBe(false),
    );

    // 其他窗口在本会话里生成的产物：本窗口无在途轮次，仍应进面板
    stream.push(toolResultFrame([item("p2")]));

    await vi.waitFor(() => expect(artifactIds()).toEqual(["p2"]));
    // 变异点：把 ingest 移到 `if (!get().isStreaming) return;` 之后 → 被早退吞掉
    expect(usePecoChatStore.getState().isStreaming).toBe(false);
    // 消息侧仍受门控：没有工具调用记录被写进气泡
    const last = usePecoChatStore.getState().messages.at(-1);
    expect(last?.toolCalls).toBeUndefined();
  });
});

describe("logout() 清理产物与 blob", () => {
  it("登出时 load 仍在飞：落地响应不写回旧数据", async () => {
    let resolvePage!: (value: {
      artifacts: ArtifactItem[];
      total: number;
      limit: number;
      offset: number;
    }) => void;
    vi.mocked(listPecoArtifacts).mockReturnValue(
      new Promise((resolve) => {
        resolvePage = resolve;
      }),
    );

    const loading = usePecoArtifactStore.getState().load();
    useAuthStore.getState().logout();
    resolvePage({ artifacts: [item("p1")], total: 1, limit: 1, offset: 0 });
    await loading;

    const s = usePecoArtifactStore.getState();
    // 变异点：去掉代际校验 → 旧账号这一页写回 pages，loaded 变 true，新账号 load() 成 no-op
    expect(s.pages).toEqual([]);
    expect(s.loaded).toBe(false);
    expect(s.total).toBe(0);
  });

  it("登出后列表为空、已取到的 object URL 被撤销", async () => {
    vi.mocked(listPecoArtifacts).mockResolvedValue({
      artifacts: [item("p1"), item("p2")],
      total: 2,
      limit: 2,
      offset: 0,
    });
    await usePecoArtifactStore.getState().load();
    await getArtifactObjectUrl(item("p1"));
    expect(__testCacheStats().entries).toBe(1);

    useAuthStore.getState().logout();

    const s = usePecoArtifactStore.getState();
    // 变异点：去掉 logout() 里的 reset() → 换账号后仍看到上一账号的产物
    expect(s.ingested).toEqual([]);
    expect(s.pages).toEqual([]);
    expect(s.total).toBe(0);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:artifact-1");
    expect(__testCacheStats().entries).toBe(0);
  });
});

describe("clear() 不碰产物 store", () => {
  it("清空对话后产物列表与已取到的 blob 原样保留", async () => {
    vi.mocked(listPecoArtifacts).mockResolvedValue({
      artifacts: [item("p1"), item("p2")],
      total: 2,
      limit: 2,
      offset: 0,
    });
    await usePecoArtifactStore.getState().load();
    await getArtifactObjectUrl(item("p1"));
    const pagesBefore = usePecoArtifactStore.getState().pages;
    expect(pagesBefore).toHaveLength(2);

    await usePecoChatStore.getState().clear();

    const s = usePecoArtifactStore.getState();
    // 清空对话既不删产物行也不删文件：列表本就该原样保留（重置再重拉只会闪一下）
    // 变异点：在 clear() 里加 reset() → pages 变空、blob 被 revoke
    expect(s.pages).toBe(pagesBefore);
    expect(s.total).toBe(2);
    expect(s.loaded).toBe(true);
    expect(URL.revokeObjectURL).not.toHaveBeenCalled();
    // 不重拉第一页
    expect(listPecoArtifacts).toHaveBeenCalledTimes(1);
  });
});
