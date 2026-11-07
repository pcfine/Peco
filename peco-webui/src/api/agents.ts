import { api, spaceScope } from "@/lib/api";
import type { SuccessResponse } from "@/types/common";
import type {
  AgentDetail,
  AgentListItem,
  CreateAgentRequest,
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

export async function createAgent(
  wsId: string,
  body: CreateAgentRequest,
): Promise<AgentDetail> {
  const res = await api.post<AgentDetail>("/agents", body, spaceScope(wsId));
  return res.data;
}

/** 请求体里没出现的字段服务端原样保留，所以调用方只放真正要改的字段。 */
export async function updateAgent(
  wsId: string,
  agentId: string,
  body: UpdateAgentRequest,
): Promise<AgentDetail> {
  const res = await api.patch<AgentDetail>(
    `/agents/${encodeURIComponent(agentId)}`,
    body,
    spaceScope(wsId),
  );
  return res.data;
}

/** 删除被后端拒绝（`@master`）时抛 `ApiError`，调用方照原样展示。 */
export async function deleteAgent(
  wsId: string,
  agentId: string,
): Promise<SuccessResponse> {
  const res = await api.delete<SuccessResponse>(
    `/agents/${encodeURIComponent(agentId)}`,
    spaceScope(wsId),
  );
  return res.data;
}
