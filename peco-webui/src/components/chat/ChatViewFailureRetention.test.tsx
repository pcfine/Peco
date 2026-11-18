import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, waitFor } from "@testing-library/react";
import { TooltipProvider } from "@/components/ui/tooltip";
import {
  ChatView,
  MAX_RETAINED_FAILURES,
  mergeRetainedFailures,
  retainFailureIn,
  type ChatMessage,
  type ChatViewProps,
  type RetainedFailure,
} from "@/components/chat/ChatView";
import type { ErrorData, TurnSnapshotData } from "@/types/chat";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

// 失败轮在库里只有 user 行：`done` 触发的 refetch 拿历史整表覆盖本地列表时，红色
// 错误气泡必须留下并待在本轮 user 之后。这里把订阅流握在测试手里，逐帧喂给
// ChatView，再用 rerender 把 `initialMessages` 换成「库里回来的行」。

const LIVE_URL = "/api/chat/%40master/conversations/conv_1/stream/live?ws=ws_a";
const CONV = "conv_1";
const ERROR_TEXT = "模型调用失败：连接中断";
const CANCELLED_TEXT = "对话已被取消";

function frame(event: string, id: number, data: unknown): string {
  return `event: ${event}\nid: ${id}\ndata: ${JSON.stringify(data)}\n\n`;
}

function snapshotFrame(
  seq: number,
  text: string,
  extra: Partial<TurnSnapshotData> = {},
): string {
  return frame("turn_snapshot", seq, {
    conversation_id: CONV,
    seq,
    phase: "running",
    text,
    reasoning: "",
    tool_calls: [],
    agent_calls: [],
    usage: { input_tokens: 3, output_tokens: 4 },
    ...extra,
  });
}

const doneFrame = (seq: number) =>
  frame("done", seq, {
    usage: { input_tokens: 3, output_tokens: 9 },
    conversation_id: CONV,
  });

const errorFrame = (seq: number, data: Partial<ErrorData> = {}) =>
  frame("error", seq, {
    message: ERROR_TEXT,
    conversation_id: CONV,
    ...data,
  });

/** 订阅流交给测试：`push` 继续发帧。 */
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
  };
}

const fetchMock = vi.fn<typeof fetch>();

const userRow = (content: string): ChatMessage => ({
  id: `turn-${content}-0`,
  role: "user",
  content,
  turnIndex: 0,
});

const assistantRow = (content: string): ChatMessage => ({
  id: `turn-${content}-1`,
  role: "assistant",
  content,
  turnIndex: 0,
});

function renderView(overrides: Partial<ChatViewProps> = {}) {
  const base: ChatViewProps = {
    mode: "internal",
    initialMessages: [],
    streamUrl: () => "/api/chat/%40master/conversations/conv_1/stream",
    liveStreamUrl: () => LIVE_URL,
    historySettled: true,
    ...overrides,
  };
  const view = render(
    <TooltipProvider>
      <ChatView {...base} />
    </TooltipProvider>,
  );
  /** 同一次挂载内换 props（模拟壳里 refetch 回来的 `initialMessages`） */
  const rerenderWith = (next: Partial<ChatViewProps>) =>
    view.rerender(
      <TooltipProvider>
        <ChatView {...base} {...next} />
      </TooltipProvider>,
    );
  return { ...view, rerenderWith };
}

/** 消息列表的直接子节点：一条气泡一个节点，顺序即渲染顺序。 */
function listItems(container: HTMLElement): HTMLElement[] {
  const list = container.querySelector<HTMLElement>(
    "[data-slot='message-list']",
  );
  if (!list) throw new Error("没有消息列表");
  return Array.from(list.children) as HTMLElement[];
}

/** 文本在消息列表里的位置；没有则 -1。 */
function indexOfText(items: HTMLElement[], text: string): number {
  return items.findIndex((el) => el.textContent?.includes(text) ?? false);
}

