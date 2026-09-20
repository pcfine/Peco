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
