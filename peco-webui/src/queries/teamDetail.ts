// 团队页的资源查询：agents / skills / MCP / 知识库都按团队寻址 —— 该团队有已打开
// 空间时借道它的空间头，没有就直接发团队头。空团队由各 tab 自己渲染空态。

import { useQuery } from "@tanstack/react-query";
import { getTeam } from "@/api/teams";
import { getAgent, listAgents } from "@/api/agents";
import { listSkills } from "@/api/skills";
import { getMcpConfig } from "@/api/mcp";
import { listKnowledgeBases } from "@/api/knowledge";
import type { ResourceScope } from "@/lib/api";

export function useTeam(teamId: string) {
  return useQuery({
    queryKey: ["team", teamId],
    queryFn: () => getTeam(teamId),
    staleTime: 30_000,
  });
}

export function useTeamAgents(scope: ResourceScope) {
  return useQuery({
    queryKey: ["agents", scope.teamId, scope.wsId],
    queryFn: () => listAgents(scope),
    enabled: Boolean(scope.teamId),
  });
}

export function useAgentDetail(scope: ResourceScope, agentId: string | null) {
  return useQuery({
    queryKey: ["agent", scope.teamId, scope.wsId, agentId],
    queryFn: () => getAgent(scope, agentId as string),
    enabled: Boolean(scope.teamId && agentId),
  });
}

export function useTeamSkills(scope: ResourceScope) {
  return useQuery({
    queryKey: ["skills", scope.teamId, scope.wsId],
    queryFn: () => listSkills(scope),
    enabled: Boolean(scope.teamId),
  });
}

export function useMcpConfig(scope: ResourceScope) {
  return useQuery({
    queryKey: ["mcp", scope.teamId, scope.wsId],
    queryFn: () => getMcpConfig(scope),
    enabled: Boolean(scope.teamId),
  });
}

export function useKnowledgeBases(scope: ResourceScope) {
  return useQuery({
    queryKey: ["knowledge", scope.teamId, scope.wsId],
    queryFn: () => listKnowledgeBases(scope),
    enabled: Boolean(scope.teamId),
  });
}
