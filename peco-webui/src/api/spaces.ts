import { api, spaceScope } from "@/lib/api";
import type { SuccessResponse } from "@/types/common";
import type {
  CreateSpaceRequest,
  LiveTaskView,
  MemoryDocumentDetail,
  MemoryDocumentList,
  Space,
  SpaceArtifactList,
  SpaceConversation,
  SpaceFilesResponse,
  SpaceOpened,
  SpaceOverview,
  Task,
} from "@/types/daemon";

export async function listSpaces(): Promise<Space[]> {
  const res = await api.get<Space[]>("/spaces", {
    // 已关闭的空间也留在列表里：关闭只表示实例不再打开，台账与历史仍指向它。
    params: { include_closed: true },
  });
  return res.data;
}

export async function getSpace(wsId: string): Promise<Space> {
  const res = await api.get<Space>(`/spaces/${encodeURIComponent(wsId)}`);
  return res.data;
}

/**
 * 取回该空间 `@master` 的唯一会话，没有才建。
 *
 * 幂等：打开集里重复点同一个空间不能建出第二条会话。`agent_name` 缺省即 `@master`，
 * 名字由后端/core 约定，前端不写死。
 */
export async function ensureSpaceConversation(
  wsId: string,
): Promise<SpaceConversation> {
  const res = await api.post<SpaceConversation>(
    `/spaces/${encodeURIComponent(wsId)}/conversation/ensure`,
    {},
    spaceScope(wsId),
  );
  return res.data;
}

/**
 * 清空该空间唯一会话的消息；会话行与已产出的成果都保留。
 *
 * 幂等：还没有会话的空间也回成功，调用方不必把「本来就干净」当成失败。
 * `agent_name` 缺省由后端补 `@master`，与 `ensure` 同一条约定。
 */
export async function clearSpaceConversation(
  wsId: string,
  agentName?: string,
): Promise<SuccessResponse> {
  const res = await api.post<SuccessResponse>(
    `/spaces/${encodeURIComponent(wsId)}/conversation/clear`,
    agentName ? { agent_name: agentName } : {},
    spaceScope(wsId),
  );
  return res.data;
}

export async function spaceOverview(wsId: string): Promise<SpaceOverview> {
  const res = await api.get<SpaceOverview>(
    `/spaces/${encodeURIComponent(wsId)}/overview`,
  );
  return res.data;
}

export async function spaceTasks(wsId: string): Promise<Task[]> {
  const res = await api.get<Task[]>(
    `/spaces/${encodeURIComponent(wsId)}/tasks`,
  );
  return res.data;
}

/**
 * 该空间 TaskHub 的实时快照；空间没打开是 409 `space_not_open`。
 *
 * 不把 409 翻译成空列表：空列表会让「Hub 不在」看起来像「开着但没任务」，
 * 在飞任务于是被显示成已完成。
 */
export async function spaceLiveTasks(wsId: string): Promise<LiveTaskView[]> {
  const res = await api.get<LiveTaskView[]>(
    `/spaces/${encodeURIComponent(wsId)}/tasks/live`,
  );
  return res.data;
}

/** 取消只转发给 Hub：已终态收到取消也是成功，真实终态靠回读 `/tasks/live` 决定。 */
export async function cancelSpaceTask(
  wsId: string,
  taskId: string,
): Promise<{ cancelled: boolean }> {
  const res = await api.post<{ cancelled: boolean }>(
    `/spaces/${encodeURIComponent(wsId)}/tasks/${encodeURIComponent(taskId)}/cancel`,
  );
  return res.data;
}

export async function spaceArtifacts(
  wsId: string,
  limit?: number,
): Promise<SpaceArtifactList> {
  const res = await api.get<SpaceArtifactList>(
    `/spaces/${encodeURIComponent(wsId)}/artifacts`,
    {
      params: limit ? { limit } : undefined,
    },
  );
  return res.data;
}

/** 记忆文档的检索条件；空串与 `null` 都表示「不加这一维过滤」。 */
export interface MemoryDocumentQuery {
  query?: string;
  category?: string;
  limit?: number;
  offset?: number;
}

/**
 * 当前空间的记忆文档清单。
 *
 * 409 `space_not_open` 不翻译成空列表：空列表会把「空间没实例」显示成
 * 「这个空间没有记忆」，而这两件事的下一步完全不同。
 */
export async function spaceMemoryDocuments(
  wsId: string,
  options: MemoryDocumentQuery = {},
): Promise<MemoryDocumentList> {
  const { query, category, limit, offset } = options;
  const res = await api.get<MemoryDocumentList>(
    `/spaces/${encodeURIComponent(wsId)}/memory/documents`,
    { params: { query, category, limit, offset } },
  );
  return res.data;
}

/** 单篇记忆的全文；跨空间访问与不存在同为 404，本空间之外的一律当不存在。 */
export async function spaceMemoryDocument(
  wsId: string,
  docId: string,
): Promise<MemoryDocumentDetail> {
  const res = await api.get<MemoryDocumentDetail>(
    `/spaces/${encodeURIComponent(wsId)}/memory/documents/${encodeURIComponent(docId)}`,
  );
  return res.data;
}

export async function createSpace(
  body: CreateSpaceRequest,
): Promise<SpaceOpened> {
  const res = await api.post<SpaceOpened>("/spaces", body);
  return res.data;
}

/**
 * 空间工作目录的只读浏览：`path` 缺省列根目录，指向文件则回正文。
 *
 * 越界或不存在统一是 400 `invalid_path`：前端只有「换一个路径」这一种反应，
 * 不需要区分是哪一种不合法。
 */
export async function spaceFiles(
  wsId: string,
  options: { path?: string; maxBytes?: number } = {},
): Promise<SpaceFilesResponse> {
  const { path, maxBytes } = options;
  const res = await api.get<SpaceFilesResponse>(
    `/spaces/${encodeURIComponent(wsId)}/files`,
    { params: { path, max_bytes: maxBytes } },
  );
  return res.data;
}

/** 关闭空间；`force` 越过在飞门槛（409 `inflight_tasks` 之后的第二条路）。 */
export async function closeSpace(
  wsId: string,
  force = false,
): Promise<{ stopped: boolean }> {
  const res = await api.post<{ stopped: boolean }>(
    `/spaces/${encodeURIComponent(wsId)}/close`,
    {
      force,
    },
  );
  return res.data;
}

export async function deleteSpace(
  wsId: string,
  force = false,
): Promise<{ purged: boolean }> {
  const res = await api.delete<{ purged: boolean }>(
    `/spaces/${encodeURIComponent(wsId)}`,
    {
      params: force ? { force } : undefined,
    },
  );
  return res.data;
}
