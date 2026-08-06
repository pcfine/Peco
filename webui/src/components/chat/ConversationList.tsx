// ConversationList — Agent 左侧对话历史列表面板

import { useState } from "react";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import {
  Plus,
  Archive,
  ChevronDown,
  ChevronRight,
  Pencil,
  Trash2,
  Box,
} from "lucide-react";
import type { Conversation } from "@/types/chat";

// ── Relative time formatter ────────────────────────────────────────────────

function relativeTime(dateStr: string): string {
  const now = Date.now();
  const then = new Date(dateStr).getTime();
  if (isNaN(then)) return "";
  const diffSec = Math.floor((now - then) / 1000);
  if (diffSec < 60) return "刚刚";
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)} 分钟前`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)} 小时前`;
  if (diffSec < 604800) return `${Math.floor(diffSec / 86400)} 天前`;
  return new Date(dateStr).toLocaleDateString("zh-CN");
}

// ── Props ──────────────────────────────────────────────────────────────────

export interface ConversationListProps {
  conversations: Conversation[];
  activeConvId: string | null;
  unreadCounts: Map<string, number>;
  loading: boolean;
  error: string | null;
  onSelectConversation: (convId: string) => void;
  onNewConversation: () => void;
  onRename: (convId: string, title: string) => Promise<void>;
  onArchive: (convId: string) => Promise<void>;
  onDelete: (convId: string) => Promise<void>;
  onRetry: () => void;
  archivedConversations?: Conversation[];
  archivedLoading?: boolean;
  archivedExpanded?: boolean;
  onToggleArchived?: () => void;
}

// ── Component ──────────────────────────────────────────────────────────────

