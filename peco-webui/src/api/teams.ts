import { api } from "@/lib/api";
import type {
  CreateTeamRequest,
  Team,
  TeamDetail,
  UpdateTeamRequest,
} from "@/types/daemon";

export async function listTeams(): Promise<Team[]> {
  const res = await api.get<Team[]>("/teams");
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
