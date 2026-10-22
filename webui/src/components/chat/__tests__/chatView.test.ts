// ChatView 折叠与流式状态机单测 — 工具调用配对、孤儿合成、三态迁移、快照恢复

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  ChatView,
  messagesFromTurn,
  reduceStreamEvent,
  snapshotToMessages,
  type ChatMessage,
} from "../ChatView";
import { TooltipProvider } from "@/components/ui/tooltip";
import { useAuthStore } from "@/stores/authStore";
import {
  __resetPecoStreamForTests,
  usePecoChatStore,
} from "@/stores/pecoChatStore";
import { formatMessageTime } from "@/lib/time";
import type { ChatSseEvent, MessageData, TurnData } from "../../../types/chat";

(
  globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  useAuthStore.setState({ token: "test-token" });
  usePecoChatStore.setState({ messages: [], isStreaming: false });
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  __resetPecoStreamForTests();
  vi.restoreAllMocks();
});

/** external 模式渲染：消息由 initialMessages 直接给出，不经流式。 */
function renderMessages(messages: ChatMessage[]) {
  return act(async () => {
    root.render(
      createElement(
        TooltipProvider,
        null,
        createElement(ChatView, {
          mode: "external",
          streamUrl: (m: string) => `/api/peco/stream?message=${m}`,
          initialMessages: messages,
          onExternalSend: () => {},
          onExternalStop: () => {},
          externalIsStreaming: false,
        }),
      ),
    );
  });
}

function frame(event: string, data: unknown): string {
  return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}

/** 可手动推送事件、保持打开的 SSE 流。 */
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

const USER_TEXT = "看看这张图";

function assistantWithTool(id = "call_1"): ChatMessage[] {
  return [
    { role: "user", content: USER_TEXT, turnIndex: 0 },
    {
      role: "assistant",
      content: "",
      turnIndex: 0,
      // status 必填：这里模拟 tool_call_start 之后的在途调用
      toolCalls: [{ id, name: "shell", arguments: "{}", status: "pending" }],
    },
  ];
}

describe("reduceStreamEvent tool_result", () => {
  it("attaches images from the tool_result event", () => {
    const messages = assistantWithTool();
    const updated = reduceStreamEvent(
      {
        event: "tool_result",
        data: {
          id: "call_1",
          name: "shell",
          result: "截图完成",
          images: ["data:image/png;base64,AAAA"],
          conversation_id: "c1",
        },
      },
      messages,
    );
    const assistant = updated[1];
    expect(assistant.toolCalls?.[0]?.result).toBe("截图完成");
    expect(assistant.toolCalls?.[0]?.images).toEqual([
      "data:image/png;base64,AAAA",
    ]);
  });

  it("keeps images undefined when the event carries none", () => {
    const messages = assistantWithTool();
    const updated = reduceStreamEvent(
      {
        event: "tool_result",
        data: {
          id: "call_1",
          name: "shell",
          result: "纯文本结果",
          conversation_id: "c1",
        },
      },
      messages,
    ) as ChatMessage[];
    expect(updated[1].toolCalls?.[0]?.images).toBeUndefined();
  });
});

