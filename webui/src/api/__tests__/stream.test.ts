import { describe, it, expect } from "vitest";
import { parseSSELines, toChatSseEvent } from "../stream";

describe("parseSSELines", () => {
  it("parses a single text_delta event", () => {
    const chunk =
      'event: text_delta\ndata: {"content":"你好","conversation_id":"abc"}\n\n';
    const { events, remaining } = parseSSELines(chunk, "");

    expect(events).toHaveLength(1);
    expect(events[0].event).toBe("text_delta");
    expect(events[0].data).toEqual({ content: "你好", conversation_id: "abc" });
    expect(remaining).toBe("");
  });

  it("parses multiple events in one chunk", () => {
    const chunk = [
      'event: text_delta\ndata: {"content":"A","conversation_id":"1"}\n',
      'event: text_delta\ndata: {"content":"B","conversation_id":"1"}\n',
      'event: done\ndata: {"usage":{"input_tokens":10,"output_tokens":5},"conversation_id":"1"}\n\n',
    ].join("\n");

    const { events } = parseSSELines(chunk, "");

    expect(events).toHaveLength(3);
    expect(events[0].event).toBe("text_delta");
    expect(events[1].event).toBe("text_delta");
    expect(events[2].event).toBe("done");
  });

  it("handles partial chunks (buffered remaining)", () => {
    // First chunk: incomplete line
    const { events: e1, remaining: r1 } = parseSSELines(
      'event: text_delta\ndata: {"content":"hel',
      "",
    );
    expect(e1).toHaveLength(0);
    expect(r1).toBe('data: {"content":"hel');

    // Second chunk: completes the line
    const { events: e2, remaining: r2 } = parseSSELines(
      'lo","conversation_id":"1"}\n\n',
      r1,
    );
    expect(e2).toHaveLength(1);
    expect(e2[0].data.content).toBe("hello");
    expect(r2).toBe("");
  });

  it("handles tool_call_start event", () => {
    const chunk =
      'event: tool_call_start\ndata: {"id":"call_1","name":"shell","arguments":"{\\"cmd\\":\\"ls\\"}","conversation_id":"1"}\n\n';

    const { events } = parseSSELines(chunk, "");

    expect(events).toHaveLength(1);
    expect(events[0].event).toBe("tool_call_start");
    expect(events[0].data.id).toBe("call_1");
    expect(events[0].data.name).toBe("shell");
  });

  it("handles agent_call_start with call_id", () => {
    const chunk =
      'event: agent_call_start\ndata: {"call_id":"call_abc","agent_id":"a1","agent_name":"CodeReviewer","task":"review","conversation_id":"1"}\n\n';

    const { events } = parseSSELines(chunk, "");

    expect(events).toHaveLength(1);
    expect(events[0].event).toBe("agent_call_start");
    expect(events[0].data.call_id).toBe("call_abc");
    expect(events[0].data.agent_name).toBe("CodeReviewer");
  });

  it("handles agent_call_end with matching call_id", () => {
    const chunk =
      'event: agent_call_end\ndata: {"call_id":"call_abc","agent_id":"a1","agent_name":"CodeReviewer","result":"done","conversation_id":"1"}\n\n';

    const { events } = parseSSELines(chunk, "");

    expect(events).toHaveLength(1);
    expect(events[0].event).toBe("agent_call_end");
    expect(events[0].data.call_id).toBe("call_abc");
  });

  it("ignores unparseable data lines", () => {
    const chunk = "event: text_delta\ndata: {broken json\n\n";
    const { events } = parseSSELines(chunk, "");
    expect(events).toHaveLength(0);
  });

  it("handles keepalive comments (lines starting with :)", () => {
    const chunk =
      ': keep-alive\n\nevent: done\ndata: {"usage":{"input_tokens":0,"output_tokens":0},"conversation_id":"1"}\n\n';
    const { events } = parseSSELines(chunk, "");
    expect(events).toHaveLength(1);
    expect(events[0].event).toBe("done");
  });
});

describe("toChatSseEvent", () => {
  it("maps text_delta to typed event", () => {
    const result = toChatSseEvent({
      event: "text_delta",
      data: { content: "hi", conversation_id: "1" },
    });
    expect(result).not.toBeNull();
    expect(result!.event).toBe("text_delta");
  });

  it("maps agent_call_start with call_id", () => {
    const result = toChatSseEvent({
      event: "agent_call_start",
      data: {
        call_id: "call_1",
        agent_id: "a1",
        agent_name: "Bot",
        task: "do stuff",
        conversation_id: "1",
      },
    });
    expect(result).not.toBeNull();
    expect(result!.event).toBe("agent_call_start");
  });

  it("returns null for unknown event types", () => {
    const result = toChatSseEvent({
      event: "unknown_event",
      data: {},
    });
    expect(result).toBeNull();
  });

  it("maps usage to typed event", () => {
    const result = toChatSseEvent({
      event: "usage",
      data: { input_tokens: 1000, output_tokens: 200, conversation_id: "1" },
    });
    expect(result).not.toBeNull();
    expect(result!.event).toBe("usage");
  });

  it("maps all 10 known event types", () => {
    const events = [
      "text_delta",
      "reasoning_delta",
      "tool_call_start",
      "tool_result",
      "turn_complete",
      "agent_call_start",
      "agent_call_end",
      "done",
      "usage",
      "error",
    ];
    for (const evt of events) {
      const result = toChatSseEvent({ event: evt, data: {} });
      expect(result, `Failed for event: ${evt}`).not.toBeNull();
      expect(result!.event).toBe(evt);
    }
  });
});
