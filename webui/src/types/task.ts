// Task types — aligned with peco-server /api/tasks/*

export interface Task {
  id: string
  name: string
  agent_id: string
  agent_name?: string
  cron_expr: string
  prompt: string
  enabled: boolean
  last_run_at?: string
  next_run_at?: string
  created_at: string
  updated_at: string
}

export interface TaskLog {
  id: string
  task_id: string
  status: 'running' | 'success' | 'error'
  output?: string
  error?: string
  started_at: string
  finished_at?: string
}

export interface CreateTaskRequest {
  agent_id: string
  name: string
  cron_expr: string
  prompt: string
  enabled?: boolean
}

export interface UpdateTaskRequest {
  name?: string
  agent_id?: string
  cron_expr?: string
  prompt?: string
}

export interface TaskToggleResponse {
  id: string
  enabled: boolean
}
