import { api } from "@/lib/api";
import type {
  CreateSpaceRequest,
  Space,
  SpaceArtifactList,
  SpaceFilesResponse,
  SpaceOpened,
  SpaceOverview,
  Task,
} from "@/types/daemon";

export async function listSpaces(): Promise<Space[]> {
  const res = await api.get<Space[]>("/spaces", {
    // 已关闭的空间也留在列表里：删团队/删空间没有入口，隐藏等于丢历史。
    params: { include_closed: true },
  });
  return res.data;
}

export async function getSpace(wsId: string): Promise<Space> {
  const res = await api.get<Space>(`/spaces/${encodeURIComponent(wsId)}`);
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
