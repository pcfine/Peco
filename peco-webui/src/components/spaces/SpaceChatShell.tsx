// 单个空间的聊天壳：一条 ensure 出来的 @master 会话 + 一轮一条 GET 流。
//
// 它按 wsId 常驻在页面里，非当前空间只加 `hidden` 而不卸载：ChatView 卸载会
// abort 在途 fetch（服务端随之结束该轮），切空间绝不能走那条路。
//
// 已知限制：离开 `/spaces/:wsId` 路由（状态页、Peco 页）就会卸载本壳，
// 在途的流随之中断 —— 应用内跨空间切换不受影响。

import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChatView, type ChatMessage } from "@/components/chat/ChatView";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { SpaceConflictHint } from "@/components/spaces/SpaceConflictHint";
import {
  conversationStreamUrl,
  getMessages,
  uploadConversationImage,
} from "@/api/conversations";
import { SPACE_MASTER_AGENT, useSpaceConversation } from "@/queries/spaceChat";
import { useSpaceTasks } from "@/queries/spaceDetail";
import { errorText } from "@/lib/api";
import { parseServerTimestamp } from "@/lib/format";
import { TASK_RUNNING_STATES } from "@/lib/taskStates";
import { cn } from "@/lib/utils";
import type { Message } from "@/types/chat";

/** 台账历史还原成展示层消息：一轮一条 GET 流不区分 turn，故 turnIndex 恒 0。 */
function toChatMessages(history: Message[]): ChatMessage[] {
  return history.map((message) => ({
    id: message.id,
    role: message.role,
    content: message.content,
    turnIndex: 0,
    timestampMs: parseServerTimestamp(message.created_at).getTime(),
  }));
}

interface SpaceConversationChatProps {
  wsId: string;
  convId: string;
  visible: boolean;
}

/** 会话 id 到手之后的聊天本体：历史、流地址与冲突提示都需要它。 */
function SpaceConversationChat({
  wsId,
  convId,
  visible,
}: SpaceConversationChatProps) {
  const history = useQuery({
    queryKey: ["messages", SPACE_MASTER_AGENT, convId],
    queryFn: () =>
      getMessages(SPACE_MASTER_AGENT, convId, { limit: 100, wsId }),
    // 挂载即重取：隐藏期间服务端可能已经把这一轮推进完了，缓存不能当历史。
    staleTime: 0,
    refetchOnMount: "always",
  });

  const tasks = useSpaceTasks(wsId);
  const inflight = (tasks.data ?? []).filter((task) =>
    TASK_RUNNING_STATES.includes(task.task_state),
  ).length;

  const initialMessages = useMemo(
    () => toChatMessages(history.data ?? []),
    [history.data],
  );

  return (
    <>
      {history.error && (
        <div className="max-w-[480px] shrink-0 px-3 pt-3">
          <ErrorBanner
            message={errorText(history.error)}
            onRetry={() => void history.refetch()}
          />
        </div>
      )}
      <div className="min-h-0 flex-1">
        <ChatView
          mode="internal"
          visible={visible}
          hideHeader
          streamUrl={(message, imageIds) =>
            conversationStreamUrl(SPACE_MASTER_AGENT, convId, message, {
              imageIds,
              wsId,
            })
          }
          // 空间对话只有图片上传端点（`/conversations/{id}/images`）：
          // 文档附件没有对应的空间级入口，选到文档时由 ChatView 给出提示。
          supportsImages
          uploadImage={(file, onProgress) =>
            uploadConversationImage(SPACE_MASTER_AGENT, convId, file, {
              onProgress,
              wsId,
            })
          }
          initialMessages={initialMessages}
          composerNotice={<SpaceConflictHint inflight={inflight} />}
          welcomeMessage="这是这个空间唯一的 @master 对话。让它做事，或者直接问它。"
        />
      </div>
    </>
  );
}

interface SpaceChatShellProps {
  wsId: string;
  /** 只有当前空间可见；隐藏的壳保持挂载与在途 SSE。 */
  visible: boolean;
}

export function SpaceChatShell({ wsId, visible }: SpaceChatShellProps) {
  const conversation = useSpaceConversation(wsId);
  const convId = conversation.data?.conversation_id ?? null;

  return (
    <div
      data-testid="space-chat-shell"
      data-ws-id={wsId}
      data-visible={visible ? "true" : "false"}
      className={cn(
        "flex h-full min-h-0 min-w-0 flex-col",
        !visible && "hidden",
      )}
    >
      {conversation.error ? (
        <div className="max-w-[480px] p-3">
          <ErrorBanner
            message={errorText(conversation.error)}
            onRetry={() => void conversation.refetch()}
          />
        </div>
      ) : !convId ? (
        <div className="space-y-2 p-3">
          <Skeleton className="h-10 w-full" />
          <Skeleton className="h-10 w-full" />
        </div>
      ) : (
        // 换会话即换聊天状态：key 让 ChatView 的本地消息不跟着串台。
        <SpaceConversationChat
          key={convId}
          wsId={wsId}
          convId={convId}
          visible={visible}
        />
      )}
    </div>
  );
}
