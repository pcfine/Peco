import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { TooltipProvider } from "@/components/ui/tooltip";
import { ChatView } from "@/components/chat/ChatView";
import { toast } from "sonner";
import type { TurnSnapshotData } from "@/types/chat";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

// 发送失败的两种时机各测一次：帧未开始（回滚）与帧已开始（不回声，只提示）。
// fetch 与正文流都握在测试手里 —— 一次性 mock 分不清「没插过气泡」与「回滚掉了」。

const CONV = "conv_1";

function frame(event: string, id: number, data: unknown): string {
  return `event: ${event}\nid: ${id}\ndata: ${JSON.stringify(data)}\n\n`;
}

function snapshotFrame(seq: number, text: string): string {
  const snapshot: TurnSnapshotData = {
    conversation_id: CONV,
    seq,
    phase: "running",
    text,
    reasoning: "",
    tool_calls: [],
    agent_calls: [],
    usage: { input_tokens: 1, output_tokens: 2 },
  };
  return frame("turn_snapshot", seq, snapshot);
}

/** 交给测试推进的响应流：`push` 继续发帧，`fail` 让 `read()` 抛错（连接被掐）。 */
function controlledStream() {
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const body = new ReadableStream<Uint8Array>({
    start(next) {
      controller = next;
    },
  });
  const encoder = new TextEncoder();
  return {
    response: new Response(body, { status: 200 }),
    push: async (text: string) => {
      await act(async () => {
        controller.enqueue(encoder.encode(text));
      });
    },
    fail: async (err: Error) => {
      await act(async () => {
        controller.error(err);
      });
    },
  };
}

const fetchMock = vi.fn<typeof fetch>();
const uploadFile = vi.fn(async (file: File) => ({
  id: "f1",
  kind: "document" as const,
  name: file.name,
}));

function renderView() {
  return render(
    <TooltipProvider>
      <ChatView
        mode="internal"
        streamUrl={() => "/api/chat/%40master/conversations/conv_1/stream"}
        initialMessages={[]}
        uploadFile={uploadFile}
      />
    </TooltipProvider>,
  );
}

/** 输入一句话 + 挂一个待发送附件，然后回车发出。 */
async function compose(text: string) {
  const { container } = renderView();
  const fileInput =
    container.querySelector<HTMLInputElement>('input[type="file"]');
  if (!fileInput) throw new Error("没有附件入口");
  fireEvent.change(fileInput, {
    target: { files: [new File(["#"], "notes.md", { type: "text/markdown" })] },
  });
  await screen.findByText("notes.md");

  const textbox = screen.getByRole("textbox");
  fireEvent.change(textbox, { target: { value: text } });
  fireEvent.keyDown(textbox, { key: "Enter" });
  return { container };
}

beforeEach(() => {
  vi.stubGlobal("fetch", fetchMock);
  fetchMock.mockReset();
  uploadFile.mockClear();
  vi.mocked(toast.error).mockClear();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("发送失败的分类与回滚", () => {
  it("帧未开始就失败：气泡、输入、附件一起复原，toast 说的是服务端的拒绝", async () => {
    let resolveFetch!: (response: Response) => void;
    fetchMock.mockImplementation(
      () =>
        new Promise<Response>((resolve) => {
          resolveFetch = resolve;
        }),
    );
    const { container } = await compose("再跑一次");

    // resolve 之前：乐观气泡确实在 DOM 里（否则「回滚掉了」与「没插过」分不清）
    await waitFor(() =>
      expect(
        container.querySelector("[data-slot='user-bubble']"),
      ).not.toBeNull(),
    );
    // 待发送区已清空（消息里的附件名由用户气泡自己展示，不在此列）
    expect(screen.queryByRole("button", { name: "移除附件" })).toBeNull();

    await act(async () => {
      resolveFetch(
        new Response(
          JSON.stringify({
            code: "provider_unconfigured",
            details: "去设置 → 系统（本机）配一条 provider",
          }),
          { status: 500 },
        ),
      );
    });

    await waitFor(() =>
      expect(container.querySelector("[data-slot='user-bubble']")).toBeNull(),
    );
    expect(screen.getByRole("textbox")).toHaveValue("再跑一次");
    // 附件随消息一起还回来，用户不必重新上传
    expect(
      await screen.findByRole("button", { name: "移除附件" }),
    ).toBeInTheDocument();
    expect(toast.error).toHaveBeenCalledWith("没有可用的模型 provider", {
      description: "去设置 → 系统（本机）配一条 provider",
    });
  });

  it("帧已开始后断流：气泡保留、助手内容来自已应用的帧，只提示连接中断", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const { container } = await compose("跑起来");

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(snapshotFrame(5, "助手已经开跑"));
    await waitFor(() =>
      expect(container.textContent).toContain("助手已经开跑"),
    );

    await stream.fail(new Error("terminated"));

    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith("连接中断", {
        description: "这一轮的结果以下次打开会话为准",
      }),
    );
    // 服务端那一轮可能已经跑了：撤气泡会让人以为消息没发出去
    expect(container.querySelector("[data-slot='user-bubble']")).not.toBeNull();
    expect(container.textContent).toContain("助手已经开跑");
    // 输入不会被还回来：消息已经发出去了
    expect(screen.getByRole("textbox")).toHaveValue("");
  });
});
