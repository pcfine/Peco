// 「清除对话」的三步：点图标 → 二次确认 → 清空。空间页与 Peco 页共用这一套。
//
// 两个页面各写一份的话，「会话 id 未就绪 / 有一轮在跑」的按不动条件与文案迟早分叉，
// 而这两件事的判断依据（会话查询 + 按 wsId 记的流式位）本就来自同一批数据。

import { useState } from "react";
import {
  useClearSpaceConversation,
  useSpaceConversation,
} from "@/queries/spaceChat";

export interface ConversationClear {
  /** 二次确认弹窗的开合。 */
  dialogOpen: boolean;
  openDialog: () => void;
  cancel: () => void;
  confirm: () => void;
  /** 清空请求在途：弹窗里的两个按钮都按不动。 */
  pending: boolean;
  /** 会话 id 未就绪时按钮按不动，但仍留着位置与说明。 */
  disabled: boolean;
  /** 现在不能清的其它原因（有一轮在跑）：挂到 Tooltip 上，用户不必撞一次 409。 */
  disabledReason?: string;
}

interface ConversationClearOptions {
  /** 空间还没打开时不要发 ensure：先去「打开」，别让用户先吃一个 409。 */
  enabled?: boolean;
}

export function useConversationClear(
  wsId: string,
  streaming: boolean,
  { enabled = true }: ConversationClearOptions = {},
): ConversationClear {
  const conversation = useSpaceConversation(wsId, { enabled });
  const convId = conversation.data?.conversation_id ?? null;
  const clear = useClearSpaceConversation(wsId);
  const [dialogOpen, setDialogOpen] = useState(false);

  // 失败不关弹窗：错误原因已经 toast 出来，用户还能再试或取消。
  const confirm = () => {
    if (!convId) return;
    clear.mutate(convId, { onSuccess: () => setDialogOpen(false) });
  };

  return {
    dialogOpen,
    openDialog: () => setDialogOpen(true),
    cancel: () => setDialogOpen(false),
    confirm,
    pending: clear.isPending,
    disabled: !convId,
    disabledReason: streaming ? "对话进行中，结束后再清除" : undefined,
  };
}
