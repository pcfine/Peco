export interface User {
  id: string;
  username: string;
  email: string;
  avatar?: string;
  createdAt: string;
}

export interface Message {
  id: string;
  role: "user" | "assistant" | "agent";
  content: string;
  agentId?: string;
  agentName?: string;
  timestamp: string;
}

export interface Agent {
  id: string;
  name: string;
  description: string;
  systemPrompt: string;
  model: string;
  icon: string;
  color: string;
  status: "idle" | "running" | "error";
  createdAt: string;
}

export interface Conversation {
  id: string;
  title: string;
  messages: Message[];
  agentId?: string;
  createdAt: string;
  updatedAt: string;
}

export interface KnowledgeBase {
  id: string;
  name: string;
  description: string;
  documentCount: number;
  createdAt: string;
}

export interface Task {
  id: string;
  name: string;
  agentId: string;
  cron: string;
  prompt: string;
  enabled: boolean;
  lastRun?: string;
  nextRun?: string;
  createdAt: string;
}