describe("reduceStreamEvent truncation_retry", () => {
  const RETRY = {
    event: "truncation_retry" as const,
    data: {
      attempt: 1,
      limit: 1,
      output_tokens: 4096,
      retry_budget: 32768,
      conversation_id: "c1",
    },
  };

  /** 残句已在画面上的那一轮：user + 正在生长的 assistant 气泡。 */
  function midTurn(): ChatMessage[] {
    return [
      { role: "user", content: "写一篇长文", turnIndex: 0 },
      { role: "assistant", content: "part", turnIndex: 0 },
    ];
  }

  it("inserts a notice before the trailing assistant bubble, keeping the residue", () => {
    const updated = reduceStreamEvent(RETRY, midTurn());

    expect(updated).toHaveLength(3);
    // 横幅在气泡之前 —— 残句在横幅之上，重试的正文在横幅之下继续生长
    expect(updated[1].isNotice).toBe(true);
    expect(updated[1].noticeIcon).toBe("🔁");
    expect(updated[1].content).toContain("1/1");
    // 已收到的增量不得删除
    expect(updated[2].role).toBe("assistant");
    expect(updated[2].content).toBe("part");
    expect(updated[2].isNotice).toBeUndefined();
  });

  it("keeps appending deltas to the answer bubble, not the notice", () => {
    // 本设计最关键的回归判据：横幅成为末条就会吞掉重试的所有增量。
    const afterNotice = reduceStreamEvent(RETRY, midTurn());
    const afterDelta = reduceStreamEvent(
      { event: "text_delta", data: { content: "done", conversation_id: "c1" } },
      afterNotice,
    );

    expect(afterDelta).toHaveLength(3);
    expect(afterDelta[1].isNotice).toBe(true);
    expect(afterDelta[1].content).not.toContain("done");
    expect(afterDelta[2].content).toBe("partdone");
  });

  it("leaves reasoning deltas on the answer bubble too", () => {
    const afterNotice = reduceStreamEvent(RETRY, midTurn());
    const afterDelta = reduceStreamEvent(
      {
        event: "reasoning_delta",
        data: { content: "想想", conversation_id: "c1" },
      },
      afterNotice,
    );

    expect(afterDelta[1].isNotice).toBe(true);
    expect(afterDelta[2].reasoning).toBe("想想");
  });

  it("truncation_retry 横幅之后的 reasoning_delta 不写进重试横幅", () => {
    let live = reduceStreamEvent(textDelta("残句"), midTurn());
    live = reduceStreamEvent(RETRY, live);
    const banner = live[1];
    live = reduceStreamEvent(reasoningDelta("想"), live);

    expect(live[1].isNotice).toBe(true);
    expect(live[1].content).toBe(banner.content);
    expect(live[1].reasoning).toBeUndefined();
    // 残句保留在原宿主上（"part" 是 midTurn 的残句，delta 追加其后）
    expect(live[2].content).toBe("part残句");
    expect(live[2].reasoning).toBe("想");
  });

  it("末条是 agent-call 时照旧插横幅，不整条丢弃", () => {
    const withAgent = reduceStreamEvent(agentCallEvent, midTurn());
    const updated = reduceStreamEvent(RETRY, withAgent);

    expect(updated).toHaveLength(4);
    // 横幅落在 assistant 宿主之前，agent-call 仍在末尾
    expect(updated[1].isNotice).toBe(true);
    expect(updated[1].noticeIcon).toBe("🔁");
    expect(updated[2].content).toBe("part");
    expect(updated[3].role).toBe("agent-call");
  });

  it("stacks one notice per retry, in order", () => {
    const once = reduceStreamEvent(RETRY, midTurn());
    const twice = reduceStreamEvent(
      { ...RETRY, data: { ...RETRY.data, attempt: 2, limit: 2 } },
      once,
    );

    expect(twice.map((m) => m.isNotice === true)).toEqual([
      false,
      true,
      true,
      false,
    ]);
    expect(twice[3].content).toBe("part");
  });

  it("renders transient reason with connection wording, keeping residue", () => {
    const transient = {
      ...RETRY,
      data: { ...RETRY.data, reason: "transient" },
    };
    const updated = reduceStreamEvent(transient, midTurn());

    expect(updated[1].isNotice).toBe(true);
    expect(updated[1].content).toContain("连接中断");
    expect(updated[1].content).toContain("1/1");
    expect(updated[1].content).not.toContain("截断");
    // 残句照旧保留
    expect(updated[2].content).toBe("part");
  });

  it("defaults to truncation wording when reason is absent (old backend)", () => {
    const updated = reduceStreamEvent(RETRY, midTurn());
    expect(updated[1].content).toContain("输出被截断");
  });

  it("returns the same array when there is no trailing assistant bubble", () => {
    const messages: ChatMessage[] = [
      { role: "user", content: "你好", turnIndex: 0 },
    ];
    expect(reduceStreamEvent(RETRY, messages)).toBe(messages);
  });
});

describe("snapshotToMessages", () => {
  const DATA_URI = "data:image/png;base64,AAAA";

  it("restores user message images from the snapshot DTO", () => {
    const turns: TurnData[] = [
      {
        turn_index: 0,
        messages: [
          {
            role: "user",
            content: "看看这张图",
            images: [DATA_URI],
            timestamp_ms: 1,
          },
          { role: "assistant", content: "图中是一只猫", timestamp_ms: 2 },
        ],
      },
    ];
    const messages = snapshotToMessages(turns);
    expect(messages[0].role).toBe("user");
    expect(messages[0].images).toEqual([DATA_URI]);
    expect(messages[0].content).toBe("看看这张图");
  });

  it("keeps images undefined for user messages without images", () => {
    // 无图消息 DTO 不含 images 字段，恢复后同样保持缺省。
    const turns: TurnData[] = [
      {
        turn_index: 0,
        messages: [
          { role: "user", content: "纯文本", timestamp_ms: 1 },
          { role: "assistant", content: "好的", timestamp_ms: 2 },
        ],
      },
    ];
    const messages = snapshotToMessages(turns);
    expect(messages[0].images).toBeUndefined();
  });

  it("prepends an interrupted banner for frozen turns", () => {
    const turns: TurnData[] = [
      {
        turn_index: 0,
        interrupted: true,
        interrupted_reason: "cancelled",
        messages: [
          { role: "user", content: "跑个长任务", timestamp_ms: 1 },
          { role: "assistant", content: "进行中…", timestamp_ms: 2 },
        ],
      },
    ];
    const messages = snapshotToMessages(turns);
    // 横幅在该轮消息之前，且复用 isNotice 居中分隔样式。
    expect(messages[0].isNotice).toBe(true);
    expect(messages[0].noticeIcon).toBe("⏹");
    expect(messages[0].content).toContain("cancelled");
    expect(messages[0].turnIndex).toBe(0);
    expect(messages.slice(1).map((m) => m.role)).toEqual(["user", "assistant"]);
  });

  it("adds no banner for normally completed turns", () => {
    const turns: TurnData[] = [
      {
        turn_index: 0,
        messages: [{ role: "user", content: "你好", timestamp_ms: 1 }],
      },
    ];
    expect(snapshotToMessages(turns).every((m) => !m.isNotice)).toBe(true);
  });
});

