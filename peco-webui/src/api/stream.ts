/**
 * SSE stream adapter — maps peco-server SSE events to structured data.
 *
 * All mapping functions are pure — easy to unit test.
 */

import type {
  ChatSseEvent,
  TextDeltaData,
  ReasoningDeltaData,
  ToolCallStartData,
  ToolResultData,
  TurnCompleteData,
  AgentCallStartData,
  AgentCallEndData,
  DoneData,
  UsageEventData,
  ContextCompactedData,
  TruncationRetryData,
  LooperStateData,
  UserSteerData,
  ErrorData,
  TurnSnapshotData,
} from "@/types/chat";

/** Parsed SSE event from the wire */
export interface ParsedSSEEvent {
  event: string;
  data: Record<string, unknown>;
  /**
   * SSE `id:` 字段 —— 服务端按轮单调分配的帧序号（每轮从 1 起）。
   *
   * 缺省表示该帧没带 id（旧服务端），此时无从去重，调用方按「新帧」应用。
   */
  id?: string;
}

/**
 * Parse a raw SSE line buffer into parsed events.
 * Handles partial lines (incomplete chunks).
 */
/**
 * Parse SSE events from one network chunk.
 *
 * `pendingEvent` / `pendingId` carry the most recently seen `event:` / `id:`
 * lines across calls: when a `data:` line spans chunk boundaries, both must
 * survive — otherwise the completed line is emitted with an empty event name
 * (silently dropped by `toChatSseEvent`) or without its sequence number (then
 * the frame can no longer be deduplicated against a `turn_snapshot`). Thread
 * the returned `pendingEvent` and `pendingId` (together with `remaining`) into
 * the next call.
 */
export function parseSSELines(
  chunk: string,
  buffer: string,
  pendingEvent = "",
  pendingId = "",
): {
  events: ParsedSSEEvent[];
  remaining: string;
  pendingEvent: string;
  pendingId: string;
} {
  const lines = (buffer + chunk).split("\n");
  const remaining = lines.pop() ?? "";

  const events: ParsedSSEEvent[] = [];
  let currentEvent = pendingEvent;
  let currentId = pendingId;
  // 本事件块里刚推出的那一帧：`id:` 可能写在 `data:` 之后（axum 按构建顺序
  // 落字段），序号又是去重的唯一依据，所以同块内要把 id 补到它身上。
  let blockEvent: ParsedSSEEvent | null = null;

  for (const line of lines) {
    if (line.startsWith("event: ")) {
      currentEvent = line.slice(7).trim();
    } else if (line.startsWith("id: ")) {
      currentId = line.slice(4).trim();
      if (blockEvent && blockEvent.id === undefined && currentId !== "") {
        blockEvent.id = currentId;
      }
    } else if (line.startsWith("data: ")) {
      try {
        const raw = JSON.parse(line.slice(6));
        // peco-server SSE: data is {event, data} or plain data with event on the event: line
        const data = raw.data ?? raw;
        blockEvent = { event: currentEvent, data };
        if (currentId !== "") blockEvent.id = currentId;
        events.push(blockEvent);
      } catch {
        // Skip unparseable lines (incomplete chunks)
      }
    } else if (line.trim() === "") {
      // Blank line = event dispatch boundary per the SSE spec — reset the
      // event type (and id) so orphan data lines can't inherit them.
      currentEvent = "";
      currentId = "";
      blockEvent = null;
    }
  }

  return {
    events,
    remaining,
    pendingEvent: currentEvent,
    pendingId: currentId,
  };
}

/**
 * `id:` 行的序号投影 —— 缺省/非数字返回 null（旧服务端不带 id）。
 *
 * 返回 null 表示「无从判断重叠」，调用方按新帧应用。
 */
export function frameSeq(id: string | undefined): number | null {
  if (id === undefined || id === "") return null;
  const seq = Number(id);
  return Number.isFinite(seq) ? seq : null;
}

/** Map a parsed SSE event into a typed ChatSseEvent */
export function toChatSseEvent(parsed: ParsedSSEEvent): ChatSseEvent | null {
  const { event, data } = parsed;

  switch (event) {
    case "text_delta":
      return { event, data: data as unknown as TextDeltaData };
    case "reasoning_delta":
      return { event, data: data as unknown as ReasoningDeltaData };
    case "tool_call_start":
      return { event, data: data as unknown as ToolCallStartData };
    case "tool_result":
      return { event, data: data as unknown as ToolResultData };
    case "turn_complete":
      return { event, data: data as unknown as TurnCompleteData };
    case "agent_call_start":
      return { event, data: data as unknown as AgentCallStartData };
    case "agent_call_end":
      return { event, data: data as unknown as AgentCallEndData };
    case "done":
      return { event, data: data as unknown as DoneData };
    case "usage":
      return { event, data: data as unknown as UsageEventData };
    case "context_compacted":
      return { event, data: data as unknown as ContextCompactedData };
    case "truncation_retry":
      return { event, data: data as unknown as TruncationRetryData };
    case "looper_state":
      return { event, data: data as unknown as LooperStateData };
    case "user_steer":
      return { event, data: data as unknown as UserSteerData };
    case "turn_snapshot":
      return { event, data: data as unknown as TurnSnapshotData };
    case "error":
      return { event, data: data as unknown as ErrorData };
    default:
      return null;
  }
}