export function ConversationList({
  conversations,
  activeConvId,
  unreadCounts,
  loading,
  error,
  onSelectConversation,
  onNewConversation,
  onRename,
  onArchive,
  onDelete,
  onRetry,
  archivedConversations = [],
  archivedLoading = false,
  archivedExpanded = false,
  onToggleArchived,
}: ConversationListProps) {
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editTitle, setEditTitle] = useState("");
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [olderExpanded, setOlderExpanded] = useState(false);

  // Show at most RECENT_LIMIT conversations directly; the rest are folded
  const RECENT_LIMIT = 10;
  const recentConversations = conversations.slice(0, RECENT_LIMIT);
  const olderConversations = conversations.slice(RECENT_LIMIT);

  // ── Inline rename handlers ──────────────────────────────────────────────

  const startRename = (conv: Conversation) => {
    setEditingId(conv.id);
    setEditTitle(conv.title);
  };

  const submitRename = async (convId: string) => {
    if (editTitle.trim()) {
      await onRename(convId, editTitle.trim());
    }
    setEditingId(null);
  };

  const cancelRename = () => {
    setEditingId(null);
  };

  // ── Delete handlers ─────────────────────────────────────────────────────

  const handleDelete = async (convId: string) => {
    setDeletingId(convId);
    try {
      await onDelete(convId);
    } finally {
      setDeletingId(null);
    }
  };

  // ── Render helpers ──────────────────────────────────────────────────────

  const renderConversationItem = (conv: Conversation, isArchived = false) => {
    const isActive = conv.id === activeConvId;
    const unread = unreadCounts.get(conv.id) ?? 0;
    const hasUnread = unread > 0 && !isActive;

    return (
      <div
        key={conv.id}
        className={cn(
          "group flex items-center gap-1.5 rounded-md px-2 py-1.5 text-sm cursor-pointer transition-colors hover:bg-accent",
          isActive && "bg-accent",
          isArchived && "text-muted-foreground",
        )}
        onClick={() => onSelectConversation(conv.id)}
      >
        {/* Icon */}
        <span className="shrink-0 text-xs">
          {isArchived ? (
            <Box className="h-3.5 w-3.5" />
          ) : hasUnread ? (
            <span className="inline-block w-2 h-2 rounded-full bg-blue-500" />
          ) : (
            <span className="inline-block w-2 h-2" />
          )}
        </span>

        {/* Title / Inline edit */}
        {editingId === conv.id ? (
          <Input
            className="h-6 flex-1 min-w-0 text-xs"
            value={editTitle}
            onChange={(e) => setEditTitle(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") submitRename(conv.id);
              if (e.key === "Escape") cancelRename();
            }}
            onBlur={() => submitRename(conv.id)}
            autoFocus
            onClick={(e) => e.stopPropagation()}
          />
        ) : (
          <span
            className={cn(
              "flex-1 min-w-0 truncate max-w-[14ch]",
              isActive && "font-semibold",
            )}
          >
            {conv.title
              ? conv.title.length > 7
                ? conv.title.slice(0, 7) + "…"
                : conv.title
              : "新对话"}
          </span>
        )}

        {/* Time + Actions slot — stable width, actions overlay time */}
        <div className="shrink-0 relative flex items-center">
          {/* Time — always in layout flow, invisible when actions shown */}
          <span
            className={cn(
              "text-[10px] text-muted-foreground whitespace-nowrap group-hover:invisible",
              isActive && "invisible",
            )}
          >
            {relativeTime(conv.updated_at)}
          </span>
          {/* Actions — absolutely positioned over time slot */}
          <div
            className={cn(
              "absolute inset-0 items-center gap-0.5",
              isActive ? "flex" : "hidden group-hover:flex",
            )}
          >
            {!isArchived && (
              <>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-5 w-5"
                  onClick={(e) => {
                    e.stopPropagation();
                    startRename(conv);
                  }}
                >
                  <Pencil className="h-3 w-3" />
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-5 w-5"
                  onClick={(e) => {
                    e.stopPropagation();
                    onArchive(conv.id);
                  }}
                >
                  <Archive className="h-3 w-3" />
                </Button>
              </>
            )}
            <Button
              variant="ghost"
              size="icon"
              className="h-5 w-5 text-destructive hover:text-destructive"
              disabled={deletingId === conv.id}
              onClick={(e) => {
                e.stopPropagation();
                handleDelete(conv.id);
              }}
            >
              <Trash2 className="h-3 w-3" />
            </Button>
          </div>
        </div>
      </div>
    );
  };

  // ── Render ──────────────────────────────────────────────────────────────

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between px-3 py-2 border-b">
        <span className="text-sm font-medium">历史对话</span>
        <Button
          variant="ghost"
          size="icon"
          className="h-7 w-7"
          onClick={onNewConversation}
        >
          <Plus className="h-4 w-4" />
        </Button>
      </div>

      {/* Content */}
      <ScrollArea className="flex-1">
        <div className="p-2 space-y-0.5">
          {/* Loading */}
          {loading && (
            <div className="space-y-2 p-2">
              {Array.from({ length: 5 }).map((_, i) => (
                <Skeleton key={i} className="h-8 w-full" />
              ))}
            </div>
          )}

          {/* Error */}
          {!loading && error && (
            <div className="text-center py-8 px-2">
              <p className="text-xs text-muted-foreground mb-2">{error}</p>
              <Button variant="outline" size="sm" onClick={onRetry}>
                重试
              </Button>
            </div>
          )}

          {/* Empty */}
          {!loading && !error && conversations.length === 0 && (
            <div className="text-center py-8 px-2">
              <p className="text-xs text-muted-foreground">
                暂无对话，开始第一条吧
              </p>
            </div>
          )}

          {/* Active conversations — recent 10 */}
          {!loading &&
            !error &&
            recentConversations.map((c) => renderConversationItem(c))}

          {/* Older conversations fold */}
          {!loading && !error && olderConversations.length > 0 && (
            <div className="mt-3 pt-2 border-t">
              <button
                className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground px-2 py-1 w-full"
                onClick={() => setOlderExpanded((v) => !v)}
              >
                {olderExpanded ? (
                  <ChevronDown className="h-3 w-3" />
                ) : (
                  <ChevronRight className="h-3 w-3" />
                )}
                更早的对话
                <span className="text-muted-foreground/60">
                  ({olderConversations.length})
                </span>
              </button>
              {olderExpanded && (
                <div className="mt-1">
                  {olderConversations.map((c) => renderConversationItem(c))}
                </div>
              )}
            </div>
          )}

          {/* Archived section */}
          {!loading && !error && onToggleArchived && (
            <div className="mt-3 pt-2 border-t">
              <button
                className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground px-2 py-1 w-full"
                onClick={onToggleArchived}
              >
                {archivedExpanded ? (
                  <ChevronDown className="h-3 w-3" />
                ) : (
                  <ChevronRight className="h-3 w-3" />
                )}
                已归档
                {archivedConversations.length > 0 && (
                  <span className="text-muted-foreground/60">
                    ({archivedConversations.length})
                  </span>
                )}
              </button>
              {archivedExpanded && (
                <div className="mt-1">
                  {archivedLoading && <Skeleton className="h-6 w-full mx-2" />}
                  {!archivedLoading && archivedConversations.length === 0 && (
                    <p className="text-[10px] text-muted-foreground px-2 py-1">
                      暂无归档对话
                    </p>
                  )}
                  {!archivedLoading &&
                    archivedConversations.map((c) =>
                      renderConversationItem(c, true),
                    )}
                </div>
              )}
            </div>
          )}
        </div>
      </ScrollArea>
    </div>
  );
}
