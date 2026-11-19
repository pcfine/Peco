// daemon HTTP 视图的字段镜像 —— 与服务端 DTO 逐字段对应，不在这里派生新语义。

export interface MarkerInfo {
  team: string;
  user_id: string;
  session_id: string;
  /** 人读名（新建空间时填的「显示名称」）。 */
  name: string;
}

/** `GET /api/spaces` 的一行。 */
export interface Space {
  ws_id: string;
  work_dir: string;
  team_id: string;
  /** 库里的逻辑名；缺省为 null。显示优先级见 `spaceLabel`。 */
  label: string | null;
  /** marker 里的人读名；marker 缺失或损坏时为 null。显示优先级见 `spaceLabel`。 */
  name: string | null;
  exit_policy: string;
  effective_exit_policy: string;
  /** `open` = 实例在 Hub 里；`closed` = 本次未打开。 */
  status: "open" | "closed";
  /** 显式关闭的时刻；重新打开会清空。三组划分靠它与 `last_opened_at`。 */
  closed_at: string | null;
  /** 最近一次成功打开的时刻；从未打开过为 null。 */
  last_opened_at: string | null;
  reserved: boolean;
  inflight_dispatch: number;
  inflight_foreign: number;
  marker: MarkerInfo | null;
}

/**
 * `POST /api/spaces/{ws_id}/conversation/ensure` 的回执。
 *
 * 每个空间的 `@master` 只有一条会话：`created=false` 表示已有、原样取回。
 */
export interface SpaceConversation {
  conversation_id: string;
  created: boolean;
}

export interface SpaceOpened {
  ws_id: string;
  work_dir: string;
  team_id: string;
  agents: string[];
}

export interface CreateSpaceRequest {
  work_dir: string;
  team_id: string;
  /** 「显示名称」：写进 marker 的 `name`，不是 `label`。 */
  name?: string;
  label?: string;
}

export interface PecoIdentity {
  ws_id: string;
  work_dir: string;
  team_id: string;
  reserved: boolean;
  from_marker: boolean;
}

export interface Team {
  team_id: string;
  display_name: string;
  template_version: number;
  /** `/uploads/...` URL；空串表示未设置。 */
  icon: string;
  agents: string[];
  bound_spaces: string[];
  /** 保留团队（Peco）标记：默认清单不含它，`?include=peco` 才带。 */
  reserved?: boolean;
  /** 计数由服务端聚合；缺失时显示「–」。 */
  agent_count?: number;
  skill_count?: number;
  mcp_count?: number;
  space_count?: number;
}

export interface TeamAgent {
  name: string;
  description: string;
}

export interface TeamKnowledgeBase {
  name: string;
  document_count: number;
}

export interface TeamSpace {
  ws_id: string;
  work_dir: string;
  closed_at: string | null;
}

export interface TeamDetail {
  id: string;
  display_name: string;
  template_version: number;
  /** `/uploads/...` URL；空串表示未设置。 */
  icon: string;
  root: string;
  has_master: boolean;
  agents: TeamAgent[];
  skills: string[];
  mcp_servers: string[];
  knowledge_bases: TeamKnowledgeBase[];
  spaces: TeamSpace[];
}

export interface CreateTeamRequest {
  id: string;
  display_name?: string;
  /** 内置模板名；与 `from_team` 二选一。 */
  from_template?: string;
  /** 复制来源团队 id；与 `from_template` 二选一。 */
  from_team?: string;
}

/** `PATCH /api/teams/{id}` 的请求体；空串表示清空图标。 */
export interface UpdateTeamRequest {
  icon: string;
}

/**
 * 唤醒投递的状态；与任务生命周期是两套独立状态，一条任务可以已终态而唤醒仍待投递。
 *
 * `awaiting` 是「任务已注册、还没终态」，此时还没进唤醒队列。
 */
export type WakeState =
  "awaiting" | "pending" | "delivering" | "delivered" | "skipped" | "failed";

/** `wake_state='skipped'` 的成因；历史行没有记录，显示为「原因未记录」。 */
export type WakeSkipReason =
  | "not_peco_origin"
  | "peco_identity_missing"
  | "orphaned"
  | "space_closed"
  | "attempts_exhausted";

