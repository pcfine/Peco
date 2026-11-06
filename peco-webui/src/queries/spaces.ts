import { useQuery } from "@tanstack/react-query";
import { listSpaces } from "@/api/spaces";

export const spacesKey = ["spaces"] as const;

/** 空间清单 15s 一次：打开/关闭是别的入口触发的，本地没有推送源。 */
export function useSpaces() {
  return useQuery({
    queryKey: spacesKey,
    queryFn: listSpaces,
    refetchInterval: 15_000,
  });
}
