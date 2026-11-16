import { describe, expect, it } from "vitest";
import { frameSeq, parseSSELines, toChatSseEvent } from "@/api/stream";

// 每帧的 SSE `id:` 是重连去重的唯一依据（`turn_snapshot.seq` 之后的重叠帧必须
// 丢得掉）。axum 按字段构建顺序落行，所以 id 出现在 data 前后都要认得出来。

/** 完整喂一块（一次调用即读完的流），返回所有帧。 */
function parseAll(chunk: string) {
  return parseSSELines(chunk, "").events;
}

describe("parseSSELines：id 解析", () => {
  it("event → id → data（服务端构建顺序）带回帧序号", () => {
    const [first] = parseAll(
      'event: text_delta\nid: 7\ndata: {"content":"A","conversation_id":"c1"}\n\n',
    );
    expect(first.event).toBe("text_delta");
    expect(first.id).toBe("7");
    expect(first.data).toEqual({ content: "A", conversation_id: "c1" });
  });

  it("id 落在 data 之后也补到同一帧上（行序不该决定去重能力）", () => {
    const [first] = parseAll(
      'event: text_delta\ndata: {"content":"A","conversation_id":"c1"}\nid: 9\n\n',
    );
    expect(first.id).toBe("9");
  });

  it("id 行与 data 行被切在两块里，靠 pendingId 续上", () => {
    const head = parseSSELines("event: text_delta\nid: 11\n", "");
    expect(head.events).toEqual([]);
    expect(head.pendingId).toBe("11");

    const tail = parseSSELines(
      'data: {"content":"B","conversation_id":"c1"}\n\n',
      head.remaining,
      head.pendingEvent,
      head.pendingId,
    );
    expect(tail.events).toHaveLength(1);
    expect(tail.events[0].id).toBe("11");
  });

  it("空行是分发边界：后面孤立 data 行不继承上一块的 id", () => {
    const events = parseAll(
      'event: text_delta\nid: 3\ndata: {"content":"A","conversation_id":"c1"}\n\n' +
        'data: {"content":"B","conversation_id":"c1"}\n\n',
    );
    expect(events).toHaveLength(2);
    expect(events[0].id).toBe("3");
    expect(events[1].id).toBeUndefined();
    expect(events[1].event).toBe("");
  });

  it("旧服务端不带 id 时帧仍照常解析（行为不变）", () => {
    const [first] = parseAll(
      'event: done\ndata: {"usage":{"input_tokens":1,"output_tokens":2},"conversation_id":"c1"}\n\n',
    );
    expect(first.event).toBe("done");
    expect(first.id).toBeUndefined();
  });
});

describe("frameSeq", () => {
  it("数字 id 投影成序号，缺省/非数字返回 null（无从判断重叠）", () => {
    expect(frameSeq("12")).toBe(12);
    expect(frameSeq("0")).toBe(0);
    expect(frameSeq(undefined)).toBeNull();
    expect(frameSeq("")).toBeNull();
    expect(frameSeq("abc")).toBeNull();
  });
});

describe("toChatSseEvent：turn_snapshot", () => {
  const payload = {
    conversation_id: "c1",
    seq: 5,
    phase: "paused",
    text: "AB",
    reasoning: "想一想",
    tool_calls: [
      {
        id: "t1",
        name: "read_file",
        arguments: '{"path":"a.ts"}',
        status: "done",
        result: "ok",
      },
    ],
    agent_calls: [
      {
        call_id: "call_1",
        agent_id: "agent_1",
        agent_name: "研究员",
        status: "running",
      },
    ],
    usage: { input_tokens: 10, output_tokens: 20 },
  };

  it("映射成带类型的事件，字段逐个到位", () => {
    const [parsed] = parseAll(
      `event: turn_snapshot\nid: 5\ndata: ${JSON.stringify(payload)}\n\n`,
    );
    const event = toChatSseEvent(parsed);
    expect(event).toEqual({ event: "turn_snapshot", data: payload });
    if (event?.event !== "turn_snapshot") throw new Error("类型不对");
    expect(event.data.seq).toBe(5);
    expect(event.data.phase).toBe("paused");
    expect(event.data.tool_calls[0].status).toBe("done");
    expect(event.data.agent_calls[0].call_id).toBe("call_1");
    expect(event.data.usage).toEqual({ input_tokens: 10, output_tokens: 20 });
  });

  it("未知事件仍返回 null（旧行为不变）", () => {
    expect(
      toChatSseEvent({ event: "brand_new_event", data: { x: 1 } }),
    ).toBeNull();
  });
});
