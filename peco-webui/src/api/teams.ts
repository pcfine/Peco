import { api } from "@/lib/api";
import type { CreateTeamRequest, Team, TeamDetail } from "@/types/daemon";

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
