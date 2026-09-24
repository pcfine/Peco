// ChatView reducer 单测 — tool_result images 附加 + 快照用户图片恢复

import { describe, expect, it } from "vitest";
import {
  reduceStreamEvent,
  snapshotToMessages,
  type ChatMessage,
} from "../ChatView";
import type { TurnData } from "../../../types/chat";

const USER_TEXT = "看看这张图";

function assistantWithTool(id = "call_1"): ChatMessage[] {
  return [
    { role: "user", content: USER_TEXT, turnIndex: 0 },
    {
      role: "assistant",
      content: "",
      turnIndex: 0,
      toolCalls: [{ id, name: "shell", arguments: "{}" }],
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
