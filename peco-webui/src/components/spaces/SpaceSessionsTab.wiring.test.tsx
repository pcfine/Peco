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
import { SpaceSessionsTab } from "@/components/spaces/SpaceSessionsTab";
import type { Conversation } from "@/types/chat";

// 会话 tab 是同一个 ChatView 的第三个入口：少接一条命令，这一边就既不能
// 在途说话也没有暂停按钮。断言落在真正的调用参数上。

const mocks = vi.hoisted(() => ({
  steer: vi.fn(async (..._args: unknown[]) => "accepted" as const),
  pause: vi.fn(async (..._args: unknown[]) => ({ success: true })),
  resume: vi.fn(async (..._args: unknown[]) => ({ success: true })),
}));

const CONVERSATIONS: Conversation[] = [
  {
    id: "conv_1",
    title: "会话",
    agent_name: "@master",
    created_at: "2024-01-01T00:00:00Z",
    updated_at: "2024-01-01T00:00:00Z",
  },
];

vi.mock("@/api/conversations", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/api/conversations")>();
  return {
    ...actual,
    listConversations: async () => CONVERSATIONS,
    getMessages: async () => [],
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

const frame = (event: string, id: number, data: unknown) =>
  `event: ${event}\nid: ${id}\ndata: ${JSON.stringify(data)}\n\n`;

const snapshotFrame = (seq: number) =>
  frame("turn_snapshot", seq, {
    conversation_id: "conv_1",
    seq,
    phase: "running",
    text: "",
    reasoning: "",
    tool_calls: [],
    agent_calls: [],
    usage: { input_tokens: 1, output_tokens: 2 },
  });

function tab() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <TooltipProvider>
        <SpaceSessionsTab
          wsId="ws_a"
          session="conv_1"
          onSelectSession={() => {}}
        />
      </TooltipProvider>
    </QueryClientProvider>,
  );
}

function send(text: string) {
  const textbox = screen.getByRole("textbox");
  fireEvent.change(textbox, { target: { value: text } });
  fireEvent.keyDown(textbox, { key: "Enter" });
}

/** 起一轮在途：发送流由测试持有，`/stream/live` 一律回 204（没有在途轮）。 */
async function startTurn() {
  const stream = inFlightStream();
  fetchMock.mockImplementation(async (input) =>
    String(input).includes("/stream/live")
      ? new Response(null, { status: 204 })
      : stream.response,
  );
  tab();
  await waitFor(() => expect(screen.getByRole("textbox")).toBeInTheDocument());
  send("开始跑");
  await waitFor(() =>
    expect(fetchMock).toHaveBeenCalledWith(
      expect.stringContaining("/conversations/conv_1/stream?"),
      expect.anything(),
    ),
  );
  await stream.push(snapshotFrame(3));
  return stream;
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

describe("空间会话 tab 的在途轮命令接线", () => {
  it("在途发送进当前会话的 pending 队列，带上空间作用域", async () => {
    await startTurn();

    send("顺便看下 lint");

    await waitFor(() =>
      expect(mocks.steer).toHaveBeenCalledWith(
        "@master",
        "conv_1",
        "顺便看下 lint",
        { imageIds: [], wsId: "ws_a" },
      ),
    );
  });

  it("暂停 / 恢复接到同一条会话与空间上", async () => {
    const stream = await startTurn();

    fireEvent.click(await screen.findByRole("button", { name: "暂停" }));
    expect(mocks.pause).toHaveBeenCalledWith("@master", "conv_1", {
      wsId: "ws_a",
    });

    // 暂停态只认服务端事件：按钮翻成「恢复」才有得点
    await stream.push(
      frame("looper_state", 4, { paused: true, conversation_id: "conv_1" }),
    );
    fireEvent.click(await screen.findByRole("button", { name: "恢复" }));
    expect(mocks.resume).toHaveBeenCalledWith("@master", "conv_1", {
      wsId: "ws_a",
    });
  });
});
