import api from './client'
import type { Conversation, Message, SessionSnapshotResponse } from '@/types/chat'
import type { SuccessResponse } from '@/types/common'

export async function listConversations(agentId?: string): Promise<Conversation[]> {
  const params = agentId ? { agent_id: agentId } : {}
  const res = await api.get<Conversation[]>('/conversations', { params })
  return res.data
}

export async function createConversation(title?: string, agentId?: string): Promise<Conversation> {
  const res = await api.post<Conversation>('/conversations', {
    title,
    agent_id: agentId,
  })
  return res.data
}

export async function deleteConversation(id: string): Promise<SuccessResponse> {
  const res = await api.delete<SuccessResponse>(`/conversations/${id}`)
  return res.data
}

export async function getMessages(
  convId: string,
  offset = 0,
  limit = 50,
): Promise<Message[]> {
  const res = await api.get<Message[]>(`/conversations/${convId}/messages`, {
    params: { offset, limit },
  })
  return res.data
}

export async function getSessionSnapshot(convId: string): Promise<SessionSnapshotResponse> {
  const res = await api.get<SessionSnapshotResponse>(`/conversations/${convId}/session`)
  return res.data
}