// ── 工具调用折叠（历史快照） ────────────────────────────────────────────────

const toolOut = (
  callId: string,
  content: string,
  timestamp_ms: number,
): MessageData => ({
  role: "tool",
  tool_call_id: callId,
  content,
  timestamp_ms,
});

const assistantCall = (
  calls: { id: string; name: string }[],
  timestamp_ms: number,
): MessageData => ({
  role: "assistant",
  content: "",
  tool_calls: calls.map((c) => ({ ...c, arguments: '{"command":"ls"}' })),
  timestamp_ms,
});

const userMsg = (content: string, timestamp_ms: number): MessageData => ({
  role: "user",
  content,
  timestamp_ms,
});

const cardsOf = (messages: ChatMessage[]) =>
  messages.flatMap((m) => m.toolCalls ?? []);

/**
 * 卡片的宿主维度：字段相等不足以说明卡片挂对了地方 —— 孤儿宿主同样是
 * assistant 且卡片下标为 0。
 */
const cardLayout = (messages: ChatMessage[]) =>
  messages.flatMap((m, mi) =>
    (m.toolCalls ?? []).map((tc, ci) => ({
      id: tc.id,
      hostRole: m.role,
      hostOrdinal:
        m.role === "assistant"
          ? messages.slice(0, mi).filter((x) => x.role === "assistant").length
          : -1,
      cardIndex: ci,
      orphan: tc.orphan === true,
    })),
  );

describe("messagesFromTurn 配对", () => {
  it('命中输出 → done + result，且不产出 role:"tool" 消息', () => {
    const messages = messagesFromTurn({
      turn_index: 4,
      messages: [
        userMsg("看看目录", 1),
        assistantCall([{ id: "a", name: "shell" }], 2),
        toolOut("a", "file.txt", 3),
        { role: "assistant", content: "只有一个文件", timestamp_ms: 4 },
      ],
    });

    expect(messages.map((m) => m.role)).toEqual([
      "user",
      "assistant",
      "assistant",
    ]);
    expect(messages[1].toolCalls?.[0]).toMatchObject({
      id: "a",
      name: "shell",
      status: "done",
      result: "file.txt",
    });
  });

  it("缺输出 → missing，且不带 result", () => {
    const messages = messagesFromTurn({
      turn_index: 0,
      messages: [
        userMsg("跑", 1),
        assistantCall([{ id: "a", name: "shell" }], 2),
      ],
    });

    const card = messages[1].toolCalls?.[0];
    expect(card?.status).toBe("missing");
    expect(card?.result).toBeUndefined();
  });

  it("空输出也算命中 → done + result 为空串", () => {
    const messages = messagesFromTurn({
      turn_index: 0,
      messages: [
        userMsg("跑", 1),
        assistantCall([{ id: "a", name: "shell" }], 2),
        toolOut("a", "", 3),
      ],
    });

    expect(messages[1].toolCalls?.[0]).toMatchObject({
      status: "done",
      result: "",
    });
  });

  it("乱序输出按 id 配对，与位置无关", () => {
    const messages = messagesFromTurn({
      turn_index: 0,
      messages: [
        userMsg("跑两个", 1),
        assistantCall(
          [
            { id: "a", name: "shell" },
            { id: "b", name: "read" },
          ],
          2,
        ),
        toolOut("b", "B", 3),
        toolOut("a", "A", 4),
      ],
    });

    const cards = cardsOf(messages);
    expect(cards.map((c) => [c.id, c.status, c.result])).toEqual([
      ["a", "done", "A"],
      ["b", "done", "B"],
    ]);
  });

  it("一条 assistant 的多个调用分散在不同 assistant 之间也能全部配对", () => {
    const messages = messagesFromTurn({
      turn_index: 0,
      messages: [
        userMsg("跑三个", 1),
        assistantCall(
          [
            { id: "a", name: "shell" },
            { id: "b", name: "shell" },
            { id: "c", name: "shell" },
          ],
          2,
        ),
        toolOut("c", "C", 3),
        toolOut("a", "A", 4),
        toolOut("b", "B", 5),
      ],
    });

    const cards = cardsOf(messages);
    expect(cards.map((c) => c.result)).toEqual(["A", "B", "C"]);
    expect(cards.every((c) => c.status === "done")).toBe(true);
  });

  it("tool_calls 为空数组或缺失时不产出 toolCalls", () => {
    const [empty, absent] = messagesFromTurn({
      turn_index: 0,
      messages: [
        { role: "assistant", content: "一", tool_calls: [], timestamp_ms: 1 },
        { role: "assistant", content: "二", timestamp_ms: 2 },
      ],
    }).map((m) => m.toolCalls);

    expect(empty).toBeUndefined();
    expect(absent).toBeUndefined();
  });

  it("快照消息 id 一律按原始下标生成（含 assistant）", () => {
    const messages = messagesFromTurn({
      turn_index: 7,
      messages: [
        userMsg("问", 1),
        assistantCall([{ id: "a", name: "shell" }], 2),
        toolOut("a", "out", 3),
      ],
    });

    // tool 消息不产出，下标仍按原始位置
    expect(messages.map((m) => m.id)).toEqual(["turn-7-0", "turn-7-1"]);
  });
});

