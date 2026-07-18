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
  ErrorData,
} from '@/types/chat'

/** Parsed SSE event from the wire */
export interface ParsedSSEEvent {
  event: string
  data: Record<string, unknown>
}

/**
 * Parse a raw SSE line buffer into parsed events.
 * Handles partial lines (incomplete chunks).
 */
export function parseSSELines(chunk: string, buffer: string): {
  events: ParsedSSEEvent[]
  remaining: string
} {
  const lines = (buffer + chunk).split('\n')
  const remaining = lines.pop() ?? ''

  const events: ParsedSSEEvent[] = []
  let currentEvent = ''

  for (const line of lines) {
    if (line.startsWith('event: ')) {
      currentEvent = line.slice(7).trim()
    } else if (line.startsWith('data: ')) {
      try {
        const raw = JSON.parse(line.slice(6))
        // peco-server SSE: data is {event, data} or plain data with event on the event: line
        const data = raw.data ?? raw
        events.push({ event: currentEvent, data })
      } catch {
        // Skip unparseable lines (incomplete chunks)
      }
    }
  }

  return { events, remaining }
}

/** Map a parsed SSE event into a typed ChatSseEvent */
export function toChatSseEvent(parsed: ParsedSSEEvent): ChatSseEvent | null {
  const { event, data } = parsed

  switch (event) {
    case 'text_delta':
      return { event, data: data as unknown as TextDeltaData }
    case 'reasoning_delta':
      return { event, data: data as unknown as ReasoningDeltaData }
    case 'tool_call_start':
      return { event, data: data as unknown as ToolCallStartData }
    case 'tool_result':
      return { event, data: data as unknown as ToolResultData }
    case 'turn_complete':
      return { event, data: data as unknown as TurnCompleteData }
    case 'agent_call_start':
      return { event, data: data as unknown as AgentCallStartData }
    case 'agent_call_end':
      return { event, data: data as unknown as AgentCallEndData }
    case 'done':
      return { event, data: data as unknown as DoneData }
    case 'error':
      return { event, data: data as unknown as ErrorData }
    default:
      return null
  }
}
