// ChatView reducer 单测 — tool_result images 附加

import { describe, expect, it } from "vitest";
import { reduceStreamEvent, type ChatMessage } from "../ChatView";

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
    const updated = reduceStreamEvent
      (
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