describe("messagesFromTurn 孤儿输出", () => {
  it("孤儿输出合成一条宿主，落点在本轮末尾", () => {
    const messages = messagesFromTurn({
      turn_index: 3,
      messages: [
        userMsg("问", 1),
        assistantCall([{ id: "a", name: "shell" }], 2),
        toolOut("a", "out", 3),
        toolOut("ghost", "无主输出", 4),
        { role: "assistant", content: "结束", timestamp_ms: 5 },
      ],
    });

    const host = messages.at(-1);
    expect(host?.id).toBe("turn-3-tool-orphans");
    expect(host?.role).toBe("assistant");
    expect(host?.content).toBe("");
    expect(host?.toolCalls).toEqual([
      expect.objectContaining({
        id: "ghost",
        name: "工具输出",
        status: "done",
        result: "无主输出",
        orphan: true,
      }),
    ]);
    // 不追加进已有宿主
    expect(messages[1].toolCalls).toHaveLength(1);
  });

  it("无孤儿时不产合成宿主", () => {
    const messages = messagesFromTurn({
      turn_index: 0,
      messages: [
        userMsg("问", 1),
        assistantCall([{ id: "a", name: "shell" }], 2),
        toolOut("a", "out", 3),
      ],
    });

    expect(messages.some((m) => m.id?.endsWith("-tool-orphans"))).toBe(false);
  });

  it("跨轮隔离：上一轮缺输出不会被下一轮同 id 的输出消费", () => {
    const messages = snapshotToMessages([
      {
        turn_index: 0,
        messages: [
          userMsg("先跑", 1),
          assistantCall([{ id: "a", name: "shell" }], 2),
        ],
      },
      {
        turn_index: 1,
        messages: [userMsg("再跑", 3), toolOut("a", "迟到的输出", 4)],
      },
    ]);

    const firstTurnCard = messages.find((m) => m.id === "turn-0-1")
      ?.toolCalls?.[0];
    expect(firstTurnCard?.status).toBe("missing");
    expect(firstTurnCard?.result).toBeUndefined();

    const orphanHost = messages.at(-1);
    expect(orphanHost?.id).toBe("turn-1-tool-orphans");
    expect(orphanHost?.toolCalls?.[0]).toMatchObject({
      id: "a",
      orphan: true,
      result: "迟到的输出",
    });
  });
});

// ── 实时流三态 ────────────────────────────────────────────────────────────

const startEvent = (id: string, name = "shell"): ChatSseEvent => ({
  event: "tool_call_start",
  data: { id, name, arguments: '{"command":"ls"}', conversation_id: "c" },
});

const resultEvent = (id: string, result: string): ChatSseEvent => ({
  event: "tool_result",
  data: { id, name: "shell", result, conversation_id: "c" },
});

const agentCallEvent: ChatSseEvent = {
  event: "agent_call_start",
  data: {
    call_id: "ac1",
    agent_id: "a1",
    agent_name: "写手",
    task: "写一段",
    conversation_id: "c",
  },
};

const agentCallEndEvent: ChatSseEvent = {
  event: "agent_call_end",
  data: {
    call_id: "ac1",
    agent_id: "a1",
    agent_name: "写手",
    result: "子任务完成",
    conversation_id: "c",
  },
};

const textDelta = (content: string): ChatSseEvent => ({
  event: "text_delta",
  data: { content, conversation_id: "c" },
});

const reasoningDelta = (content: string): ChatSseEvent => ({
  event: "reasoning_delta",
  data: { content, conversation_id: "c" },
});

const compactedEvent: ChatSseEvent = {
  event: "context_compacted",
  data: { evicted_turns: 2, summary: "更早对话的摘要", conversation_id: "c" },
};

const liveTurn = (): ChatMessage[] => [
  { role: "user", content: "跑一下", turnIndex: 0 },
  { role: "assistant", content: "", turnIndex: 0 },
];

describe("reduceStreamEvent 通知横幅不作 delta 宿主", () => {
  it("context_compacted 之后到达的 text_delta 不写进归档横幅", () => {
    // 归档横幅的 role 同为 assistant：只按 role 找宿主就会把正文写进横幅。
    let live = reduceStreamEvent(textDelta("正文"), liveTurn());
    live = reduceStreamEvent(compactedEvent, live);
    const banner = live[2];
    live = reduceStreamEvent(textDelta("继续"), live);

    expect(live).toHaveLength(3);
    expect(live[2].isNotice).toBe(true);
    expect(live[2].content).toBe(banner.content);
    expect(live[2].content).not.toContain("继续");
    // 归档横幅之前那条 assistant 才是正文宿主
    expect(live[1].role).toBe("assistant");
    expect(live[1].content).toBe("正文继续");
  });

  it("归档横幅之后的 reasoning_delta 也不落进横幅", () => {
    let live = reduceStreamEvent(textDelta("正文"), liveTurn());
    live = reduceStreamEvent(compactedEvent, live);
    const banner = live[2];
    live = reduceStreamEvent(reasoningDelta("想"), live);

    expect(live[2].content).toBe(banner.content);
    expect(live[2].reasoning).toBeUndefined();
    expect(live[1].reasoning).toBe("想");
  });
});

