// Agent types — aligned with peco-server /api/agents/*
export interface AgentListItem {
  id: string
  name: string
  description: string
  model?: string
  provider?: string
  icon: string
  color: string
  status: string
  tools?: string[]
  knowledge_bases?: string[]
  created_at: string
}

export interface AgentDetail extends AgentListItem {
  system_prompt: string
  mcp_servers: string[]
  skills: string[]
  temperature?: number
  max_tokens?: number
  updated_at: string
}

export interface CreateAgentRequest {
  name: string
  description?: string
  system_prompt?: string
  model?: string
  provider?: string
  icon?: string
  color?: string
  tools?: string[]
  mcp_servers?: string[]
  skills?: string[]
  temperature?: number
  max_tokens?: number
}

export interface UpdateAgentRequest {
  name?: string
  description?: string
  system_prompt?: string
  model?: string
  provider?: string
  icon?: string
  color?: string
  tools?: string[]
  mcp_servers?: string[]
  skills?: string[]
  temperature?: number
  max_tokens?: number
}

export interface SuccessResponse {
  success: boolean
}
