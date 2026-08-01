// Chat types — aligned with peco-server /api/conversations/* and SSE events

export interface Conversation {
  id: string
  title: string
  agent_id?: string
  agent_name: string
  archived?: boolean
  created_at: string
  updated_at: string
}

export interface Message {
  id: string
  role: 'user' | 'assistant'
  content: string
  agent_id?: string
  agent_name?: string
  created_at: string
}

export interface UsageData {
  input_tokens: number
  output_tokens: number
}

// SSE events — tagged union matching ChatSseEvent on the backend
export type ChatSseEvent =
  | { event: 'text_delta'; data: TextDeltaData }
  | { event: 'reasoning_delta'; data: ReasoningDeltaData }
  | { event: 'tool_call_start'; data: ToolCallStartData }
  | { event: 'tool_result'; data: ToolResultData }
  | { event: 'turn_complete'; data: TurnCompleteData }
  | { event: 'agent_call_start'; data: AgentCallStartData }
  | { event: 'agent_call_end'; data: AgentCallEndData }
  | { event: 'done'; data: DoneData }
  | { event: 'error'; data: ErrorData }

export interface TextDeltaData {
  content: string
  conversation_id: string
}

export interface ReasoningDeltaData {
  content: string
  conversation_id: string
}

export interface ToolCallStartData {
  id: string
  name: string
  arguments: string
  conversation_id: string
}

export interface ToolResultData {
  id: string
  name: string
  result: string
  conversation_id: string
}

export interface TurnCompleteData {
  text: string
  usage: UsageData
  conversation_id: string
}

export interface AgentCallStartData {
  call_id: string
  agent_id: string
  agent_name: string
  task: string
  conversation_id: string
}

export interface AgentCallEndData {
  call_id: string
  agent_id: string
  agent_name: string
  result: string
  conversation_id: string
}

export interface DoneData {
  usage: UsageData
  conversation_id: string
}

export interface ErrorData {
  message: string
  conversation_id: string
}

// Session snapshot (GET /api/conversations/:id/session)
export interface SessionSnapshotResponse {
  conversation_id: string
  turns: TurnData[]
  total_usage: UsageData
}

export interface TurnData {
  turn_index: number
  messages: MessageData[]
}

export interface MessageData {
  role: string
  content?: string
  tool_calls?: ToolCallData[]
  reasoning_content?: string
  tool_call_id?: string
  timestamp_ms: number
}

export interface ToolCallData {
  id: string
  name: string
  arguments: string
}
