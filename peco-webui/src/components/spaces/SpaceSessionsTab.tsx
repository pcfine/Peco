// 会话 tab：左侧会话列表 + ChatView 本体。空间用 URL 里的 wsId，
// 不用 `currentWsId` —— 详情页看的是 URL 指的那个空间。

import { useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { ChatView, type ChatMessage } from "@/components/chat/ChatView";
import { ConversationList } from "@/components/chat/ConversationList";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import {
  conversationStreamUrl,
  createConversation,
  deleteConversation,
  getMessages,
  listConversations,
  updateConversation,
  uploadConversationImage,
} from "@/api/conversations";
import { errorText } from "@/lib/api";
import { parseServerTimestamp } from "@/lib/format";
import { cn } from "@/lib/utils";

/** 空间里的总管 agent：派发都落到它身上，会话也归它。 */
const AGENT = "@master";

interface SpaceSessionsTabProps {
  wsId: string;
  session: string | null;
  onSelectSession: (id: string) => void;
}

export function SpaceSessionsTab({
  wsId,
  session,
  onSelectSession,
}: SpaceSessionsTabProps) {
  const queryClient = useQueryClient();
  const listKey = ["conversations", wsId, AGENT] as const;

  const conversations = useQuery({
    queryKey: listKey,
    queryFn: () => listConversations(AGENT, { wsId }),
  });

  const active = session ?? conversations.data?.[0]?.id ?? null;

  const history = useQuery({
    queryKey: ["messages", wsId, AGENT, active],
    queryFn: () => getMessages(AGENT, active as string, { limit: 100, wsId }),
    enabled: Boolean(active),
    // 重挂载时重取：dock 之外改过的对话只能从服务端看到。
    staleTime: 0,
    refetchOnMount: "always",
  });

  const initialMessages = useMemo<ChatMessage[]>(
    () =>
      (history.data ?? []).map((message) => ({
        id: message.id,
        role: message.role === "user" ? "user" : "assistant",
        content: message.content,
        turnIndex: 0,
        timestampMs: parseServerTimestamp(message.created_at).getTime(),
      })),
    [history.data],
  );

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: listKey });
  };

  const create = useMutation({
    mutationFn: () => createConversation(AGENT, undefined, wsId),
    onSuccess: (conversation) => {
      onSelectSession(conversation.id);
      invalidate();
    },
    onError: (error) => toast.error(errorText(error)),
  });

  const rename = useMutation({
    mutationFn: ({ id, title }: { id: string; title: string }) =>
      updateConversation(AGENT, id, { title }, wsId),
    onSuccess: invalidate,
    onError: (error) => toast.error(errorText(error)),
  });

  const archive = useMutation({
    mutationFn: (id: string) =>
      updateConversation(AGENT, id, { archive: true }, wsId),
    onSuccess: (_conversation, id) => {
      if (id === active) onSelectSession("");
      invalidate();
    },
    onError: (error) => toast.error(errorText(error)),
  });

  const remove = useMutation({
    mutationFn: (id: string) => deleteConversation(AGENT, id, wsId),
    onSuccess: (_result, id) => {
      if (id === active) onSelectSession("");
      invalidate();
    },
    onError: (error) => toast.error(errorText(error)),
  });

  if (conversations.error) {
    return (
      <div className="p-3">
        <ErrorBanner
          message={errorText(conversations.error)}
          onRetry={() => void conversations.refetch()}
        />
      </div>
    );
  }

  const rows = conversations.data ?? [];

  return (
    <div className="flex h-full min-h-0">
      <div className="hidden w-60 shrink-0 flex-col border-r border-line @min-[900px]/work:flex">
        <ConversationList
          conversations={rows}
          activeConvId={active}
          unreadCounts={new Map()}
          loading={conversations.isPending}
          error={null}
          onSelectConversation={onSelectSession}
          onNewConversation={() => create.mutate()}
          onRename={async (id, title) => {
            await rename.mutateAsync({ id, title });
          }}
          onArchive={async (id) => {
            await archive.mutateAsync(id);
          }}
          onDelete={async (id) => {
            await remove.mutateAsync(id);
          }}
          onRetry={() => void conversations.refetch()}
        />
      </div>

      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        {/* 窄容器没有左栏：会话切换降级成一行横向可滚的标签。 */}
        {rows.length > 1 && (
          <div className="flex h-8 shrink-0 items-center gap-1 overflow-x-auto border-b border-line px-2 @min-[900px]/work:hidden">
            {rows.map((conversation) => (
              <button
                key={conversation.id}
                type="button"
                onClick={() => onSelectSession(conversation.id)}
                className={cn(
                  "shrink-0 rounded-[4px] px-2 text-2xs",
                  conversation.id === active
                    ? "bg-line text-ink"
                    : "text-ink-muted hover:text-ink",
                )}
              >
                {conversation.title || "未命名"}
              </button>
            ))}
          </div>
        )}

        {!active ? (
          <div className="p-3">
            <EmptyState
              title="这个空间还没有会话。"
              action={
                <button
                  type="button"
                  disabled={create.isPending}
                  onClick={() => create.mutate()}
                  className="text-xs text-primary-ink hover:underline"
                >
                  开始对话
                </button>
              }
            />
          </div>
        ) : (
          <ChatView
            key={active}
            mode="internal"
            headerTitle="空间会话"
            supportsImages
            uploadImage={(file, onProgress) =>
              uploadConversationImage(AGENT, active, file, {
                onProgress,
                wsId,
              })
            }
            streamUrl={(message, imageIds) =>
              conversationStreamUrl(AGENT, active, message, {
                imageIds,
                wsId,
              })
            }
            initialMessages={initialMessages}
            welcomeMessage="这个空间已经就绪。把要它做的事写下来。"
            className="h-full p-3"
          />
        )}
      </div>
    </div>
  );
}
