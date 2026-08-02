import api from "./client";
import type {
  Conversation,
  Message,
  SessionSnapshotResponse,
} from "@/types/chat";
import type { SuccessResponse } from "@/types/common";

// ── New v2 endpoints (Agent-scoped) ──────────────────────────────────────

// axios baseURL is '/api', so paths here are relative to that.
function chatBase(agentId: string) {
  return `/chat/${encodeURIComponent(agentId)}/conversations`;
}

export async function listConversations(
  agentId: string,
  status?: string,
): Promise<Conversation[]> {
  const params = status ? { status } : {};
  const res = await api.get<Conversation[]>(chatBase(agentId), { params });
  return res.data;
}

export async function createConversation(
  agentId: string,
  title?: string,
): Promise<Conversation> {
  const res = await api.post<Conversation>(chatBase(agentId), { title });
  return res.data;
}

export interface UpdateConversationBody {
  title?: string;
  archive?: boolean;
  unarchive?: boolean;
}

export async function updateConversation(
  agentId: string,
  convId: string,
  data: UpdateConversationBody,
): Promise<Conversation> {
  const res = await api.patch<Conversation>(
    `${chatBase(agentId)}/${convId}`,
    data,
  );
  return res.data;
}

export async function deleteConversation(
  agentId: string,
  convId: string,
): Promise<SuccessResponse> {
  const res = await api.delete<SuccessResponse>(
    `${chatBase(agentId)}/${convId}`,
  );
  return res.data;
}

export async function getMessages(
  agentId: string,
  convId: string,
  offset = 0,
  limit = 50,
): Promise<Message[]> {
  const res = await api.get<Message[]>(
    `${chatBase(agentId)}/${convId}/messages`,
    {
      params: { offset, limit },
    },
  );
  return res.data;
}

export async function getSessionSnapshot(
  agentId: string,
  convId: string,
): Promise<SessionSnapshotResponse> {
  const res = await api.get<SessionSnapshotResponse>(
    `${chatBase(agentId)}/${convId}/session`,
  );
  return res.data;
}
