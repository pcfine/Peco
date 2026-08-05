// AgentChatPage — 双栏对话界面 + keep-alive ChatView 池 (max 5 SSE)

import { useEffect, useState, useCallback, useRef } from "react";
import { useParams, useNavigate, useLocation } from "react-router-dom";
import { ChatView, snapshotToMessages } from "@/components/chat/ChatView";
import { ConversationList } from "@/components/chat/ConversationList";
import {
  getSessionSnapshot,
  listConversations,
  updateConversation,
  deleteConversation as deleteConversationApi,
  createConversation,
} from "@/api/conversations";
import { getAgent, listAgents } from "@/api/agents";
import type { AgentDetail } from "@/types/agent";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { AlertCircle, ArrowLeft, Send } from "lucide-react";
import { toast } from "sonner";
import type { ChatMessage } from "@/components/chat/ChatView";
import type { Conversation } from "@/types/chat";

// ── Constants ──────────────────────────────────────────────────────────────

const MAX_CONCURRENT = 5;
const EMPTY_MESSAGES: ChatMessage[] = [];

// ── Types ──────────────────────────────────────────────────────────────────

interface ChatInstance {
  convId: string;
  lastAccessTime: number;
}

// ── Component ──────────────────────────────────────────────────────────────

export function AgentChatPage() {
  const { agentId, conversationId } = useParams<{
    agentId: string;
    conversationId: string;
  }>();
  const navigate = useNavigate();
  const location = useLocation();

  // ── State ─────────────────────────────────────────────────────────────

  // Initialize chatPool immediately with the URL's conversationId so ChatView
  // renders without waiting for snapshot load. Prevents the "no content" bug
  // where a failed snapshot load leaves the right panel empty.
  const [chatPool, setChatPool] = useState<Map<string, ChatInstance>>(() => {
    const map = new Map<string, ChatInstance>();
    if (conversationId) {
      map.set(conversationId, {
        convId: conversationId,
        lastAccessTime: Date.now(),
      });
    }
    return map;
  });
  const [visibleConvId, setVisibleConvId] = useState<string | null>(conversationId ?? null);
  const [unreadCounts, setUnreadCounts] = useState<Map<string, number>>(new Map());
  const [snapshots, setSnapshots] = useState<Map<string, ChatMessage[]>>(new Map());
  const [snapshotReady, setSnapshotReady] = useState(false);
  const [initialQuery, setInitialQuery] = useState<string | undefined>();
  const [invalidConv, setInvalidConv] = useState(false);
  const [loading, setLoading] = useState(true);

  // Conversation list state
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [convListLoading, setConvListLoading] = useState(true);
  const [convListError, setConvListError] = useState<string | null>(null);
  const [archivedConversations, setArchivedConversations] = useState<Conversation[]>([]);
  const [archivedExpanded, setArchivedExpanded] = useState(false);
  const [archivedLoading, setArchivedLoading] = useState(false);

  // Agent detail
  const [agent, setAgent] = useState<AgentDetail | null>(null);

  // Mobile
  const [mobilePanelOpen, setMobilePanelOpen] = useState(false);

  // New conversation input
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);

  // Track which convId came from StartChatPage (for initialQuery targeting)
  const newFromStartRef = useRef<string | null>(null);

  // ── Read initialQuery from navigation state ────────────────────────────

  useEffect(() => {
    const state = location.state as Record<string, unknown> | null;
    const query = state?.initialQuery as string | undefined;
    if (query && conversationId) {
      setInitialQuery(query);
      newFromStartRef.current = conversationId;
      // Clear only initialQuery from state
      window.history.replaceState(
        { ...(location.state as object), initialQuery: undefined },
        "",
      );
    }
  }, [conversationId, location.state]);

  // ── Load agent detail ──────────────────────────────────────────────────

  useEffect(() => {
    if (!agentId) return;

    const controller = new AbortController();

    // Try UUID lookup first
    getAgent(agentId)
      .then((data) => {
        if (!controller.signal.aborted) {
          setAgent(data);
        }
      })
      .catch(async (err) => {
        if (controller.signal.aborted) return;
        // If UUID lookup fails, try finding by name via listAgents
        if (err?.response?.status === 404) {
          try {
            const all = await listAgents();
            const match = all.find((a) => a.name === agentId);
            if (match && !controller.signal.aborted) {
              const detail = await getAgent(match.id);
              if (!controller.signal.aborted) {
                setAgent(detail);
                return;
              }
            }
          } catch {
            // fall through — agent stays null
          }
        }
        // Silently degrade — header will show plain text fallback
      });

    return () => {
      controller.abort();
    };
  }, [agentId]);

  // ── Load conversation list ─────────────────────────────────────────────

  const loadConversations = useCallback(async () => {
    if (!agentId) return;
    setConvListLoading(true);
    setConvListError(null);
    try {
      const active = await listConversations(agentId, "active");
      setConversations(active);
    } catch {
      setConvListError("加载对话列表失败");
    } finally {
      setConvListLoading(false);
    }
  }, [agentId]);

  useEffect(() => {
    loadConversations();
  }, [loadConversations]);

  // ── Load initial snapshot and setup pool ───────────────────────────────

  // Track initial mount to avoid showing loading spinner on conversation switch.
  // `loading` starts as true in useState, so the first mount already shows the
  // spinner. We must NOT set loading=true on subsequent conversation switches,
  // otherwise the header flashes and the UI is replaced by a spinner.
  const initialMountRef = useRef(true);

  useEffect(() => {
    if (!agentId) return;

    // No conversation selected — clear pool, show StartChatPage content
    if (!conversationId) {
      if (initialMountRef.current) {
        setLoading(false);
        initialMountRef.current = false;
      }
      setVisibleConvId(null);
      setChatPool((prev) => {
        if (prev.size === 0) return prev;
        return new Map();
      });
      setSnapshots((prev) => {
        if (prev.size === 0) return prev;
        return new Map();
      });
      setSnapshotReady(true);
      return;
    }

    // Only show full-page spinner on initial mount, not on conversation switches
    if (initialMountRef.current) {
      setLoading(true);
      initialMountRef.current = false;
    }
    setInvalidConv(false);
    setSnapshotReady(false);

    getSessionSnapshot(agentId, conversationId)
      .then((snap) => {
        const msgs = snapshotToMessages(snap.turns);
        setSnapshots((prev) => new Map(prev).set(conversationId, msgs));

        // Add to chat pool
        setChatPool((prev) => {
          const next = new Map(prev);
          if (!next.has(conversationId)) {
            next.set(conversationId, {
              convId: conversationId,
              lastAccessTime: Date.now(),
            });
          }
          return next;
        });

        setVisibleConvId(conversationId);
        setSnapshotReady(true);
        setLoading(false);
      })
      .catch((err: unknown) => {
        // Snapshot load failure means no session history yet — treat as empty.
        // This is normal for newly created conversations. Do NOT treat 404 as
        // "invalid conversation" since the conversation exists in the DB; it
        // just has no session data yet.
        const is404 =
          err &&
          typeof err === "object" &&
          "response" in err &&
          (err as { response?: { status?: number } }).response?.status === 404;
        if (is404) {
          // New conversation with no session yet — proceed with empty snapshot
          setSnapshots((prev) => new Map(prev).set(conversationId, []));
        }
        setSnapshotReady(true);
        setLoading(false);
      });
  }, [agentId, conversationId]);

  // ── Switch conversation ────────────────────────────────────────────────

  const handleSelectConversation = useCallback(
    (convId: string) => {
      if (convId === visibleConvId) return;

      // Update lastAccessTime of current visible
      if (visibleConvId) {
        setChatPool((prev) => {
          const next = new Map(prev);
          const inst = next.get(visibleConvId);
          if (inst) {
            next.set(visibleConvId, { ...inst, lastAccessTime: Date.now() });
          }
          return next;
        });
      }

      // Always add to pool synchronously so ChatView renders immediately —
      // prevents a flash of the "no conversation" placeholder while the
      // snapshot loads asynchronously in the useEffect below.
      setChatPool((prev) => {
        const next = new Map(prev);
        if (!next.has(convId)) {
          // LRU eviction if pool is full (protect the conv we're leaving)
          if (next.size >= MAX_CONCURRENT && visibleConvId) {
            let victimId: string | null = null;
            let minTime = Infinity;
            for (const [id, inst] of next) {
              if (id !== visibleConvId && inst.lastAccessTime < minTime) {
                minTime = inst.lastAccessTime;
                victimId = id;
              }
            }
            if (victimId) {
              next.delete(victimId);
              setSnapshots((prev) => {
                const s = new Map(prev);
                s.delete(victimId!);
                return s;
              });
            }
          }
        }
        next.set(convId, { convId, lastAccessTime: Date.now() });
        return next;
      });

      setVisibleConvId(convId);
      navigate(`/chat/${agentId}/${convId}`);
      // Snapshot loading is handled by the useEffect on [agentId, conversationId]
    },
    [agentId, visibleConvId, navigate],
  );

  // ── Conversation list actions ──────────────────────────────────────────

  const handleNewConversation = useCallback(() => {
    navigate(`/chat/${agentId}`);
  }, [agentId, navigate]);

  const handleRename = useCallback(
    async (convId: string, title: string) => {
      if (!agentId) return;
      try {
        await updateConversation(agentId, convId, { title });
        setConversations((prev) =>
          prev.map((c) => (c.id === convId ? { ...c, title } : c)),
        );
      } catch {
        toast.error("重命名失败");
      }
    },
    [agentId],
  );

  const handleArchive = useCallback(
    async (convId: string) => {
      if (!agentId) return;
      try {
        await updateConversation(agentId, convId, { archive: true });
        setConversations((prev) => prev.filter((c) => c.id !== convId));
        // If archiving current visible conv, switch
        if (convId === visibleConvId) {
          const remaining = conversations.filter((c) => c.id !== convId);
          if (remaining.length > 0) {
            handleSelectConversation(remaining[0].id);
          } else {
            navigate(`/chat/${agentId}`);
          }
        }
        // Remove from pool
        setChatPool((prev) => {
          const next = new Map(prev);
          next.delete(convId);
          return next;
        });
        toast.success("已归档");
      } catch {
        toast.error("归档失败");
      }
    },
    [agentId, visibleConvId, conversations, handleSelectConversation, navigate],
  );

  const handleDelete = useCallback(
    async (convId: string) => {
      if (!agentId) return;
      try {
        await deleteConversationApi(agentId, convId);
        setConversations((prev) => prev.filter((c) => c.id !== convId));
        // If deleting current visible conv
        if (convId === visibleConvId) {
          setChatPool((prev) => {
            const next = new Map(prev);
            next.delete(convId);
            return next;
          });
          // Find the next most recent conversation in the remaining pool
          const remaining = conversations.filter((c) => c.id !== convId);
          const poolRemaining = Array.from(chatPool.keys()).filter(
            (id) => id !== convId,
          );
          if (poolRemaining.length > 0) {
            handleSelectConversation(poolRemaining[0]);
          } else if (remaining.length > 0) {
            handleSelectConversation(remaining[0].id);
          } else {
            navigate(`/chat/${agentId}`);
          }
        } else {
          setChatPool((prev) => {
            const next = new Map(prev);
            next.delete(convId);
            return next;
          });
        }
        toast.success("已删除");
      } catch {
        toast.error("删除失败");
      }
    },
    [agentId, visibleConvId, conversations, chatPool, handleSelectConversation, navigate],
  );

  const handleRetryConversations = useCallback(() => {
    loadConversations();
  }, [loadConversations]);

  const handleToggleArchived = useCallback(async () => {
    if (!agentId) return;
    if (!archivedExpanded) {
      setArchivedExpanded(true);
      setArchivedLoading(true);
      try {
        const archived = await listConversations(agentId, "archived");
        setArchivedConversations(archived);
      } catch {
        // ignore
      } finally {
        setArchivedLoading(false);
      }
    } else {
      setArchivedExpanded(false);
    }
  }, [agentId, archivedExpanded]);

  // Resolved agent ID: prefer UUID from state, fallback to URL param (agent name)
  const resolvedAgentId = agentId || "";

  // ── Send (new conversation) handler ──────────────────────────────────

  const handleSend = useCallback(async () => {
    const query = input.trim();
    if (!query || !resolvedAgentId || sending) return;
    setSending(true);
    try {
      const conv = await createConversation(resolvedAgentId, query.slice(0, 15));
      setConversations((prev) => [conv, ...prev]);
      setInput("");
      setSending(false);
      navigate(`/chat/${encodeURIComponent(resolvedAgentId)}/${conv.id}`, {
        state: { initialQuery: query },
        replace: true,
      });
    } catch {
      toast.error("创建对话失败，请重试");
      setSending(false);
    }
  }, [input, resolvedAgentId, sending, navigate]);

  const handleInputKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        handleSend();
      }
    },
    [handleSend],
  );

  // ── Unread handler ─────────────────────────────────────────────────────

  const handleUnread = useCallback(
    (convId: string, count: number) => {
      setUnreadCounts((prev) => {
        const next = new Map(prev);
        next.set(convId, count);
        return next;
      });
    },
    [],
  );

  if (loading) return <LoadingSpinner />;

  // ── Render: invalid conversation ───────────────────────────────────────

  if (invalidConv) {
    return (
      <div className="flex items-center justify-center h-full">
        <div className="flex flex-col items-center gap-4 text-center">
          <AlertCircle className="h-10 w-10 text-muted-foreground" />
          <p className="text-lg text-muted-foreground">对话不存在或已被删除</p>
          <Button variant="outline" onClick={() => navigate(`/chat/${resolvedAgentId}`)}>
            返回开始页
          </Button>
        </div>
      </div>
    );
  }

  // ── Render: dual-pane ──────────────────────────────────────────────────

  const showPool = Array.from(chatPool.keys());

  return (
    <div className="flex h-full relative">
      {/* Mobile hamburger */}
      <div className="md:hidden absolute top-2 left-2 z-10">
        <Button
          variant="ghost"
          size="icon"
          onClick={() => setMobilePanelOpen(!mobilePanelOpen)}
        >
          <span className="text-lg">☰</span>
        </Button>
      </div>

      {/* Left panel — minimal: back + agent icon + conversation list */}
      <div
        className={`${
          mobilePanelOpen ? "block" : "hidden"
        } md:flex md:flex-col w-[280px] lg:w-[280px] md:w-[220px] shrink-0 border-r bg-muted`}
      >
        {/* Header: back button left, icon + name centered */}
        <div className="relative flex items-center justify-center gap-2 px-3 py-2 border-b">
          <Button
            variant="ghost"
            size="icon"
            className="absolute left-1 h-7 w-7 shrink-0"
            onClick={() => navigate("/workspace/agents")}
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div
            className="w-6 h-6 rounded-full flex items-center justify-center text-xs shrink-0"
            style={{
              backgroundColor: agent?.background_color || "#6366f1",
            }}
          >
            {agent?.icon || "🤖"}
          </div>
          <span className="font-semibold text-sm truncate">
            {agent?.name || agentId || ""}
          </span>
        </div>

        {/* Conversation list */}
        <div className="flex-1 min-h-0">
          <ConversationList
            conversations={conversations}
            activeConvId={visibleConvId}
            unreadCounts={unreadCounts}
            loading={convListLoading}
            error={convListError}
            onSelectConversation={(convId) => {
              handleSelectConversation(convId);
              setMobilePanelOpen(false);
            }}
            onNewConversation={handleNewConversation}
            onRename={handleRename}
            onArchive={handleArchive}
            onDelete={handleDelete}
            onRetry={handleRetryConversations}
            archivedConversations={archivedConversations}
            archivedLoading={archivedLoading}
            archivedExpanded={archivedExpanded}
            onToggleArchived={handleToggleArchived}
          />
        </div>
      </div>

      {/* Right panel */}
      <div className="flex-1 relative">
        {/* No conversation selected — StartChatPage style */}
        {showPool.length === 0 && (
          <div className="flex items-center justify-center h-full">
            <div className="flex flex-col items-center gap-4 w-full max-w-xl px-4">
              <div
                className="w-16 h-16 rounded-full flex items-center justify-center text-2xl shrink-0"
                style={{ backgroundColor: agent?.background_color || "#6366f1" }}
              >
                {agent?.icon || "🤖"}
              </div>
              <h1 className="text-xl font-semibold">{agent?.name}</h1>
              {agent?.description && (
                <p className="text-sm text-muted-foreground text-center max-w-md">
                  {agent.description}
                </p>
              )}
              <div className="flex gap-2 w-full mt-4">
                <Input
                  className="flex-1"
                  placeholder="输入你的问题…"
                  value={input}
                  onChange={(e) => setInput(e.target.value)}
                  onKeyDown={handleInputKeyDown}
                  disabled={sending}
                  autoFocus
                />
                <Button onClick={handleSend} disabled={!input.trim() || sending}>
                  {sending ? (
                    "创建中…"
                  ) : (
                    <>
                      <Send className="h-4 w-4 mr-1" />
                      发送
                    </>
                  )}
                </Button>
              </div>
            </div>
          </div>
        )}
        {showPool.map((convId) => (
          <div
            key={convId}
            style={{
              display: convId === visibleConvId ? undefined : "none",
              height: "100%",
            }}
          >
            <ChatView
              visible={convId === visibleConvId}
              className="h-full"
              streamUrl={(msg) =>
                `/api/chat/${encodeURIComponent(agentId ?? "")}/conversations/${convId}/stream?message=${encodeURIComponent(msg)}`
              }
              initialMessages={snapshots.get(convId) ?? EMPTY_MESSAGES}
              initialQuery={
                snapshotReady && convId === newFromStartRef.current
                  ? initialQuery
                  : undefined
              }
              onInitialQuerySent={() => {
                setInitialQuery(undefined);
                newFromStartRef.current = null;
              }}
              onUnread={(count) => handleUnread(convId, count)}
              headerTitle={
                agent ? (
                  <div className="flex items-center gap-2">
                    <div
                      className="w-6 h-6 rounded-full flex items-center justify-center text-xs shrink-0"
                      style={{
                        backgroundColor: agent.background_color || "#6366f1",
                      }}
                    >
                      {agent.icon || "🤖"}
                    </div>
                    <span className="font-semibold">{agent.name}</span>
                  </div>
                ) : (
                  agentId ?? "对话"
                )
              }
            />
          </div>
        ))}
      </div>
    </div>
  );
}