describe("reduceStreamEvent 工具三态", () => {
  it("tool_call_start 建 pending 卡片", () => {
    const live = reduceStreamEvent(startEvent("a"), liveTurn());

    expect(live[1].toolCalls?.[0]).toMatchObject({
      id: "a",
      name: "shell",
      status: "pending",
    });
  });

  it("tool_result 命中 → done + result", () => {
    const started = reduceStreamEvent(startEvent("a"), liveTurn());
    const live = reduceStreamEvent(resultEvent("a", "out"), started);

    expect(live[1].toolCalls?.[0]).toMatchObject({
      status: "done",
      result: "out",
    });
  });

  it("tool_result 只扫最后一个 user 之后的 assistant，不改写历史轮", () => {
    const messages: ChatMessage[] = [
      { role: "user", content: "上一轮", turnIndex: 0 },
      {
        role: "assistant",
        content: "",
        turnIndex: 0,
        toolCalls: [
          {
            id: "a",
            name: "shell",
            arguments: "{}",
            status: "done",
            result: "旧",
          },
        ],
      },
      { role: "user", content: "这一轮", turnIndex: 0 },
      {
        role: "assistant",
        content: "",
        turnIndex: 0,
        toolCalls: [
          { id: "a", name: "shell", arguments: "{}", status: "pending" },
        ],
      },
    ];

    const live = reduceStreamEvent(resultEvent("a", "新"), messages);

    expect(live[1].toolCalls?.[0].result).toBe("旧");
    expect(live[3].toolCalls?.[0]).toMatchObject({
      status: "done",
      result: "新",
    });
  });

  it("tool_result 未命中 → 合成孤儿宿主，且不带 id", () => {
    const live = reduceStreamEvent(resultEvent("ghost", "无主输出"), [
      { role: "user", content: "问", turnIndex: 0 },
      { role: "assistant", content: "答", turnIndex: 0 },
    ]);

    const host = live.at(-1);
    expect(live).toHaveLength(3);
    expect(host?.role).toBe("assistant");
    expect(host?.id).toBeUndefined();
    expect(host?.toolCalls?.[0]).toMatchObject({
      id: "ghost",
      status: "done",
      result: "无主输出",
      orphan: true,
    });
  });

  it("turn_complete 把残留 pending 结算为 missing", () => {
    const started = reduceStreamEvent(startEvent("a"), liveTurn());
    const live = reduceStreamEvent(
      {
        event: "turn_complete",
        data: {
          text: "答",
          usage: { input_tokens: 1, output_tokens: 1 },
          conversation_id: "c",
        },
      },
      started,
    );

    expect(live[1].toolCalls?.[0].status).toBe("missing");
  });

  it("done 把残留 pending 结算为 missing", () => {
    const started = reduceStreamEvent(startEvent("a"), liveTurn());
    const live = reduceStreamEvent(
      {
        event: "done",
        data: {
          usage: { input_tokens: 1, output_tokens: 1 },
          conversation_id: "c",
        },
      },
      started,
    );

    expect(live[1].toolCalls?.[0].status).toBe("missing");
  });

  it("error 追加错误气泡后仍结算前面消息里的 pending", () => {
    const started = reduceStreamEvent(startEvent("a"), liveTurn());
    const live = reduceStreamEvent(
      { event: "error", data: { message: "模型报错", conversation_id: "c" } },
      started,
    );

    // 末条是错误气泡 —— 结算必须扫全部消息，不能只看末条
    expect(live.at(-1)?.isError).toBe(true);
    expect(live[1].toolCalls?.[0].status).toBe("missing");
  });

  it("agent-call 穿插后 done 仍结算 pending", () => {
    const started = reduceStreamEvent(startEvent("a"), liveTurn());
    const withAgent = reduceStreamEvent(agentCallEvent, started);
    const live = reduceStreamEvent(
      {
        event: "done",
        data: {
          usage: { input_tokens: 1, output_tokens: 1 },
          conversation_id: "c",
        },
      },
      withAgent,
    );

    expect(live.at(-1)?.role).toBe("agent-call");
    expect(live[1].toolCalls?.[0].status).toBe("missing");
  });
});