/** 台账一行的通用形状：`/api/tasks` 与 `/api/events` 共用同名载荷。 */
export interface Task {
  id: string;
  task_id: string;
  target_ws_id: string;
  target_work_dir: string;
  target_team_id: string;
  origin_ws_id: string;
  origin_kind: string;
  agent_name: string;
  task_state: string;
  /** `delivered` 只表示已交给运行器，不代表任务处理完成。 */
  wake_state: WakeState;
  wake_skip_reason: WakeSkipReason | null;
  created_at: string;
  terminal_at: string | null;
  result_digest: string | null;
  /** 派发入参里那段任务描述的前 512 字；旧行没有记录，是 `null`。 */
  prompt_digest?: string | null;
  /** 派发回执里的 dispatch_id；旧行没有记录，是 `null`。 */
  dispatch_id?: string | null;
}

/** `/api/events` 的 `task` 事件载荷：只是台账变更信号，行本身要重取。 */
export interface TaskEvent {
  /**
   * 事件种类；缺省即台账行变更。
   *
   * `wake_turn_done` 是另一回事：一轮唤醒在服务端跑完，会话里多出了反馈消息，
   * 需要按 `conversation_id` 重取会话历史。这种载荷只有 `ws_id` 与
   * `conversation_id` 有意义，其余字段是空串或 null。
   */
  kind?: string;
  /** 哪条会话的历史变了；只有 `wake_turn_done` 会给。 */
  conversation_id?: string | null;
  dispatch_id?: string | null;
  ws_id: string;
  task_id: string;
  task_state: string;
  wake_state: WakeState;
  wake_skip_reason: WakeSkipReason | null;
  origin_kind: string;
  created_at: string;
  terminal_at: string | null;
}

/** `tag` + `payload`：载荷形状由执行体定义，UI 只按需读。 */
export interface TaggedJson {
  tag: string;
  payload: unknown;
}

/** 快照的进度：三格都可缺，`current` / `total` 只表示数量，单位由执行体决定。 */
export interface LiveTaskProgress {
  phase: string | null;
  current: number | null;
  total: number | null;
}

/** 输出窗口：按字节裁剪，首尾都可能落在多字节字符中间，解码交给展示层。 */
export interface LiveTaskOutput {
  tail: number[];
  start_offset: number;
  end_offset: number;
}

export interface LiveTaskArtifact {
  id: string;
  kind: string;
  offset: number;
  append: boolean;
  last_chunk: boolean;
  payload: TaggedJson;
}

export interface LiveTaskError {
  kind: string;
  message: string;
  detail: TaggedJson | null;
}

/** 仅终态非 `null`；终态本身由 `state` 承载，两处不互为判据。 */
export interface LiveTaskOutcome {
  output: TaggedJson | null;
  error: LiveTaskError | null;
}

/**
 * `GET /api/spaces/{ws_id}/tasks/live` 的一行：空间 TaskHub 的内存快照。
 *
 * 列表含已终态任务（槽在空间存活期内常驻），在飞与已完成靠 `state` 区分。
 * `created_at` / `updated_at` 是 chrono 的 RFC3339，与台账的 `"YYYY-MM-DD hh:mm:ss"` 不同形。
 */
export interface LiveTaskView {
  id: string;
  kind: string;
  state: string;
  state_detail: TaggedJson | null;
  progress: LiveTaskProgress;
  output: LiveTaskOutput;
  outcome: LiveTaskOutcome | null;
  artifacts: LiveTaskArtifact[];
  created_at: string;
  updated_at: string;
}

export interface TodayCounts {
  completed: number;
  failed: number;
  active: number;
  waiting: number;
}

export interface RecentTask {
  id: string;
  task_id: string;
  task_state: string;
  wake_state: WakeState;
  wake_skip_reason: WakeSkipReason | null;
  agent_name: string;
  origin_kind: string;
  created_at: string;
  terminal_at: string | null;
  digest: string | null;
}

