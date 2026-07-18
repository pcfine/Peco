import type { User, Agent, Conversation, KnowledgeBase, Task } from '@/types'

const BASE_URL = '/api'

async function request<T>(url: string, options?: RequestInit): Promise<T> {
  const token = localStorage.getItem('token')
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...(token ? { Authorization: `Bearer ${token}` } : {}),
    ...(options?.headers as Record<string, string> || {}),
  }

  const res = await fetch(`${BASE_URL}${url}`, {
    ...options,
    headers,
  })

  if (!res.ok) {
    const err = await res.json().catch(() => ({ message: res.statusText }))
    throw new Error(err.message || `HTTP ${res.status}`)
  }

  return res.json()
}

// Auth
export const auth = {
  register: (data: { username: string; email: string; password: string }) =>
    request<{ user: User; token: string }>('/auth/register', { method: 'POST', body: JSON.stringify(data) }),
  login: (data: { email: string; password: string }) =>
    request<{ user: User; token: string }>('/auth/login', { method: 'POST', body: JSON.stringify(data) }),
  me: () => request<User>('/auth/me'),
}

// Chat
export const chat = {
  send: (conversationId: string | null, message: string) =>
    request<{ message: any; conversationId: string }>('/chat/send', {
      method: 'POST',
      body: JSON.stringify({ conversationId, message }),
    }),
  stream: (conversationId: string | null, message: string) => {
    const token = localStorage.getItem('token')
    const url = `${BASE_URL}/chat/stream?conversationId=${conversationId || ''}&message=${encodeURIComponent(message)}`
    return new EventSource(url, {
      headers: token ? { Authorization: `Bearer ${token}` } : {},
    } as EventSourceInit)
  },
  conversations: () => request<Conversation[]>('/chat/conversations'),
}

// Agents
export const agents = {
  list: () => request<Agent[]>('/agents'),
  create: (data: Partial<Agent>) =>
    request<Agent>('/agents', { method: 'POST', body: JSON.stringify(data) }),
  update: (id: string, data: Partial<Agent>) =>
    request<Agent>(`/agents/${id}`, { method: 'PATCH', body: JSON.stringify(data) }),
  delete: (id: string) =>
    request<void>(`/agents/${id}`, { method: 'DELETE' }),
}

// Knowledge Base
export const knowledge = {
  list: () => request<KnowledgeBase[]>('/knowledge'),
  create: (data: { name: string; description: string }) =>
    request<KnowledgeBase>('/knowledge', { method: 'POST', body: JSON.stringify(data) }),
  delete: (id: string) =>
    request<void>(`/knowledge/${id}`, { method: 'DELETE' }),
  upload: (knowledgeId: string, file: File) => {
    const formData = new FormData()
    formData.append('file', file)
    const token = localStorage.getItem('token')
    return fetch(`${BASE_URL}/knowledge/${knowledgeId}/upload`, {
      method: 'POST',
      headers: token ? { Authorization: `Bearer ${token}` } : {},
      body: formData,
    }).then(r => r.json())
  },
}

// Tasks
export const tasks = {
  list: () => request<Task[]>('/tasks'),
  create: (data: Partial<Task>) =>
    request<Task>('/tasks', { method: 'POST', body: JSON.stringify(data) }),
  update: (id: string, data: Partial<Task>) =>
    request<Task>(`/tasks/${id}`, { method: 'PATCH', body: JSON.stringify(data) }),
  delete: (id: string) =>
    request<void>(`/tasks/${id}`, { method: 'DELETE' }),
  toggle: (id: string) =>
    request<Task>(`/tasks/${id}/toggle`, { method: 'POST' }),
}