describe("reduceStreamEvent 委派顺序", () => {
  const DELEGATE = "delegate_sub_agent";

  /**
   * 后端的真实顺序：agent_call_start → tool_call_start →（子 agent 运行）→
   * agent_call_end → tool_result → text_delta。末条在 tool_call_start 时已是
   * agent-call，任何「只看末条」的实现都会丢卡片。
   */
  function runDelegation() {
    let live = reduceStreamEvent(agentCallEvent, liveTurn());
    live = reduceStreamEvent(startEvent("a", DELEGATE), live);
    const afterStart = live;
    live = reduceStreamEvent(agentCallEndEvent, live);
    live = reduceStreamEvent(resultEvent("a", "子任务完成"), live);
    const afterResult = live;
    live = reduceStreamEvent(textDelta("汇总完成"), live);
    return { afterStart, afterResult, live };
  }

  it("tool_call_start 建卡在 assistant 宿主上，参数保留", () => {
    const { afterStart } = runDelegation();

    expect(afterStart).toHaveLength(3);
    const host = afterStart[1];
    expect(host.role).toBe("assistant");
    expect(host.toolCalls).toHaveLength(1);
    expect(host.toolCalls?.[0]).toMatchObject({
      id: "a",
      name: DELEGATE,
      status: "pending",
      arguments: '{"command":"ls"}',
    });
    // 卡片不挂到 agent-call 上
    expect(afterStart.at(-1)?.role).toBe("agent-call");
    expect(afterStart.at(-1)?.toolCalls).toBeUndefined();
  });

  it("tool_result 回收同一张卡：done + result，不记 orphan", () => {
    const { afterResult } = runDelegation();

    // 没有合成孤儿宿主：user + assistant + agent-call
    expect(afterResult).toHaveLength(3);
    const hosts = afterResult.filter((m) => (m.toolCalls?.length ?? 0) > 0);
    expect(hosts).toHaveLength(1);
    expect(hosts[0].role).toBe("assistant");
    expect(hosts[0].toolCalls?.[0]).toMatchObject({
      status: "done",
      result: "子任务完成",
      arguments: '{"command":"ls"}',
    });
    expect(hosts[0].toolCalls?.[0].orphan).toBeUndefined();
  });

  it("text_delta 写回该 assistant，不落进 agent-call 或孤儿宿主", () => {
    const { live } = runDelegation();

    const assistants = live.filter((m) => m.role === "assistant");
    expect(assistants).toHaveLength(1);
    expect(assistants[0].content).toBe("汇总完成");
    expect(live.find((m) => m.role === "agent-call")?.content).toBe(
      "子任务完成",
    );
  });

  it("reasoning_delta 同样落回 assistant 宿主", () => {
    let live = reduceStreamEvent(agentCallEvent, liveTurn());
    live = reduceStreamEvent(startEvent("a", DELEGATE), live);
    live = reduceStreamEvent(
      {
        event: "reasoning_delta",
        data: { content: "先想", conversation_id: "c" },
      },
      live,
    );

    expect(live[1].reasoning).toBe("先想");
    expect(live.at(-1)?.reasoning).toBeUndefined();
  });
});

describe("实时与历史收敛同构", () => {
  it("同一轮数据的 id / name / status 相等，实时侧不带 id", () => {
    const turn: TurnData = {
      turn_index: 2,
      messages: [
        userMsg("看看目录", 1),
        assistantCall([{ id: "a", name: "shell" }], 2),
        toolOut("a", "file.txt", 3),
        { role: "assistant", content: "只有一个文件", timestamp_ms: 4 },
      ],
    };
    const historical = messagesFromTurn(turn);

    let live: ChatMessage[] = liveTurn();
    live = reduceStreamEvent(
      {
        event: "tool_call_start",
        data: {
          id: "a",
          name: "shell",
          arguments: '{"command":"ls"}',
          conversation_id: "c",
        },
      },
      live,
    );
    live = reduceStreamEvent(resultEvent("a", "file.txt"), live);
    live = reduceStreamEvent(
      {
        event: "text_delta",
        data: { content: "只有一个文件", conversation_id: "c" },
      },
      live,
    );

    const pick = (messages: ChatMessage[]) =>
      cardsOf(messages).map(({ id, name, status, result }) => ({
        id,
        name,
        status,
        result,
      }));

    // 夹具输出远小于服务端归档上限，故 result 也可比
    expect(pick(live)).toEqual(pick(historical));
    // 宿主维度：字段相等时卡片仍可能挂在孤儿宿主上
    expect(cardLayout(live)).toEqual(cardLayout(historical));

    // 不变量：实时消息一律不带 id（否则刷新后与快照重复渲染）
    expect(live.every((m) => m.id === undefined)).toBe(true);
    expect(historical.every((m) => m.id !== undefined)).toBe(true);
  });

  it("委派插曲后宿主维度仍收敛（agent-call 不吃卡片）", () => {
    const turn: TurnData = {
      turn_index: 2,
      messages: [
        userMsg("委派一下", 1),
        assistantCall([{ id: "a", name: "delegate_sub_agent" }], 2),
        toolOut("a", "子任务完成", 3),
        { role: "assistant", content: "汇总完成", timestamp_ms: 4 },
      ],
    };
    const historical = messagesFromTurn(turn);

    let live: ChatMessage[] = liveTurn();
    live = reduceStreamEvent(agentCallEvent, live);
    live = reduceStreamEvent(startEvent("a", "delegate_sub_agent"), live);
    live = reduceStreamEvent(agentCallEndEvent, live);
    live = reduceStreamEvent(resultEvent("a", "子任务完成"), live);
    live = reduceStreamEvent(textDelta("汇总完成"), live);

    // 实时侧多一条 agent-call，故绝对下标不可比；宿主序号与卡片下标可比
    expect(cardLayout(live)).toEqual(cardLayout(historical));
  });
});

