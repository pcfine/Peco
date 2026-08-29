// Chat types — aligned with peco-server /api/conversations/* and SSE events

export interface Conversation {
  id: string;
  title: string;
  agent_id?: string;
  agent_name: string;
  archived?: boolean;
  created_at: string;
  updated_at: string;
}

export interface Message {
  id: string;
  role: "user" | "assistant";
  content: string;
  agent_id?: string;
  agent_name?: string;
  created_at: string;
}

export interface UsageData {
  input_tokens: number;
  output_tokens: number;
}

// SSE events — tagged union matching ChatSseEvent on the backend
export type ChatSseEvent =
  | { event: "text_delta"; data: TextDeltaData }
  | { event: "reasoning_delta"; data: ReasoningDeltaData }
  | { event: "tool_call_start"; data: ToolCallStartData }
  | { event: "tool_result"; data: ToolResultData }
  | { event: "turn_complete"; data: TurnCompleteData }
  | { event: "agent_call_start"; data: AgentCallStartData }
  | { event: "agent_call_end"; data: AgentCallEndData }
  | { event: "done"; data: DoneData }
  | { event: "usage"; data: UsageEventData }
  | { event: "context_compacted"; data: ContextCompactedData }
  | { event: "error"; data: ErrorData };

export interface TextDeltaData {
  content: string;
  conversation_id: string;
}

export interface ReasoningDeltaData {
  content: string;
  conversation_id: string;
}

export interface ToolCallStartData {
  id: string;
  name: string;
  arguments: string;
  conversation_id: string;
}

export interface ToolResultData {
  id: string;
  name: string;
  result: string;
  conversation_id: string;
}

export interface TurnCompleteData {
  text: string;
  usage: UsageData;
  conversation_id: string;
}

export interface AgentCallStartData {
  call_id: string;
  agent_id: string;
  agent_name: string;
  task: string;
  conversation_id: string;
}

export interface AgentCallEndData {
  call_id: string;
  agent_id: string;
  agent_name: string;
  result: string;
  conversation_id: string;
}

export interface DoneData {
  usage: UsageData;
  conversation_id: string;
}

export interface UsageEventData {
  input_tokens: number;
  output_tokens: number;
  conversation_id: string;
}

export interface ErrorData {
  message: string;
  conversation_id: string;
}

/** 上下文滚动压缩完成 — 更早的对话轮次已被结构化摘要替换 */
export interface ContextCompactedData {
  evicted_turns: number;
  summary: string;
  conversation_id: string;
}

/** 单条压缩记录（时间线） */
export interface CompactionRecord {
  at: string;
  evicted_turns: number;
  tokens_before: number;
  tokens_after: number;
  summary_chars: number;
}

/** 上下文指标（GET /api/peco/session） */
export interface ContextMetrics {
  /** 压缩触发口径：pinned 摘要 + 全部 committed 轮（含 tool/reasoning）估算 token */
  estimated_total_tokens: number;
  /** Verbatim 预算口径：历史轮 viewable（User/Assistant 文本）估算 token */
  estimated_view_tokens: number;
  pinned_summary_tokens: number;
  history_token_budget: number;
  compaction_trigger_tokens: number;
  compaction_count: number;
  compactions: CompactionRecord[];
}

// Session snapshot (GET /api/conversations/:id/session)
export interface SessionSnapshotResponse {
  conversation_id: string;
  turns: TurnData[];
  total_usage: UsageData;
  /** 钉扎的历史摘要（compaction 产物，无压缩历史时缺省） */
  pinned_summary?: string;
  /** 上下文指标（会话不存在时缺省） */
  context_metrics?: ContextMetrics;
}

export interface TurnData {
  turn_index: number;
  messages: MessageData[];
}

export interface MessageData {
  role: string;
  content?: string;
  tool_calls?: ToolCallData[];
  reasoning_content?: string;
  tool_call_id?: string;
  timestamp_ms: number;
}

export interface ToolCallData {
  id: string;
  name: string;
  arguments: string;
}