beforeEach(() => {
  vi.stubGlobal("fetch", fetchMock);
  fetchMock.mockReset();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("失败轮的错误气泡跨 refetch 保留", () => {
  it("库里只有 user 行：错误气泡仍在，且跟在本轮 user 之后、列表末尾", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const onTurnSettled = vi.fn();
    const { container, rerenderWith } = renderView({
      initialMessages: [
        userRow("第一问"),
        assistantRow("第一答"),
        userRow("第二问"),
      ],
      onTurnSettled,
    });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(errorFrame(6));
    await stream.push(doneFrame(7));
    await waitFor(() => expect(onTurnSettled).toHaveBeenCalledTimes(1));

    // 壳里 refetch 回来：第二轮没落库，只有 user 行
    rerenderWith({
      initialMessages: [
        userRow("第一问"),
        assistantRow("第一答"),
        userRow("第二问"),
      ],
    });

    await waitFor(() => expect(container.textContent).toContain(ERROR_TEXT));
    // 历史已整体换过：正文必须以 DB 为准（第一答还在）
    expect(container.textContent).toContain("第一答");
    const items = listItems(container);
    const userAt = indexOfText(items, "第二问");
    const errorAt = indexOfText(items, ERROR_TEXT);
    expect(userAt).toBeGreaterThanOrEqual(0);
    expect(errorAt).toBeGreaterThan(userAt);
    expect(errorAt).toBe(items.length - 1);
  });

  it("这一轮已有真实回复落库：错误气泡不再渲染", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const onTurnSettled = vi.fn();
    const { container, rerenderWith } = renderView({
      initialMessages: [userRow("第一问"), userRow("第二问")],
      onTurnSettled,
    });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(errorFrame(6));
    await stream.push(doneFrame(7));
    await waitFor(() => expect(onTurnSettled).toHaveBeenCalledTimes(1));
    // 登记之前气泡就在 DOM 里：下面「不在」的断言才有区分力
    expect(container.textContent).toContain(ERROR_TEXT);

    rerenderWith({
      initialMessages: [
        userRow("第一问"),
        userRow("第二问"),
        assistantRow("落库的回答"),
      ],
    });

    await waitFor(() => expect(container.textContent).toContain("落库的回答"));
    expect(container.textContent).not.toContain(ERROR_TEXT);
  });

  it("取消不保留：注册表里没有它，refetch 后气泡消失", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const onTurnSettled = vi.fn();
    const { container, rerenderWith } = renderView({
      initialMessages: [userRow("第一问")],
      onTurnSettled,
    });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(
      errorFrame(6, { message: CANCELLED_TEXT, cancelled: true }),
    );
    await stream.push(doneFrame(7));
    await waitFor(() => expect(onTurnSettled).toHaveBeenCalledTimes(1));
    expect(container.textContent).toContain(CANCELLED_TEXT);

    rerenderWith({ initialMessages: [userRow("第一问")] });

    await waitFor(() => expect(container.textContent).toContain("第一问"));
    expect(container.textContent).not.toContain(CANCELLED_TEXT);
  });

  it("多轮定位：错误气泡落在失败的那一轮，不落到更早的轮", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const onTurnSettled = vi.fn();
    const { container, rerenderWith } = renderView({
      initialMessages: [userRow("第一问"), userRow("第二问")],
      onTurnSettled,
    });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(errorFrame(6));
    await stream.push(doneFrame(7));
    await waitFor(() => expect(onTurnSettled).toHaveBeenCalledTimes(1));

    rerenderWith({ initialMessages: [userRow("第一问"), userRow("第二问")] });

    await waitFor(() => expect(container.textContent).toContain(ERROR_TEXT));
    const items = listItems(container);
    // 落到第一轮就是 [第一问, 错误, 第二问]：位置 1 而非 2
    expect(indexOfText(items, ERROR_TEXT)).toBe(2);
    expect(indexOfText(items, "第二问")).toBe(1);
  });

  it("多次 refetch 不会叠出第二条同样的气泡", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const onTurnSettled = vi.fn();
    const { container, rerenderWith } = renderView({
      initialMessages: [userRow("第一问")],
      onTurnSettled,
    });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(errorFrame(6));
    await stream.push(doneFrame(7));
    await waitFor(() => expect(onTurnSettled).toHaveBeenCalledTimes(1));

    rerenderWith({ initialMessages: [userRow("第一问")] });
    await waitFor(() =>
      expect(
        container.querySelectorAll("[data-slot='assistant-bubble']"),
      ).toHaveLength(1),
    );

    rerenderWith({ initialMessages: [userRow("第一问")] });

    await waitFor(() =>
      expect(
        container.querySelectorAll("[data-slot='assistant-bubble']"),
      ).toHaveLength(1),
    );
    expect(container.textContent?.split(ERROR_TEXT)).toHaveLength(2);
  });

  it("回归：DB 里有更新回复时，refetch 仍以 DB 为准替换本地在途气泡", async () => {
    const stream = controlledStream();
    fetchMock.mockResolvedValue(stream.response);
    const onTurnSettled = vi.fn();
    const { container, rerenderWith } = renderView({
      initialMessages: [userRow("第一问")],
      onTurnSettled,
    });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await stream.push(snapshotFrame(5, "在途内容"));
    await stream.push(doneFrame(6));
    await waitFor(() => expect(onTurnSettled).toHaveBeenCalledTimes(1));
    expect(container.textContent).toContain("在途内容");

    rerenderWith({
      initialMessages: [userRow("第一问"), assistantRow("落库的回答")],
    });

    await waitFor(() => expect(container.textContent).toContain("落库的回答"));
    expect(container.textContent).not.toContain("在途内容");
  });
});

