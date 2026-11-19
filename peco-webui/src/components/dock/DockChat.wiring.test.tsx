import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TooltipProvider } from "@/components/ui/tooltip";
import { DockChat } from "@/components/dock/DockChat";

// dock 与页面壳是同一个 ChatView：在途轮的三条命令少接一条，就有一边没有
// 暂停按钮 / 在途说不了话。断言落在真正的调用参数上。

const mocks = vi.hoisted(() => ({
  steer: vi.fn(async (..._args: unknown[]) => "accepted" as const),
  pause: vi.fn(async (..._args: unknown[]) => ({ success: true })),
  resume: vi.fn(async (..._args: unknown[]) => ({ success: true })),
}));

vi.mock("@/api/conversations", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/api/conversations")>();
  return {
    ...actual,
    getMessages: async () => [],
    conversationStreamUrl: () => "/api/chat/agent/conversations/conv_1/stream",
    conversationLiveStreamUrl: () =>
      "/api/chat/agent/conversations/conv_1/stream/live",
    uploadConversationImage: async () => ({ id: "img", url: "/img" }),
    steerConversationTurn: (...args: unknown[]) => mocks.steer(...args),
    pauseConversation: (...args: unknown[]) => mocks.pause(...args),
    resumeConversation: (...args: unknown[]) => mocks.resume(...args),
  };
});

const fetchMock = vi.fn<typeof fetch>();

/** 发送流留给测试推帧：轮一直在跑，才测得了「在途」。 */
function inFlightStream() {
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const body = new ReadableStream<Uint8Array>({
    start(next) {
      controller = next;
    },
  });
  return {
    response: new Response(body, { status: 200 }),
    push: async (text: string) => {
      await act(async () => {
        controller.enqueue(new TextEncoder().encode(text));
      });
    },
  };
}

function renderDockChat() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <TooltipProvider>
        <DockChat agentId="agent_1" convId="conv_1" wsId="ws_peco" />
      </TooltipProvider>
    </QueryClientProvider>,
  );
}

async function startTurn(stream: ReturnType<typeof inFlightStream>) {
  fetchMock.mockImplementation(async (input) =>
    String(input).includes("/stream/live")
      ? new Response(null, { status: 204 })
      : stream.response,
  );
  renderDockChat();
  await waitFor(() => expect(fetchMock).toHaveBeenCalled());

  const textbox = screen.getByRole("textbox");
  fireEvent.change(textbox, { target: { value: "开始跑" } });
  fireEvent.keyDown(textbox, { key: "Enter" });
  await waitFor(() =>
    expect(fetchMock).toHaveBeenCalledWith(
      expect.stringContaining("/conversations/conv_1/stream"),
      expect.anything(),
    ),
  );
}

beforeEach(() => {
  mocks.steer.mockClear();
  mocks.pause.mockClear();
  mocks.resume.mockClear();
  fetchMock.mockReset();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("dock 的在途轮命令接线", () => {
  it("在途发送进当前会话的 pending 队列，带上空间作用域", async () => {
    const stream = inFlightStream();
    await startTurn(stream);

    const textbox = screen.getByRole("textbox");
    fireEvent.change(textbox, { target: { value: "顺便看下 lint" } });
    fireEvent.keyDown(textbox, { key: "Enter" });

    await waitFor(() =>
      expect(mocks.steer).toHaveBeenCalledWith(
        "agent_1",
        "conv_1",
        "顺便看下 lint",
        { imageIds: [], wsId: "ws_peco" },
      ),
    );
  });

  it("暂停接到同一条会话上", async () => {
    const stream = inFlightStream();
    await startTurn(stream);

    fireEvent.click(await screen.findByRole("button", { name: "暂停" }));

    // 空间显式带上：dock 恒落在 Peco 空间，不传就会落到正在浏览的那个空间
    expect(mocks.pause).toHaveBeenCalledWith("agent_1", "conv_1", {
      wsId: "ws_peco",
    });
  });
});
