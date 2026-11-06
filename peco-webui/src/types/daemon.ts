// daemon HTTP 视图的字段镜像 —— 与服务端 DTO 逐字段对应，不在这里派生新语义。

export interface MarkerInfo {
  team: string;
  user_id: string;
  session_id: string;
}

/** `GET /api/spaces` 的一行。 */
export interface Space {
  ws_id: string;
  work_dir: string;
  team_id: string;
  label: string | null;
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

export interface SpaceOpened {
  ws_id: string;
  work_dir: string;
  team_id: string;
  agents: string[];
}

export interface CreateSpaceRequest {
  work_dir: string;
  team_id: string;
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
  agents: string[];
  bound_spaces: string[];
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
  from_template?: string;
}

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
  state: string;
  created_at: string;
  terminal_at: string | null;
  result_digest: string | null;
}

/** `/api/events` 的 `task` 事件载荷：只是台账变更信号，行本身要重取。 */
export interface TaskEvent {
  ws_id: string;
  task_id: string;
  state: string;
  origin_kind: string;
  created_at: string;
  terminal_at: string | null;
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
  state: string;
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
  icon: string;
  background_color: string;
  status: string;
  tools: string[];
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
  icon: string;
  background_color: string;
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

/** `PATCH /api/agents/{id}` 的请求体；本期只用 `system_prompt`。 */
export interface UpdateAgentRequest {
  system_prompt?: string;
}

export interface SkillInfo {
  name: string;
  description: string;
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
