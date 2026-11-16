// dock 的对话归属：与 Peco 页共用同一条空间会话（同一 convId、同一份历史）。
// 抽屉与页面壳（含隐藏的那些）都观察同一个 key，`ensure` 幂等 —— 前端不必
// 再自己记一份 id。

import { useSpaceConversation } from "@/queries/spaceChat";
import { usePeco } from "@/queries/peco";

/** 派发到空间的任务都由总管 agent 接；这是唯一一个跨空间常驻的 agent。 */
export const PECO_AGENT_ID = "@master";

export interface DockConversation {
  agentId: string;
  wsId: string | null;
  convId: string | null;
  /** 取回会话的失败原因（例如空间不存在 → 404）。 */
  error: unknown;
  /** 失败后重取：这条查询不在重挂载时重试，翻盘得靠它。 */
  retry: () => void;
}

export function useDockConversation(open: boolean): DockConversation {
  const { data: peco } = usePeco();
  const wsId = peco?.ws_id ?? null;
  // 空间未定时查询不启用，空 key 不会发出去。
  const conversation = useSpaceConversation(wsId ?? "", {
    enabled: open && Boolean(wsId),
  });

  return {
    agentId: PECO_AGENT_ID,
    wsId,
    convId: conversation.data?.conversation_id ?? null,
    error: conversation.error,
    retry: () => void conversation.refetch(),
  };
}