export interface SpaceOverview {
  ws_id: string;
  team_id: string;
  label: string | null;
  closed_at: string | null;
  inflight: { dispatch: number; foreign: number };
  tasks: { today: TodayCounts; recent: RecentTask[] };
  agent_count: number;
  skill_count: number;
}

export interface SpaceArtifact {
  id: string;
  conversation_id: string;
  agent_name: string;
  kind: string;
  rel_path: string;
  size_bytes: number;
  created_at: string;
}

export interface SpaceArtifactList {
  items: SpaceArtifact[];
  total: number;
}

/**
 * `GET /api/spaces/{ws_id}/memory/documents` 的一行。
 *
 * `category` 是 KB 的分类标签（`profile` / `semantic` / `episodic`），不在此枚举：
 * 后端新增分类时前端应当照原样显示，而不是把行整条吞掉。
 */
export interface MemoryDocumentSummary {
  id: string;
  title: string;
  /** 原文档的来源路径，只用于展示与详情。 */
  source: string;
  category: string;
  created_at: string;
  snippet: string;
}

/** 同一个端点的整页响应：`total` 是过滤后的总数，与 `items.length` 可不等。 */
export interface MemoryDocumentList {
  total: number;
  items: MemoryDocumentSummary[];
}

/** `GET /api/spaces/{ws_id}/memory/documents/{doc_id}`：`content` 是全文。 */
export interface MemoryDocumentDetail {
  id: string;
  title: string;
  content: string;
  source: string;
  category: string;
  created_at: string;
  /** 形状由后端决定（KB 的 `DocumentMetadata` 投影），本面板只读不解释。 */
  metadata: Record<string, unknown>;
}

/** `/files` 的一行：目录只给名字，文件还带字节数与时刻。 */
export interface SpaceFileEntry {
  name: string;
  kind: "dir" | "file";
  size_bytes?: number;
  modified_at?: string;
}

/** `path` 指向目录时的响应。 */
export interface SpaceDirListing {
  path: string;
  entries: SpaceFileEntry[];
  truncated: boolean;
}

/** `path` 指向文件时的响应；二进制不给 `text`。 */
export interface SpaceFileContent {
  path: string;
  kind: "file";
  size_bytes: number;
  text?: string;
  truncated: boolean;
  binary: boolean;
}

export type SpaceFilesResponse = SpaceDirListing | SpaceFileContent;

/** `/api/agents` 的列表项。正文只在详情里给，列表不带。 */
export interface AgentListItem {
  id: string;
  name: string;
  description: string;
  model: string | null;
  provider: string | null;
  status: string;
  tools: string[];
  mcp_servers: string[];
  skills: string[];
  knowledge_bases: string[];
  /** 名字由 core 约定，前端不写死 `@master` 字符串。 */
  is_master: boolean;
  created_at: string;
}

/** `/api/agents/{id}` 的详情；`system_prompt` 就是 agent.md 的正文。 */
export interface AgentDetail {
  id: string;
  name: string;
  description: string;
  system_prompt: string;
  model: string | null;
  provider: string | null;
  status: string;
  tools: string[];
  mcp_servers: string[];
  skills: string[];
  knowledge_bases: string[];
  temperature: number | null;
  max_tokens: number | null;
  stream: boolean | null;
  reasoning_effort: string | null;
  max_iterations: number;
  supports_images: boolean;
  created_at: string;
  updated_at: string;
}

/** `POST /api/agents` 的请求体：只有 `name` 必填，省略的字段由服务端补默认值。 */
export interface CreateAgentRequest {
  name: string;
  description?: string;
  system_prompt?: string;
  model?: string;
  provider?: string;
  tools?: string[];
  mcp_servers?: string[];
  skills?: string[];
  temperature?: number;
  max_tokens?: number;
  stream?: boolean;
  reasoning_effort?: string;
  max_iterations?: number;
  knowledge_bases?: string[];
}

/**
 * `PATCH /api/agents/{id}` 的请求体：**省略 = 不改**。
 *
 * `model` / `provider` / `temperature` / `max_tokens` / `stream` / `reasoning_effort`
 * 服务端按 `Option<Option<T>>` 解析，显式 `null` 才是「清空」，与省略不是一回事。
 */
