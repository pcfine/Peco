// 空间唯一会话的取回。
//
// `ensure` 是 POST，但语义是「取回那一条会话」，所以走 query 而不是 mutation：
// 多个空间壳同时挂着时只会打一次，重挂载也不会建出第二条。

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { clearSpaceConversation, ensureSpaceConversation } from "@/api/spaces";
import { errorText } from "@/lib/api";

/** 空间聊天只有这一个 agent，名字由后端约定；前端只拿它拼 queryKey 与流地址。 */
export const SPACE_MASTER_AGENT = "@master";

function spaceConversationKey(wsId: string) {
  return ["space-conversation", wsId, SPACE_MASTER_AGENT];
}

function messagesKey(convId: string) {
  return ["messages", SPACE_MASTER_AGENT, convId];
}

export function useSpaceConversation(wsId: string) {
  return useQuery({
    queryKey: spaceConversationKey(wsId),
    queryFn: () => ensureSpaceConversation(wsId),
    // 会话 id 一旦定下来就不再变：重取只会白打一次 POST。
    staleTime: Infinity,
    gcTime: Infinity,
    // 页面与聊天壳是两个观察者、先后挂载：带错误的查询不该被后挂载的那个再打一遍。
    retryOnMount: false,
    // 失败交给页面的「重试」按钮，自动重试只是把同一个错误再打一遍。
    retry: false,
  });
}

/**
 * 清空空间当前会话的消息：会话行留着，会话 id 因此不变。
 *
 * 失效消息缓存就够了 —— `ChatView` 见 `initialMessages` 换了引用就把本地消息
 * 换成新取回的（空）历史；会话缓存一动反而会让聊天壳以为换了一条会话。
 */
export function useClearSpaceConversation(wsId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    // 请求只认空间；这里仍收 convId，是为了让 onSuccess 知道该失效谁的 messages。
    mutationFn: (_convId: string) =>
      clearSpaceConversation(wsId, SPACE_MASTER_AGENT),
    onSuccess: (_result, convId) =>
      queryClient.invalidateQueries({ queryKey: messagesKey(convId) }),
    // 后端拒绝的原因（空间已关闭之类）要原样说出来，弹窗保持打开，
    // 用户可以再试或取消。
    onError: (error) => toast.error(errorText(error)),
  });
}
