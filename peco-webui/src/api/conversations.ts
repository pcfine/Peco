import { api, spaceScope } from "@/lib/api";
import type {
  Conversation,
  Message,
  SessionSnapshotResponse,
} from "@/types/chat";
import type { SuccessResponse } from "@/types/common";
import type { ArtifactItem } from "@/types/artifact";

// ── New v2 endpoints (Agent-scoped) ──────────────────────────────────────

// axios baseURL is '/api', so paths here are relative to that.
function chatBase(agentId: string) {
  return `/chat/${encodeURIComponent(agentId)}/conversations`;
}

export async function listConversations(
  agentId: string,
  options: { status?: string; signal?: AbortSignal; wsId?: string } = {},
): Promise<Conversation[]> {
  const { status, signal, wsId } = options;
  const res = await api.get<Conversation[]>(chatBase(agentId), {
    params: status ? { status } : {},
    signal,
    ...spaceScope(wsId),
  });
  return res.data;
}

export async function createConversation(
  agentId: string,
  title?: string,
  wsId?: string,
): Promise<Conversation> {
  const res = await api.post<Conversation>(
    chatBase(agentId),
    { title },
    spaceScope(wsId),
  );
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
  wsId?: string,
): Promise<Conversation> {
  const res = await api.patch<Conversation>(
    `${chatBase(agentId)}/${convId}`,
    data,
    spaceScope(wsId),
  );
  return res.data;
}

export async function deleteConversation(
  agentId: string,
  convId: string,
  wsId?: string,
): Promise<SuccessResponse> {
  const res = await api.delete<SuccessResponse>(
    `${chatBase(agentId)}/${convId}`,
    spaceScope(wsId),
  );
  return res.data;
}

export async function getMessages(
  agentId: string,
  convId: string,
  options: { offset?: number; limit?: number; wsId?: string } = {},
): Promise<Message[]> {
  const { offset = 0, limit = 50, wsId } = options;
  const res = await api.get<Message[]>(
    `${chatBase(agentId)}/${convId}/messages`,
    {
      params: { offset, limit },
      ...spaceScope(wsId),
    },
  );
  return res.data;
}

/**
 * 请求暂停该对话在途的 ReAct 轮。
 *
 * 暂停在边界检查点落地：请求发出前，或工具批执行前。请求在途时须等本次模型调用
 * 收尾；工具批执行中不打断，批跑完才停；无工具调用的轮次暂停被吸收。故 200 只
 * 表示「请求已收到」；实际状态由 SSE `looper_state` 事件下发。
 */
export async function pauseConversation(
  agentId: string,
  convId: string,
): Promise<SuccessResponse> {
  const res = await api.post<SuccessResponse>(
    `${chatBase(agentId)}/${convId}/pause`,
  );
  return res.data;
}

/** 请求恢复该对话被暂停的 ReAct 轮（幂等）。 */
export async function resumeConversation(
  agentId: string,
  convId: string,
): Promise<SuccessResponse> {
  const res = await api.post<SuccessResponse>(
    `${chatBase(agentId)}/${convId}/resume`,
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

export interface UploadedImage {
  /** 图片引用 id，发送消息时以 image_ids 传给 stream 端点 */
  id: string;
  /** 图片访问 URL，直接用于前端渲染 */
  url: string;
}

/** 上传会话图片（multipart），返回引用 id 与渲染 URL。 */
export async function uploadConversationImage(
  agentId: string,
  convId: string,
  file: File,
  options: { onProgress?: (percent: number) => void; wsId?: string } = {},
): Promise<UploadedImage> {
  const { onProgress, wsId } = options;
  const form = new FormData();
  form.append("file", file);
  const scope = spaceScope(wsId);
  const res = await api.post<UploadedImage>(
    `${chatBase(agentId)}/${convId}/images`,
    form,
    {
      ...scope,
      headers: {
        "Content-Type": "multipart/form-data",
        ...scope?.headers,
      },
      ...(onProgress && {
        onUploadProgress: (e) => {
          if (e.total) onProgress(Math.round((e.loaded * 100) / e.total));
        },
      }),
    },
  );
  return res.data;
}

/** 该对话的产物清单；DTO 与 `/api/artifacts/{id}/content` 属同一套。 */
export async function listConversationArtifacts(
  agentId: string,
  convId: string,
  wsId?: string,
): Promise<ArtifactItem[]> {
  const res = await api.get<ArtifactItem[]>(
    `${chatBase(agentId)}/${convId}/artifacts`,
    spaceScope(wsId),
  );
  return res.data;
}

/**
 * 一轮对话的流式地址：服务端「GET 即开一轮」，正文与附件经查询参数传入。
 *
 * 与 `fetch` 直连（ChatView internal 模式）而不是走 axios：SSE 要逐块读 body，
 * axios 会先把整包缓冲完。
 */
export function conversationStreamUrl(
  agentId: string,
  convId: string,
  message: string,
  options: { imageIds?: string[]; wsId?: string } = {},
): string {
  const { imageIds = [], wsId } = options;
  const images = imageIds.length > 0 ? `&image_ids=${imageIds.join(",")}` : "";
  // fetch 不认 axios 的 baseURL，也不带拦截器加的请求头：`/api` 与 `?ws=` 都在这里补。
  const ws = wsId ? `&ws=${encodeURIComponent(wsId)}` : "";
  return `/api${chatBase(agentId)}/${encodeURIComponent(convId)}/stream?message=${encodeURIComponent(message)}${images}${ws}`;
}
