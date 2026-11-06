// 团队页的资源查询：agents / skills / MCP / 知识库都要求一个「该团队已打开的空间」
// 作为 `X-Peco-Space`。没有已打开空间时调用方禁用入口，不在这里静默降级成空列表。

import { useQuery } from "@tanstack/react-query";
import { getTeam } from "@/api/teams";
import { getAgent, listAgents } from "@/api/agents";
import { getSkillMd, listSkills } from "@/api/skills";
import { getMcpConfig } from "@/api/mcp";
import { listKnowledgeBases } from "@/api/knowledge";

export function useTeam(teamId: string) {
  return useQuery({
    queryKey: ["team", teamId],
    queryFn: () => getTeam(teamId),
    staleTime: 30_000,
  });
}

export function useTeamAgents(wsId: string | null) {
  return useQuery({
    queryKey: ["agents", wsId],
    queryFn: () => listAgents(wsId as string),
    enabled: Boolean(wsId),
  });
}

export function useAgentDetail(wsId: string | null, agentId: string | null) {
  return useQuery({
    queryKey: ["agent", wsId, agentId],
    queryFn: () => getAgent(wsId as string, agentId as string),
    enabled: Boolean(wsId && agentId),
  });
}

export function useTeamSkills(wsId: string | null) {
  return useQuery({
    queryKey: ["skills", wsId],
    queryFn: () => listSkills(wsId as string),
    enabled: Boolean(wsId),
  });
}

export function useSkillMd(wsId: string | null, name: string | null) {
  return useQuery({
    queryKey: ["skill", wsId, name],
    queryFn: () => getSkillMd(wsId as string, name as string),
    enabled: Boolean(wsId && name),
  });
}

export function useMcpConfig(wsId: string | null) {
  return useQuery({
    queryKey: ["mcp", wsId],
    queryFn: () => getMcpConfig(wsId as string),
    enabled: Boolean(wsId),
  });
}

export function useKnowledgeBases(wsId: string | null) {
  return useQuery({
    queryKey: ["knowledge", wsId],
    queryFn: () => listKnowledgeBases(wsId as string),
    enabled: Boolean(wsId),
  });
}
