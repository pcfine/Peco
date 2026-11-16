import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TooltipProvider } from "@/components/ui/tooltip";
import { SpaceChatShell } from "@/components/spaces/SpaceChatShell";

// 壳层的接线只有一件事要盯住：回到对话时**重新订阅在途轮**，而不是重发。
// 隐藏的壳不握连接（切空间不必为每个后台空间维持一条出口），一回到可见就
// 请求 `/stream/live` —— 服务端 204 表示没有在途轮，历史即权威。

const mocks = vi.hoisted(() => ({ getMessages: vi.fn() }));

vi.mock("@/api/conversations", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/api/conversations")>();
  return {
    ...actual,
    getMessages: (...args: unknown[]) => mocks.getMessages(...args),
    uploadConversationImage: async () => ({ id: "img_1", url: "/x.png" }),
  };
});

vi.mock("@/queries/spaceChat", () => ({
  SPACE_MASTER_AGENT: "@master",
  useSpaceConversation: () => ({ data: { conversation_id: "conv_1" } }),
}));

vi.mock("@/queries/spaceDetail", () => ({
  useInflightTaskCount: () => 0,
  useSpaceTasks: () => ({ data: [] }),
}));

const fetchMock = vi.fn<typeof fetch>();

function shell(visible: boolean) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return (
    <QueryClientProvider client={client}>
      <TooltipProvider>
        <SpaceChatShell wsId="ws_a" visible={visible} teamId={null} />
      </TooltipProvider>
    </QueryClientProvider>
  );
}

beforeEach(() => {
  mocks.getMessages.mockReset();
  mocks.getMessages.mockResolvedValue([]);
  fetchMock.mockReset();
  // 没有在途轮：204 是「历史即权威」的充分信号
  fetchMock.mockImplementation(async () => new Response(null, { status: 204 }));
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("空间聊天壳的在途轮订阅", () => {
  it("切回可见时重新订阅（回来靠 attach 收敛，不靠重发）", async () => {
    const { rerender } = render(shell(false));

    // 历史请求照发（隐藏只是不握在途轮的连接）；落定后仍不订阅
    await waitFor(() => expect(mocks.getMessages).toHaveBeenCalled());
    expect(fetchMock).not.toHaveBeenCalled();

    rerender(shell(true));

    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        expect.stringContaining("/conversations/conv_1/stream/live?ws=ws_a"),
        expect.anything(),
      ),
    );
  });

  it("历史还没落定就不发订阅请求（历史请求必须先于订阅请求）", async () => {
    // 历史永远不返回：isFetched 为假，attach 不该抢跑
    mocks.getMessages.mockImplementation(() => new Promise(() => {}));
    render(shell(true));

    await waitFor(() => expect(mocks.getMessages).toHaveBeenCalled());
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