export interface UpdateAgentRequest {
  name?: string;
  description?: string;
  system_prompt?: string;
  model?: string | null;
  provider?: string | null;
  tools?: string[];
  mcp_servers?: string[];
  skills?: string[];
  temperature?: number | null;
  max_tokens?: number | null;
  stream?: boolean | null;
  reasoning_effort?: string | null;
  max_iterations?: number;
  knowledge_bases?: string[];
}

export interface SkillInfo {
  name: string;
  description: string;
}

/**
 * `POST /api/mcp/{name}/test` 的结果。连不上也是 200：`success=false` 走 `message`，
 * 只有「server 名不在配置里」之类的前置错误才是 HTTP 错误。
 */
export interface McpTestResult {
  success: boolean;
  server: string;
  /** `stdio` / `sse` / `streamable_http`；配置缺失时不下发。 */
  transport?: string;
  tool_count: number;
  tools: string[];
  message: string;
  duration_ms: number;
  error_type?: string;
}

/** `POST /api/knowledge` 的请求体；新建表单只用 name + description。 */
export interface CreateKnowledgeBaseRequest {
  name: string;
  description?: string;
}

/** `/api/knowledge` 的一行；`document_count` 由服务端聚合，前端不再数一遍。 */
export interface KnowledgeBase {
  id: string;
  name: string;
  description: string;
  backend: string;
  embedding_model: string;
  document_count: number;
  chunk_count: number;
  created_at: string;
  updated_at: string;
}

/**
 * `GET /api/schedules` 的一行。
 *
 * 时间字段是 UTC 文本 `YYYY-MM-DD HH:MM:SS`（无时区后缀），展示前必须按本机
 * 时区换算，走 `parseServerTimestamp`。
 */
export interface ScheduleView {
  id: string;
  name: string;
  target_ws_id: string;
  /** 目标空间的人读名；后端给不出时是空串，展示层回落到 `target_ws_id`。 */
  target_label: string;
  /** 定时投进目标空间 `@master` 会话的那段话。 */
  prompt: string;
  cron_expr: string;
  enabled: boolean;
  next_run_at: string | null;
  last_run_at: string | null;
  last_status:
    "completed" | "failed" | "skipped" | "timeout" | "interrupted" | null;
  created_at: string;
  updated_at: string;
}

export type ScheduleRunStatus =
  | "pending"
  | "running"
  | "completed"
  | "failed"
  | "skipped"
  | "timeout"
  | "interrupted";

/** `GET /api/schedules/{id}/runs` 的一行。 */
export interface ScheduleRun {
  id: string;
  schedule_id: string;
  trigger_kind: "cron" | "manual";
  scheduled_for: string;
  status: ScheduleRunStatus;
  /** 已经尝试过几次；`pending` 行按它报「第 N 次」。 */
  attempts: number;
  conversation_id: string | null;
  started_at: string;
  finished_at: string | null;
  result_digest: string | null;
  error: string | null;
  notify_state: "none" | "pending" | "sent" | "skipped" | "failed";
  notified_at: string | null;
}

/** `POST /api/schedules` 的请求体；`enabled` 缺省由服务端补 `true`。 */
export interface CreateScheduleRequest {
  name: string;
  target_ws_id: string;
  prompt: string;
  cron_expr: string;
  enabled?: boolean;
}

/** `PATCH /api/schedules/{id}` 的请求体：省略即不改。 */
export interface UpdateScheduleRequest {
  name?: string;
  target_ws_id?: string;
  prompt?: string;
  cron_expr?: string;
  enabled?: boolean;
}

/** `POST /api/schedules/preview` 的回执：最多 3 个 UTC 文本时刻。 */
export interface CronPreview {
  next: string[];
}

/**
 * `POST /api/schedules/{id}/run` 的回执：`pending` 表示目标会话忙，已排队；
 * `failed` 表示同步启动就失败了，原因在运行历史里。
 */
export interface ScheduleRunAccepted {
  run_id: string;
  status: "running" | "pending" | "failed";
}
