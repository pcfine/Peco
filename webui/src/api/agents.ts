import api from './client'
import type { AgentDetail, AgentListItem, CreateAgentRequest, UpdateAgentRequest } from '@/types/agent'
import type { SuccessResponse } from '@/types/common'

export async function listAgents(): Promise<AgentListItem[]> {
  const res = await api.get<AgentListItem[]>('/agents')
  return res.data
}

export async function createAgent(data: CreateAgentRequest): Promise<AgentDetail> {
  const res = await api.post<AgentDetail>('/agents', data)
  return res.data
}

export async function getAgent(id: string): Promise<AgentDetail> {
  const res = await api.get<AgentDetail>(`/agents/${id}`)
  return res.data
}

export async function updateAgent(id: string, data: UpdateAgentRequest): Promise<AgentDetail> {
  const res = await api.patch<AgentDetail>(`/agents/${id}`, data)
  return res.data
}

export async function deleteAgent(id: string): Promise<SuccessResponse> {
  const res = await api.delete<SuccessResponse>(`/agents/${id}`)
  return res.data
}
