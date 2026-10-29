import api from "./client";
import type {
  AgentDetail,
  AgentListItem,
  CreateAgentRequest,
  UpdateAgentRequest,
} from "@/types/agent";
import type { SuccessResponse } from "@/types/common";

// ── 总管 Agent 图片输入能力（进程内 memo）────────────────────────────────
//
// 该能力只取决于 agent 绑定的 provider，属静态值；每次进 /peco 都 listAgents +
// getAgent 会随路由往返重复打两个请求（开发态 StrictMode 双跑还会翻倍）。
// 探测结果进程内缓存一次，改 agent 配置（updateAgent）时失效重取。
let assistantSupportsImages: boolean | null = null;
let assistantSupportsImagesInFlight: Promise<boolean> | null = null;

export async function listAgents(
  signal?: AbortSignal,
): Promise<AgentListItem[]> {
  const res = await api.get<AgentListItem[]>("/agents", { signal });
  return res.data;
}

export async function createAgent(
  data: CreateAgentRequest,
): Promise<AgentDetail> {
  const res = await api.post<AgentDetail>("/agents", data);
  return res.data;
}

export async function getAgent(id: string): Promise<AgentDetail> {
  const res = await api.get<AgentDetail>(`/agents/${id}`);
  return res.data;
}

export async function updateAgent(
  id: string,
  data: UpdateAgentRequest,
): Promise<AgentDetail> {
  const res = await api.patch<AgentDetail>(`/agents/${id}`, data);
  // provider 可能随本次修改变化 → 图片能力缓存失效，下次进对话页重取。
  assistantSupportsImages = null;
  return res.data;
}

export async function deleteAgent(id: string): Promise<SuccessResponse> {
  const res = await api.delete<SuccessResponse>(`/agents/${id}`);
  return res.data;
}

/** 总管 Agent 是否支持用户图片输入。结果 memo，重复调用至多一次网络往返。 */
export async function getAssistantSupportsImages(): Promise<boolean> {
  if (assistantSupportsImages !== null) return assistantSupportsImages;
  if (!assistantSupportsImagesInFlight) {
    assistantSupportsImagesInFlight = (async () => {
      try {
        const all = await listAgents();
        const match = all.find((a) => a.is_master);
        if (!match) return false;
        const detail = await getAgent(match.id);
        assistantSupportsImages = detail.supports_images;
        return assistantSupportsImages;
      } finally {
        assistantSupportsImagesInFlight = null;
      }
    })();
  }
  return assistantSupportsImagesInFlight;
}

/** 仅供测试：清除能力探测缓存。 */
export function __resetAgentCapabilitiesForTests(): void {
  assistantSupportsImages = null;
  assistantSupportsImagesInFlight = null;
}
