import { api, resourceScope, type ResourceScope } from "@/lib/api";
import type { SuccessResponse } from "@/types/common";
import type {
  AgentDetail,
  AgentListItem,
  CreateAgentRequest,
  UpdateAgentRequest,
} from "@/types/daemon";

/**
 * Agent 是团队级资源：请求只需认到团队 —— 该团队有已打开空间时借道它的
 * 空间头，没有就直接按团队寻址。
 */
export async function listAgents(
  scope: ResourceScope,
): Promise<AgentListItem[]> {
  const res = await api.get<AgentListItem[]>("/agents", resourceScope(scope));
  return res.data;
}

export async function getAgent(
  scope: ResourceScope,
  agentId: string,
): Promise<AgentDetail> {
  const res = await api.get<AgentDetail>(
    `/agents/${encodeURIComponent(agentId)}`,
    resourceScope(scope),
  );
  return res.data;
}

export async function createAgent(
  scope: ResourceScope,
  body: CreateAgentRequest,
): Promise<AgentDetail> {
  const res = await api.post<AgentDetail>(
    "/agents",
    body,
    resourceScope(scope),
  );
  return res.data;
}

/** 请求体里没出现的字段服务端原样保留，所以调用方只放真正要改的字段。 */
export async function updateAgent(
  scope: ResourceScope,
  agentId: string,
  body: UpdateAgentRequest,
): Promise<AgentDetail> {
  const res = await api.patch<AgentDetail>(
    `/agents/${encodeURIComponent(agentId)}`,
    body,
    resourceScope(scope),
  );
  return res.data;
}

/** 删除被后端拒绝（`@master`）时抛 `ApiError`，调用方照原样展示。 */
export async function deleteAgent(
  scope: ResourceScope,
  agentId: string,
): Promise<SuccessResponse> {
  const res = await api.delete<SuccessResponse>(
    `/agents/${encodeURIComponent(agentId)}`,
    resourceScope(scope),
  );
  return res.data;
}
