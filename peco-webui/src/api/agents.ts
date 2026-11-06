import { api, spaceScope } from "@/lib/api";
import type {
  AgentDetail,
  AgentListItem,
  UpdateAgentRequest,
} from "@/types/daemon";

/**
 * Agent 是团队级资源：空间只决定「读哪个团队的能力根」，所以每个请求都要带上
 * 一个该团队已打开空间的 `X-Peco-Space`。
 */
export async function listAgents(wsId: string): Promise<AgentListItem[]> {
  const res = await api.get<AgentListItem[]>("/agents", spaceScope(wsId));
  return res.data;
}

export async function getAgent(
  wsId: string,
  agentId: string,
): Promise<AgentDetail> {
  const res = await api.get<AgentDetail>(
    `/agents/${encodeURIComponent(agentId)}`,
    spaceScope(wsId),
  );
  return res.data;
}

/** 只改正文：本期团队页是「看 + 改 system_prompt」，其余字段没有编辑入口。 */
export async function updateAgentPrompt(
  wsId: string,
  agentId: string,
  systemPrompt: string,
): Promise<AgentDetail> {
  const body: UpdateAgentRequest = { system_prompt: systemPrompt };
  const res = await api.patch<AgentDetail>(
    `/agents/${encodeURIComponent(agentId)}`,
    body,
    spaceScope(wsId),
  );
  return res.data;
}
