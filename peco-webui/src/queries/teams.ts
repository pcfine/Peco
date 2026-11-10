import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { createTeam, deleteTeam, listTeams, updateTeam } from "@/api/teams";
import { spacesKey } from "@/queries/spaces";
import type { CreateTeamRequest } from "@/types/daemon";

export const teamsKey = ["teams"] as const;

/**
 * 含保留团队 Peco 的独立 key。
 *
 * 不复用 `teamsKey`：那份清单是团队列表页与各处团队选择器的来源，多出一个 Peco
 * 会改变它们的语义。`teamsKey` 是它的前缀，团队增删照样能把它一起失效掉。
 */
export const teamsWithPecoKey = ["teams", "include=peco"] as const;

/** 团队清单。 */
export function useTeams() {
  return useQuery({
    queryKey: teamsKey,
    queryFn: () => listTeams(),
    staleTime: 60_000,
  });
}

/** 团队清单 + 保留团队 Peco：只有要选「哪一层配置」的地方需要它。 */
export function useTeamsWithPeco() {
  return useQuery({
    queryKey: teamsWithPecoKey,
    queryFn: () => listTeams({ includePeco: true }),
    staleTime: 60_000,
  });
}

/** 新建团队。 */
export function useCreateTeam() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: CreateTeamRequest) => createTeam(body),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: teamsKey });
    },
  });
}

/** 删除团队；同时失效空间清单与团队详情缓存。 */
export function useDeleteTeam() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (teamId: string) => deleteTeam(teamId),
    onSuccess: (_data, teamId) => {
      queryClient.removeQueries({ queryKey: ["team", teamId] });
      void queryClient.invalidateQueries({ queryKey: teamsKey });
      void queryClient.invalidateQueries({ queryKey: spacesKey });
      toast.success("团队已删除");
    },
  });
}

/** 更新团队图标。 */
export function useUpdateTeamIcon() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ teamId, icon }: { teamId: string; icon: string }) =>
      updateTeam(teamId, { icon }),
    onSuccess: (_view, { teamId }) => {
      void queryClient.invalidateQueries({ queryKey: teamsKey });
      void queryClient.invalidateQueries({ queryKey: ["team", teamId] });
      toast.success("团队图标已更新");
    },
  });
}
