import api from './client'
import type {
  CreateTaskRequest,
  Task,
  TaskLog,
  TaskToggleResponse,
  UpdateTaskRequest,
} from '@/types/task'
import type { SuccessResponse } from '@/types/common'

export async function listTasks(): Promise<Task[]> {
  const res = await api.get<Task[]>('/tasks')
  return res.data
}

export async function createTask(data: CreateTaskRequest): Promise<Task> {
  const res = await api.post<Task>('/tasks', data)
  return res.data
}

export async function updateTask(id: string, data: UpdateTaskRequest): Promise<Task> {
  const res = await api.patch<Task>(`/tasks/${id}`, data)
  return res.data
}

export async function deleteTask(id: string): Promise<SuccessResponse> {
  const res = await api.delete<SuccessResponse>(`/tasks/${id}`)
  return res.data
}

export async function toggleTask(id: string): Promise<TaskToggleResponse> {
  const res = await api.post<TaskToggleResponse>(`/tasks/${id}/toggle`)
  return res.data
}

export async function getTaskLogs(
  taskId: string,
  offset = 0,
  limit = 50,
): Promise<TaskLog[]> {
  const res = await api.get<TaskLog[]>(`/tasks/${taskId}/logs`, {
    params: { offset, limit },
  })
  return res.data
}
