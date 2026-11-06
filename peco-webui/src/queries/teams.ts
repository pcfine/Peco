import { useQuery } from "@tanstack/react-query";
import { listTeams } from "@/api/teams";

export const teamsKey = ["teams"] as const;

/** 团队跨空间共享：没有轮询，靠窗口获焦与手动失效更新即可。 */
export function useTeams() {
  return useQuery({
    queryKey: teamsKey,
    queryFn: listTeams,
    staleTime: 60_000,
  });
}