describe("中断轮入史", () => {
  it("工具卡为 done 且展开体保留 [interrupted] 文案，中断横幅不被折叠吞掉", () => {
    const reason = "cancelled";
    const interruptedOutput =
      "[interrupted] tool execution was interrupted before completion";
    const messages = snapshotToMessages([
      {
        turn_index: 3,
        interrupted: true,
        interrupted_reason: reason,
        messages: [
          userMsg("跑个长任务", 1),
          assistantCall([{ id: "c1", name: "shell" }], 2),
          toolOut("c1", interruptedOutput, 3),
          {
            role: "assistant",
            content: "[interrupted] cancelled",
            timestamp_ms: 4,
          },
        ],
      },
    ]);

    // 横幅在该轮消息之前，且只有一条 —— 折叠不产出同 role 的占位把它顶掉
    expect(messages[0].isNotice).toBe(true);
    expect(messages[0].content).toContain(reason);
    expect(messages.filter((m) => m.isNotice)).toHaveLength(1);

    const card = cardsOf(messages)[0];
    expect(card).toMatchObject({ id: "c1", name: "shell", status: "done" });
    // 展开体渲染的就是 result —— 中断说明必须随卡片带出来
    expect(card?.result).toBe(interruptedOutput);
    expect(card?.orphan).toBeUndefined();

    // 横幅之外还有真实宿主，卡片没被折进横幅
    const hostIndex = messages.findIndex((m) => (m.toolCalls?.length ?? 0) > 0);
    expect(hostIndex).toBeGreaterThan(0);
    expect(messages[hostIndex].role).toBe("assistant");
  });
});

// ── 气泡 hover 时间戳 ──────────────────────────────────────────────────────
//
// 时间戳是气泡的直接子元素（不是 assistant-column 的下级），靠气泡自身的
// group + relative 在 hover 时显形。

