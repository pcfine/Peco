// dock 的对话归属：Peco 空间 + `@master` 一条对话，按空间分开记。
// 对话不存在时补建一条 —— 单用户 daemon 没有「默认对话」的概念，得自己造。

import { useEffect, useRef } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createConversation, listConversations } from "@/api/conversations";
import { usePeco } from "@/queries/peco";
import { useDockStore } from "@/stores/dockStore";

/** 派发到空间的任务都由总管 agent 接；这是唯一一个跨空间常驻的 agent。 */
export const PECO_AGENT_ID = "@master";

export interface DockConversation {
  agentId: string;
  wsId: string | null;
  convId: string | null;
  /** 列对话或补建对话的失败原因（例如空间未打开 → 409）。 */
  error: unknown;
}

export function useDockConversation(open: boolean): DockConversation {
  const queryClient = useQueryClient();
  const { data: peco } = usePeco();
  const wsId = peco?.ws_id ?? null;
  const convId = useDockStore((state) =>
    wsId ? (state.conversationByWs[wsId] ?? null) : null,
  );
  const setConversation = useDockStore((state) => state.setConversation);

  const { data: conversations, error: listError } = useQuery({
    queryKey: ["conversations", wsId, PECO_AGENT_ID],
    queryFn: () => listConversations(PECO_AGENT_ID, { wsId: wsId as string }),
    enabled: open && Boolean(wsId),
  });

  // 已有对话：接最近更新的一条。store 里的 id 失效（对话被删）时也走这里。
  const firstId = conversations?.[0]?.id ?? null;
  const knowsConv =
    convId !== null && (conversations ?? []).some((c) => c.id === convId);
  useEffect(() => {
    if (!wsId || !firstId || knowsConv) return;
    setConversation(wsId, firstId);
  }, [wsId, firstId, knowsConv, setConversation]);

  const create = useMutation({
    // 变量只是「给哪个空间建」，服务端不需要它 —— 空间由请求头决定。
    mutationFn: (spaceId: string) =>
      createConversation(PECO_AGENT_ID, undefined, spaceId),
    onSuccess: (conversation, spaceId) => {
      setConversation(spaceId, conversation.id);
      void queryClient.invalidateQueries({
        queryKey: ["conversations", spaceId, PECO_AGENT_ID],
      });
    },
  });

  // 列表拉回来是空的且没有本地记录 ⇒ 补建。`create.isPending` 挡住重复提交。
  const creating = useRef(false);
  useEffect(() => {
    if (!open || !wsId || convId || !conversations) return;
    if (conversations.length > 0 || creating.current) return;
    creating.current = true;
    create.mutate(wsId, { onSettled: () => (creating.current = false) });
  }, [open, wsId, convId, conversations, create]);

  return {
    agentId: PECO_AGENT_ID,
    wsId,
    convId: knowsConv ? convId : null,
    error: create.error ?? listError,
  };
}
