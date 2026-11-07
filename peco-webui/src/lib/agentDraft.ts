// 装配页的表单状态与提交体：哪些字段算「改过」是能定死的规则，就不放进组件里。
//
// 提交体只带变化字段 —— `PATCH /api/agents/{id}` 按「省略即保留」合并，
// 一路全量提交会把别处刚改好的字段按旧快照写回去。

import type {
  AgentDetail,
  CreateAgentRequest,
  UpdateAgentRequest,
} from "@/types/daemon";

/**
 * 编辑器草稿。数字字段留成原始输入串：空串是「不指定」，
 * 与「改成 0」是两件事，混成 `number | null` 就分不开。
 */
export interface AgentDraft {
  name: string;
  description: string;
  icon: string;
  model: string | null;
  provider: string | null;
  system_prompt: string;
  tools: string[];
  skills: string[];
  mcp_servers: string[];
  knowledge_bases: string[];
  temperature: string;
  max_tokens: string;
  max_iterations: string;
}

/** 新建对话框的表单体：只有这几项能一次写进 agent.md，其余留空走服务端默认。 */
export interface NewAgentDraft {
  name: string;
  description: string;
  icon: string;
  provider: string | null;
  model: string | null;
  system_prompt: string;
}

export const EMPTY_NEW_AGENT: NewAgentDraft = {
  name: "",
  description: "",
  icon: "",
  provider: null,
  model: null,
  system_prompt: "",
};

function textOf(value: number | null): string {
  return value === null ? "" : String(value);
}

export function draftFromDetail(detail: AgentDetail): AgentDraft {
  return {
    name: detail.name,
    description: detail.description,
    icon: detail.icon,
    model: detail.model,
    provider: detail.provider,
    system_prompt: detail.system_prompt,
    tools: detail.tools,
    skills: detail.skills,
    mcp_servers: detail.mcp_servers,
    knowledge_bases: detail.knowledge_bases,
    temperature: textOf(detail.temperature),
    max_tokens: textOf(detail.max_tokens),
    max_iterations: textOf(detail.max_iterations),
  };
}

/** 勾选顺序不是改动，按集合比。 */
function sameMembers(a: string[], b: string[]): boolean {
  if (a.length !== b.length) return false;
  const known = new Set(b);
  return a.every((item) => known.has(item));
}

/** 空串 → `null`（不指定）；填了不是数字 → 记一条原因并返回 `undefined`（该字段不进提交体）。 */
function numberField(
  raw: string,
  label: string,
  integer: boolean,
  invalid: string[],
): number | null | undefined {
  const text = raw.trim();
  if (text === "") return null;
  const value = Number(text);
  if (
    !Number.isFinite(value) ||
    (integer && (!Number.isInteger(value) || value <= 0))
  ) {
    invalid.push(`${label} ${integer ? "要填正整数" : "要填数字"}`);
    return undefined;
  }
  return value;
}

export function toggleValue(list: string[], value: string): string[] {
  return list.includes(value)
    ? list.filter((item) => item !== value)
    : [...list, value];
}

export interface DraftDiff {
  /** 只含真正变化的字段；`invalid` 非空时不可提交。 */
  body: UpdateAgentRequest;
  invalid: string[];
}

export function diffDraft(detail: AgentDetail, draft: AgentDraft): DraftDiff {
  const body: UpdateAgentRequest = {};
  const invalid: string[] = [];

  const name = draft.name.trim();
  if (name === "") invalid.push("名称不能为空");
  else if (name !== detail.name) body.name = name;

  if (draft.description !== detail.description)
    body.description = draft.description;
  if (draft.icon !== detail.icon) body.icon = draft.icon;
  if (draft.model !== detail.model) body.model = draft.model;
  if (draft.provider !== detail.provider) body.provider = draft.provider;
  if (draft.system_prompt !== detail.system_prompt)
    body.system_prompt = draft.system_prompt;

  if (!sameMembers(draft.tools, detail.tools)) body.tools = draft.tools;
  if (!sameMembers(draft.skills, detail.skills)) body.skills = draft.skills;
  if (!sameMembers(draft.mcp_servers, detail.mcp_servers))
    body.mcp_servers = draft.mcp_servers;
  if (!sameMembers(draft.knowledge_bases, detail.knowledge_bases))
    body.knowledge_bases = draft.knowledge_bases;

  const temperature = numberField(
    draft.temperature,
    "temperature",
    false,
    invalid,
  );
  if (temperature !== undefined && temperature !== detail.temperature)
    body.temperature = temperature;

  const maxTokens = numberField(draft.max_tokens, "max_tokens", true, invalid);
  if (maxTokens !== undefined && maxTokens !== detail.max_tokens)
    body.max_tokens = maxTokens;

  // max_iterations 服务端没有「清空」形态，留空只能算不改。
  const maxIterations = numberField(
    draft.max_iterations,
    "max_iterations",
    true,
    invalid,
  );
  if (
    maxIterations !== undefined &&
    maxIterations !== null &&
    maxIterations !== detail.max_iterations
  )
    body.max_iterations = maxIterations;

  return { body, invalid };
}

/** 新建请求体：没填的项不发，让服务端按默认值落 agent.md。 */
export function createRequestFromDraft(
  draft: NewAgentDraft,
): CreateAgentRequest {
  const body: CreateAgentRequest = { name: draft.name.trim() };
  if (draft.description.trim()) body.description = draft.description.trim();
  if (draft.icon.trim()) body.icon = draft.icon.trim();
  if (draft.provider) body.provider = draft.provider;
  if (draft.model) body.model = draft.model;
  if (draft.system_prompt) body.system_prompt = draft.system_prompt;
  return body;
}