describe("mergeRetainedFailures", () => {
  const user = (content: string): ChatMessage => ({
    role: "user",
    content,
    turnIndex: 0,
  });
  const assistant = (content: string): ChatMessage => ({
    role: "assistant",
    content,
    turnIndex: 0,
  });
  const failure = (content: string): ChatMessage => ({
    role: "assistant",
    content,
    turnIndex: 0,
    isError: true,
  });
  const retainedAt = (
    anchorUserIndex: number,
    content: string,
  ): RetainedFailure => ({ anchorUserIndex, message: failure(content) });

  it("没有登记项时原样返回基准（引用不变，不触发多余渲染）", () => {
    const base = [user("问"), assistant("答")];
    const merged = mergeRetainedFailures(base, []);
    expect(merged.messages).toBe(base);
    expect(merged.retained).toEqual([]);
  });

  it("插到本轮块末尾，并保留登记供下一次 refetch 复用", () => {
    const base = [user("问一"), assistant("答一"), user("问二")];
    const merged = mergeRetainedFailures(base, [retainedAt(1, "失败")]);
    expect(merged.messages.map((m) => m.content)).toEqual([
      "问一",
      "答一",
      "问二",
      "失败",
    ]);
    expect(merged.retained).toHaveLength(1);
  });

  it("本轮已有 assistant：丢弃登记（不再回写）", () => {
    const base = [user("问一"), assistant("答一")];
    const merged = mergeRetainedFailures(base, [retainedAt(0, "失败")]);
    expect(merged.messages.map((m) => m.content)).toEqual(["问一", "答一"]);
    expect(merged.retained).toEqual([]);
  });

  it("锚点越界：追加到末尾，登记仍然保留", () => {
    const base = [user("问一")];
    const merged = mergeRetainedFailures(base, [retainedAt(3, "失败")]);
    expect(merged.messages.map((m) => m.content)).toEqual(["问一", "失败"]);
    expect(merged.retained).toHaveLength(1);
  });

  it("同一锚点只有一条登记时不会插出两条", () => {
    const base = [user("问一")];
    const merged = mergeRetainedFailures(base, [retainedAt(0, "失败")]);
    expect(merged.messages.map((m) => m.content)).toEqual(["问一", "失败"]);
    const again = mergeRetainedFailures(base, merged.retained);
    expect(again.messages.map((m) => m.content)).toEqual(["问一", "失败"]);
  });
});

describe("retainFailureIn", () => {
  const item = (n: number): RetainedFailure => ({
    anchorUserIndex: n,
    message: {
      role: "assistant",
      content: `失败 ${n}`,
      turnIndex: 0,
      isError: true,
    },
  });

  it("超出上限丢最旧的", () => {
    let retained: RetainedFailure[] = [];
    for (let i = 0; i < MAX_RETAINED_FAILURES + 3; i += 1) {
      retained = retainFailureIn(retained, item(i));
    }
    expect(retained).toHaveLength(MAX_RETAINED_FAILURES);
    expect(retained[0].anchorUserIndex).toBe(3);
    expect(retained[MAX_RETAINED_FAILURES - 1].anchorUserIndex).toBe(
      MAX_RETAINED_FAILURES + 2,
    );
  });
});
