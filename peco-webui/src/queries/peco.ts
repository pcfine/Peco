import { useQuery } from "@tanstack/react-query";
import { getPeco } from "@/api/peco";

/** Peco 身份只在启动时变（记录文件派生），没有轮询的必要。 */
export function usePeco() {
  return useQuery({
    queryKey: ["peco"],
    queryFn: getPeco,
    staleTime: 5 * 60_000,
  });
}
