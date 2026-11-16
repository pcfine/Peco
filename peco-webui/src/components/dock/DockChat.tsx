// dock 的对话 tab：ChatView internal 模式，一轮一条 GET 流，
// 端点是 daemon 的 `/api/chat/@master/conversations/{id}/stream`。

import { useEffect, useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChatView, type ChatMessage } from "@/components/chat/ChatView";
import {
  cancelConversationTurn,
  conversationLiveStreamUrl,
  conversationStreamUrl,
  getMessages,
  uploadConversationImage,
} from "@/api/conversations";
import { parseServerTimestamp } from "@/lib/format";
import { useSpaceActivityStore } from "@/stores/spaceActivityStore";

/** 抽屉在这条会话上的身份：与页面壳各记一笔，互不覆盖。 */
const OWNER = "dock";

interface DockChatProps {
  agentId: string;
  convId: string;
  /** dock 恒落在 Peco 空间：与「当前空间」无关，必须显式带上。 */
  wsId: string;
}

export function DockChat({ agentId, convId, wsId }: DockChatProps) {
  const setStreaming = useSpaceActivityStore((state) => state.setStreaming);
  // 切到「产物」tab 会把聊天卸掉，之后没有任何回调会再上报：不复位的话头部
  // 会一直停在「生成中」。卸载只是断开出入口，服务端那一轮照常跑完并落库。
  useEffect(
    () => () =>
      useSpaceActivityStore.getState().setStreaming(wsId, OWNER, false),
    [wsId],
  );
  const historyQuery = useQuery({
    queryKey: ["messages", agentId, convId],
    queryFn: () => getMessages(agentId, convId, { limit: 100, wsId }),
    // 重挂载时重取：dock 关掉再开，本轮新消息只在服务端。
    staleTime: 0,
    refetchOnMount: "always",
  });

  const history = historyQuery.data;

  const initialMessages = useMemo<ChatMessage[]>(
    () =>
      (history ?? []).map((message) => ({
        id: message.id,
        role: message.role === "user" ? "user" : "assistant",
        content: message.content,
        turnIndex: 0,
        timestampMs: parseServerTimestamp(message.created_at).getTime(),
      })),
    [history],
  );

  return (
    <ChatView
      key={convId}
      mode="internal"
      hideHeader
      supportsImages
      uploadImage={(file, onProgress) =>
        uploadConversationImage(agentId, convId, file, { onProgress, wsId })
      }
      streamUrl={(message, imageIds) =>
        conversationStreamUrl(agentId, convId, message, { imageIds, wsId })
      }
      initialMessages={initialMessages}
      // ── 在途轮的重订阅：dock 关掉再开等于「回到对话」，接上在途轮而不是重发 ──
      historySettled={historyQuery.isFetched}
      liveStreamUrl={() => conversationLiveStreamUrl(agentId, convId, { wsId })}
      // 「停止」改走服务端取消（200 = 命令已投递，等 done 收尾）
      cancelTurn={() => cancelConversationTurn(agentId, convId, { wsId })}
      onAttachSettled={() => void historyQuery.refetch()}
      onTurnSettled={() => void historyQuery.refetch()}
      welcomeMessage="让 Peco 开始一件事。它会派发到某个空间，或直接回答。"
      suggestions={["现在有哪些任务在跑？", "团队的能力边界是什么？"]}
      className="h-full"
      // 一轮一条 fetch 流：没有常驻连接可重连，只有「生成中 / 空闲」两态。
      onStreamingChange={(streaming) => setStreaming(wsId, OWNER, streaming)}
    />
  );
}
