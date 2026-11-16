// 单个空间的聊天壳：一条 ensure 出来的 @master 会话 + 一轮一条 GET 流。
//
// 它按 wsId 常驻在页面里，非当前空间只加 `hidden` 而不卸载：历史查询与本地消息
// 不必每次切回都重建。
//
// 为什么卸载/隐藏都不可怕：一轮的执行归 daemon，SSE 连接只是它的出口之一。断开
// 出口既不停住那一轮（它照常跑到轮末并落库），也不丢已产出的内容 —— 重新可见或
// 重新挂载时 `liveStreamUrl` 接上 `/stream/live`，首帧快照把在途内容补齐，`done`
// 之后再取一次历史以库里为准。所以「切走再回来」靠重订阅收敛，不靠重发。

import { useEffect, useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChatView, type ChatMessage } from "@/components/chat/ChatView";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { SpaceConflictHint } from "@/components/spaces/SpaceConflictHint";
import {
  cancelConversationTurn,
  conversationLiveStreamUrl,
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
import { useSpaceActivityStore } from "@/stores/spaceActivityStore";
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
}

/** 会话 id 到手之后的聊天本体：历史、流地址与冲突提示都需要它。 */
function SpaceConversationChat({
  wsId,
  convId,
  visible,
  teamId,
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
          // ── 在途轮的重订阅（切走再回来靠它收敛，不靠重发）──
          // 历史已落定才订阅：帧序规则要求历史请求先于订阅请求发出。
          historySettled={history.isFetched}
          liveStreamUrl={() =>
            conversationLiveStreamUrl(SPACE_MASTER_AGENT, convId, { wsId })
          }
          // 「停止」改走服务端取消：detach 之后掐本地连接停不住那一轮。
          cancelTurn={() =>
            cancelConversationTurn(SPACE_MASTER_AGENT, convId, { wsId })
          }
          // 快照落定补一次历史（user 行可能晚于历史快照落库）；
          // `done` 之后同样取一次：以落库结果替换本地气泡。
          onAttachSettled={() => void history.refetch()}
          onTurnSettled={() => void history.refetch()}
          // 一轮的起止进壳级 store：文件面板靠它在回复落地后立刻重取，
          // 不必等下一次轮询（dock 头部的对话状态也读同一份）。
          onStreamingChange={(streaming) => {
            setChat(streaming ? { kind: "running" } : { kind: "idle" });
            // 连接态只有一份，活动按 wsId 记录；隐藏的壳同样上报，供清除对话的禁用态读取。
            useSpaceActivityStore.getState().setStreaming(wsId, streaming);
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
}

export function SpaceChatShell({
  wsId,
  visible,
  teamId = null,
}: SpaceChatShellProps) {
  const conversation = useSpaceConversation(wsId);
  const convId = conversation.data?.conversation_id ?? null;

  // 壳卸载后没人再上报活动位：必须复位，否则清除按钮会一直不可用。
  // （卸载本身不会中断服务端那一轮 —— 见文件头的说明。）
  useEffect(() => {
    return () => useSpaceActivityStore.getState().setStreaming(wsId, false);
  }, [wsId]);

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
        />
      )}
    </div>
  );
}
