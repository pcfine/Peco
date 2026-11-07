// 空间唯一会话的取回。
//
// `ensure` 是 POST，但语义是「取回那一条会话」，所以走 query 而不是 mutation：
// 多个空间壳同时挂着时只会打一次，重挂载也不会建出第二条。

import { useQuery } from "@tanstack/react-query";
import { ensureSpaceConversation } from "@/api/spaces";

/** 空间聊天只有这一个 agent，名字由后端约定；前端只拿它拼 queryKey 与流地址。 */
export const SPACE_MASTER_AGENT = "@master";

export function useSpaceConversation(wsId: string) {
  return useQuery({
    queryKey: ["space-conversation", wsId, SPACE_MASTER_AGENT],
    queryFn: () => ensureSpaceConversation(wsId),
    // 会话 id 一旦定下来就不再变：重取只会白打一次 POST。
    staleTime: Infinity,
    gcTime: Infinity,
    // 失败交给页面的「重试」按钮，自动重试只是把同一个错误再打一遍。
    retry: false,
  });
}
