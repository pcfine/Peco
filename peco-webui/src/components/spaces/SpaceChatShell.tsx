// 单个空间的聊天壳：一条 ensure 出来的 @master 会话 + 一轮一条 GET 流。
//
// 它按 wsId 常驻在页面里，非当前空间只加 `hidden` 而不卸载：ChatView 卸载会
// abort 在途 fetch（服务端随之结束该轮），切空间绝不能走那条路。
//
// 已知限制：离开承载它的路由（`/spaces/:wsId`、`/peco` 或状态页）就会卸载本壳，
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
import { useInflightTaskCount, useSpaceTasks } from "@/queries/spaceDetail";
import { errorText } from "@/lib/api";
import { parseServerTimestamp } from "@/lib/format";
import { cn } from "@/lib/utils";
import { useConnectionStore } from "@/stores/connectionStore";
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
  /** 空对话的说明要知道自己属于哪个团队；拿不到时退回通用文案。 */
  teamId: string | null;
  onStreamingChange?: (wsId: string, streaming: boolean) => void;
}

/** 会话 id 到手之后的聊天本体：历史、流地址与冲突提示都需要它。 */
function SpaceConversationChat({
  wsId,
  convId,
  visible,
  teamId,
  onStreamingChange,
}: SpaceConversationChatProps) {
  const setChat = useConnectionStore((state) => state.setChat);

  const history = useQuery({
    queryKey: ["messages", SPACE_MASTER_AGENT, convId],
    queryFn: () =>
      getMessages(SPACE_MASTER_AGENT, convId, { limit: 100, wsId }),
    // 挂载即重取：隐藏期间服务端可能已经把这一轮推进完了，缓存不能当历史。
    staleTime: 0,
    refetchOnMount: "always",
  });

  // 隐藏的壳不轮询实时列表：它只影响提示与角标，为此让每个后台空间都发请求
  // 不划算 —— 拿不到时这个 hook 自己回落到台账。
  const inflight = useInflightTaskCount(wsId, { enabled: visible });

  // 空对话的说明只在台账到手后才敢说「还没有执行过任务」：任务也可能由 Peco
  // 或别的空间派发进来，没有会话不等于没跑过东西。
  const tasks = useSpaceTasks(wsId);
  const welcomeMessage = useMemo(() => {
    if (!teamId) {
      return "这是这个空间唯一的 @master 对话。让它做事，或者直接问它。";
    }
    const team = `「${teamId}」团队`;
    return tasks.data?.length === 0
      ? `这是${team}的工作空间，还没有执行过任务。`
      : `这是${team}的工作空间。让它做事，或者直接问它。`;
  }, [teamId, tasks.data]);

  const initialMessages = useMemo(
    () => toChatMessages(history.data ?? []),
    [history.data],
  );

  return (
    <>
      {history.error && (
        <div className="mx-auto w-full max-w-chat shrink-0 px-3 pt-3">
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
          // 一轮的起止进壳级 store：文件面板靠它在回复落地后立刻重取，
          // 不必等下一次轮询（dock 头部的对话状态也读同一份）。
          onStreamingChange={(streaming) => {
            setChat(streaming ? { kind: "running" } : { kind: "idle" });
            // store 里只有一份「谁在跑」：隐藏的空间也写同一个位。要按空间
            // 判断的调用方（清除对话的可用性）得另收一份带 wsId 的上报。
            onStreamingChange?.(wsId, streaming);
          }}
          composerNotice={<SpaceConflictHint inflight={inflight} />}
          welcomeMessage={welcomeMessage}
        />
      </div>
    </>
  );
}

interface SpaceChatShellProps {
  wsId: string;
  /** 只有当前空间可见；隐藏的壳保持挂载与在途 SSE。 */
  visible: boolean;
  /** 空间所属团队；空对话的说明用它拼出「哪个团队的工作空间」。 */
  teamId?: string | null;
  /** 这一轮起止的按空间上报；隐藏的壳同样会报，调用方按 wsId 自取。 */
  onStreamingChange?: (wsId: string, streaming: boolean) => void;
}

export function SpaceChatShell({
  wsId,
  visible,
  teamId = null,
  onStreamingChange,
}: SpaceChatShellProps) {
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
        <div className="mx-auto w-full max-w-chat p-3">
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
          teamId={teamId}
          onStreamingChange={onStreamingChange}
        />
      )}
    </div>
  );
}