describe("气泡 hover 时间戳", () => {
  const USER_TS = new Date(2026, 2, 1, 10, 0).getTime();
  const ASSISTANT_TS = USER_TS + 2_750;

  const bubble = (slot: "user-bubble" | "assistant-result") =>
    container.querySelector(`[data-slot="${slot}"]`) as HTMLElement | null;
  const timeIn = (el: HTMLElement | null) =>
    el?.querySelector('[data-slot="message-time"]') as HTMLElement | null;

  it("J1 user 气泡直接挂着时间戳，且自身是 hover 分组与定位基准", async () => {
    // 从快照 DTO 起渲染：回填链路断了这条也会红
    await renderMessages(
      snapshotToMessages([
        {
          turn_index: 0,
          messages: [{ role: "user", content: "你好", timestamp_ms: USER_TS }],
        },
      ]),
    );

    const el = bubble("user-bubble");
    const time = timeIn(el);
    expect(time).not.toBeNull();
    // 直接子元素：挪到外层包装上会让定位基准变成整行
    expect(time?.parentElement).toBe(el);
    expect(time?.textContent).toBe(formatMessageTime(USER_TS)!.label);
    // 渲染的是 label 而不是 iso
    expect(time?.textContent).not.toContain("T");
    expect(el).toHaveClass("group", "relative");
  });

  it("J2 assistant 正文气泡同样带时间戳与 hover 分组", async () => {
    await renderMessages([
      {
        id: "a1",
        role: "assistant",
        content: "答",
        turnIndex: 0,
        timestampMs: ASSISTANT_TS,
      },
    ]);

    const el = bubble("assistant-result");
    const time = timeIn(el);
    expect(time).not.toBeNull();
    expect(time?.parentElement).toBe(el);
    expect(time?.textContent).toBe(formatMessageTime(ASSISTANT_TS)!.label);
    expect(el).toHaveClass("group", "relative");
  });

  it("J3 快照只给 user 与正常 assistant 回填时间戳", () => {
    const turn: TurnData = {
      turn_index: 0,
      messages: [
        { role: "user", content: "问", timestamp_ms: USER_TS },
        {
          role: "assistant",
          content: "答",
          timestamp_ms: ASSISTANT_TS,
          tool_calls: [{ id: "call_a", name: "shell", arguments: "{}" }],
        },
        // tool_call_id 与上方的 call id 不匹配 —— 这条才会落成孤儿输出
        {
          role: "tool",
          content: "输出",
          tool_call_id: "call_b",
          timestamp_ms: 1,
        },
      ],
    };

    const messages = messagesFromTurn(turn);
    const byId = (id: string) => messages.find((m) => m.id === id);

    expect(byId("turn-0-0")?.role).toBe("user");
    expect(byId("turn-0-0")?.timestampMs).toBe(USER_TS);
    expect(byId("turn-0-1")?.role).toBe("assistant");
    expect(byId("turn-0-1")?.timestampMs).toBe(ASSISTANT_TS);
    // fixture 前提：孤儿条目真的产出了，否则下一条断言恒真
    expect(byId("turn-0-tool-orphans")).toBeDefined();
    expect(byId("turn-0-tool-orphans")?.timestampMs).toBeUndefined();

    // 中断横幅同理：它是分隔条，不是气泡
    const interrupted = snapshotToMessages([
      { ...turn, interrupted: true, interrupted_reason: "cancelled" },
    ]);
    const notice = interrupted.find((m) => m.id === "turn-0-notice");
    expect(notice?.isNotice).toBe(true);
    expect(notice?.timestampMs).toBeUndefined();
  });

  it("J4 缺 timestampMs 时不渲染时间戳（气泡本身照常渲染）", async () => {
    await renderMessages([
      { id: "u1", role: "user", content: "问", turnIndex: 0 },
      { id: "a1", role: "assistant", content: "答", turnIndex: 0 },
    ]);

    expect(bubble("user-bubble")).not.toBeNull();
    expect(bubble("assistant-result")).not.toBeNull();
    expect(container.querySelector('[data-slot="message-time"]')).toBeNull();
  });

  it("J5 时间戳靠 absolute + top-full 悬在气泡下方，opacity 由 group-hover 驱动", async () => {
    await renderMessages([
      {
        id: "u1",
        role: "user",
        content: "问",
        turnIndex: 0,
        timestampMs: USER_TS,
      },
      {
        id: "a1",
        role: "assistant",
        content: "答",
        turnIndex: 0,
        timestampMs: ASSISTANT_TS,
      },
    ]);

    const times = [
      ...container.querySelectorAll('[data-slot="message-time"]'),
    ] as HTMLElement[];
    expect(times).toHaveLength(2);
    for (const time of times) {
      expect(time.tagName).toBe("TIME");
      expect(time).toHaveClass(
        "absolute",
        "top-full",
        "opacity-0",
        "group-hover:opacity-100",
        "pointer-events-none",
        "select-none",
      );
      // 无动画：出现即终值，否则计算样式取到的是插值
      expect(time.className).not.toContain("transition");
      expect(time.className).not.toContain("tabular-nums");
    }
  });

  it("J6 工具卡组与推理行内没有时间戳", async () => {
    await renderMessages([
      {
        id: "a1",
        role: "assistant",
        content: "正文",
        turnIndex: 0,
        timestampMs: ASSISTANT_TS,
        reasoning: "想一想",
        toolCalls: [
          {
            id: "call_1",
            name: "shell",
            arguments: "{}",
            status: "done",
            result: "out",
          },
        ],
      },
    ]);

    const process = container.querySelector(
      '[data-slot="assistant-process"]',
    ) as HTMLElement;
    expect(process).not.toBeNull();
    expect(process.querySelector("time")).toBeNull();
    expect(
      container.querySelector('[data-slot="tool-call-group"] time'),
    ).toBeNull();
    // 反向保护：时间戳仍在，只是只属于正文气泡
    const times = [...container.querySelectorAll('[data-slot="message-time"]')];
    expect(times).toHaveLength(1);
    expect(times[0].parentElement).toBe(bubble("assistant-result"));
  });

  it("J10 非法 timestampMs 不渲染时间戳且不抛错", async () => {
    for (const ts of [0, Number.NaN, null as unknown as number]) {
      await expect(
        renderMessages([
          {
            id: "u1",
            role: "user",
            content: "问",
            turnIndex: 0,
            timestampMs: ts,
          },
          {
            id: "a1",
            role: "assistant",
            content: "答",
            turnIndex: 0,
            timestampMs: ts,
          },
        ]),
      ).resolves.toBeUndefined();

      expect(bubble("user-bubble")).not.toBeNull();
      expect(bubble("assistant-result")).not.toBeNull();
      expect(container.querySelector('[data-slot="message-time"]')).toBeNull();
    }
  });

  it("J11 本地模式实时构造：user 与 assistant 占位都带同一批次时间戳", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));

    const before = Date.now();
    await act(async () => {
      root.render(
        createElement(
          TooltipProvider,
          null,
          createElement(ChatView, {
            streamUrl: (m: string) => `/api/peco/stream?message=${m}`,
            initialQuery: "你好",
          }),
        ),
      );
    });
    // 占位气泡没有正文，要等首个增量才渲染出来
    s.push(frame("text_delta", { content: "在", conversation_id: "c" }));
    await act(async () => {});
    const after = Date.now();

    const ms = [
      ...container.querySelectorAll('[data-slot="message-time"]'),
    ].map((el) => new Date(el.getAttribute("dateTime") ?? "").getTime());

    expect(ms).toHaveLength(2);
    for (const value of ms) {
      expect(value).toBeGreaterThanOrEqual(before);
      expect(value).toBeLessThanOrEqual(after);
    }
    // 同一次发送的两条消息共享构造时刻
    expect(ms[0]).toBe(ms[1]);
  });

  it("J11 store 模式实时构造：userMsg 与 assistantMsg 都带同一批次时间戳", async () => {
    const s = controllableStream();
    global.fetch = vi.fn(() => Promise.resolve(s.response));

    const before = Date.now();
    await usePecoChatStore.getState().sendMessage("你好", "test-token");
    const after = Date.now();

    const messages = usePecoChatStore.getState().messages;
    expect(messages).toHaveLength(2);
    expect(messages[0].role).toBe("user");
    expect(messages[1].role).toBe("assistant");
    for (const message of messages) {
      expect(message.timestampMs).toBeGreaterThanOrEqual(before);
      expect(message.timestampMs).toBeLessThanOrEqual(after);
    }
    expect(messages[0].timestampMs).toBe(messages[1].timestampMs);
  });
});
