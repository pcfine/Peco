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
import { SpaceChatShell } from "@/components/spaces/SpaceChatShell";

// 壳只负责把在途轮的三条命令接到当前会话与空间上：注入、暂停、恢复。
// 接漏的症状是「这一边在途不能说话 / 没有暂停按钮」，所以断言落在真正的调用参数上。

const mocks = vi.hoisted(() => ({
  getMessages: vi.fn(),
  steer: vi.fn(async (..._args: unknown[]) => "accepted" as const),
  pause: vi.fn(async (..._args: unknown[]) => ({ success: true })),
  resume: vi.fn(async (..._args: unknown[]) => ({ success: true })),
}));

vi.mock("@/api/conversations", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/api/conversations")>();
  return {
    ...actual,
    getMessages: (...args: unknown[]) => mocks.getMessages(...args),
    uploadConversationImage: async () => ({ id: "img_1", url: "/x.png" }),
    steerConversationTurn: (...args: unknown[]) => mocks.steer(...args),
    pauseConversation: (...args: unknown[]) => mocks.pause(...args),
    resumeConversation: (...args: unknown[]) => mocks.resume(...args),
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

/** 第一条流留给测试推帧：轮一直在跑，才测得了「在途」。 */
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

function shell() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <TooltipProvider>
        <SpaceChatShell wsId="ws_a" visible teamId={null} />
      </TooltipProvider>
    </QueryClientProvider>,
  );
}

function send(text: string) {
  const textbox = screen.getByRole("textbox");
  fireEvent.change(textbox, { target: { value: text } });
  fireEvent.keyDown(textbox, { key: "Enter" });
}

const snapshotFrame = (seq: number) =>
  `event: turn_snapshot\nid: ${seq}\ndata: ${JSON.stringify({
    conversation_id: "conv_1",
    seq,
    phase: "running",
    text: "",
    reasoning: "",
    tool_calls: [],
    agent_calls: [],
    usage: { input_tokens: 1, output_tokens: 2 },
  })}\n\n`;

beforeEach(() => {
  mocks.getMessages.mockReset();
  mocks.getMessages.mockResolvedValue([]);
  mocks.steer.mockClear();
  mocks.pause.mockClear();
  mocks.resume.mockClear();
  fetchMock.mockReset();
  // 在途轮的订阅：统一回 204（没有在途轮），本文件自己造一条发送流
  fetchMock.mockImplementation(async () => new Response(null, { status: 204 }));
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("空间壳的在途轮命令接线", () => {
  it("在途发送进当前会话的 pending 队列，带上空间作用域", async () => {
    const stream = inFlightStream();
    fetchMock.mockImplementation(async (input) =>
      String(input).includes("/stream/live")
        ? new Response(null, { status: 204 })
        : stream.response,
    );
    shell();

    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    send("开始跑");
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        expect.stringContaining("/conversations/conv_1/stream?"),
        expect.anything(),
      ),
    );

    send("顺便看下 lint");

    await waitFor(() =>
      expect(mocks.steer).toHaveBeenCalledWith(
        "@master",
        "conv_1",
        "顺便看下 lint",
        {
          imageIds: [],
          wsId: "ws_a",
        },
      ),
    );
  });

  it("暂停 / 恢复接到同一条会话上", async () => {
    const stream = inFlightStream();
    fetchMock.mockImplementation(async (input) =>
      String(input).includes("/stream/live")
        ? new Response(null, { status: 204 })
        : stream.response,
    );
    shell();

    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    send("开始跑");
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        expect.stringContaining("/conversations/conv_1/stream?"),
        expect.anything(),
      ),
    );
    await stream.push(snapshotFrame(3));

    const pause = await screen.findByRole("button", { name: "暂停" });
    fireEvent.click(pause);

    expect(mocks.pause).toHaveBeenCalledWith("@master", "conv_1", {
      wsId: "ws_a",
    });
  });
});
