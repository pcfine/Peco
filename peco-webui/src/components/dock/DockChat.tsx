// dock 的对话 tab：ChatView internal 模式，一轮一条 GET 流，
// 端点是 daemon 的 `/api/chat/@master/conversations/{id}/stream`。

import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChatView, type ChatMessage } from "@/components/chat/ChatView";
import {
  conversationStreamUrl,
  getMessages,
  uploadConversationImage,
} from "@/api/conversations";
import { parseServerTimestamp } from "@/lib/format";
import { useConnectionStore } from "@/stores/connectionStore";

interface DockChatProps {
  agentId: string;
  convId: string;
  /** dock 恒落在 Peco 空间：与「当前空间」无关，必须显式带上。 */
  wsId: string;
}

export function DockChat({ agentId, convId, wsId }: DockChatProps) {
  const setChat = useConnectionStore((state) => state.setChat);
  const { data: history } = useQuery({
    queryKey: ["messages", agentId, convId],
    queryFn: () => getMessages(agentId, convId, { limit: 100, wsId }),
    // 重挂载时重取：dock 关掉再开，本轮新消息只在服务端。
    staleTime: 0,
    refetchOnMount: "always",
  });

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
      headerTitle="Peco"
      supportsImages
      uploadImage={(file, onProgress) =>
        uploadConversationImage(agentId, convId, file, { onProgress, wsId })
      }
      streamUrl={(message, imageIds) =>
        conversationStreamUrl(agentId, convId, message, { imageIds, wsId })
      }
      initialMessages={initialMessages}
      welcomeMessage="让 Peco 开始一件事。它会派发到某个空间，或直接回答。"
      suggestions={["现在有哪些任务在跑？", "团队的能力边界是什么？"]}
      className="h-full"
      // 一轮一条 fetch 流：没有常驻连接可重连，只有「生成中 / 空闲」两态。
      onStreamingChange={(streaming) =>
        setChat(streaming ? { kind: "running" } : { kind: "idle" })
      }
    />
  );
}
