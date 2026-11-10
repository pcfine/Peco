import { api } from "@/lib/api";
import type {
  CreateTeamRequest,
  Team,
  TeamDetail,
  UpdateTeamRequest,
} from "@/types/daemon";

export interface ListTeamsOptions {
  /**
   * 把保留团队 Peco 一起取回来。
   *
   * 它不在默认清单里（团队列表页不该出现「去打开 Peco」这种入口），但 Peco 是
   * provider 继承链的默认层，设置页必须能选中它。
   */
  includePeco?: boolean;
}

export async function listTeams(
  options: ListTeamsOptions = {},
): Promise<Team[]> {
  const res = await api.get<Team[]>(
    "/teams",
    options.includePeco ? { params: { include: "peco" } } : undefined,
  );
  return res.data;
}

export async function getTeam(teamId: string): Promise<TeamDetail> {
  const res = await api.get<TeamDetail>(`/teams/${encodeURIComponent(teamId)}`);
  return res.data;
}

export async function createTeam(body: CreateTeamRequest): Promise<Team> {
  const res = await api.post<Team>("/teams", body);
  return res.data;
}

export async function updateTeam(
  teamId: string,
  body: UpdateTeamRequest,
): Promise<Team> {
  const res = await api.patch<Team>(
    `/teams/${encodeURIComponent(teamId)}`,
    body,
  );
  return res.data;
}

/** 删除团队；服务端会一并移除绑定空间的行（工作目录保留）。 */
export async function deleteTeam(teamId: string): Promise<void> {
  await api.delete(`/teams/${encodeURIComponent(teamId)}`);
}
