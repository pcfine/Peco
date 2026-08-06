// StartChatPage — 双栏布局：左侧对话历史 + 右侧居中开始新对话界面

import { useEffect, useState, useCallback, useRef } from "react";
import { useParams, useNavigate } from "react-router-dom";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { ConversationList } from "@/components/chat/ConversationList";
import { getAgent, listAgents } from "@/api/agents";
import {
  createConversation,
  listConversations,
  updateConversation,
  deleteConversation as deleteConversationApi,
} from "@/api/conversations";
import { Send, ArrowLeft } from "lucide-react";
import { AgentIcon } from "@/components/common/AgentIcon";
import { toast } from "sonner";
import type { AgentDetail } from "@/types/agent";
import type { Conversation } from "@/types/chat";

// ── Title generation ──────────────────────────────────────────────────────

function generateTitle(message: string): string {
  const trimmed = message.trim();
  if (!trimmed) return "新对话";
  const prefix = trimmed.slice(0, 15);
  return prefix.length < trimmed.length ? prefix + "…" : prefix;
}

// ── Component ─────────────────────────────────────────────────────────────

export function StartChatPage() {
  const { agentId } = useParams<{ agentId: string }>();
  const navigate = useNavigate();

  // ── Agent state ─────────────────────────────────────────────────────────

  const [agent, setAgent] = useState<AgentDetail | null>(null);
  const [agentLoading, setAgentLoading] = useState(true);
  const [agentError, setAgentError] = useState<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  // ── Conversation list state ─────────────────────────────────────────────

  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [convListLoading, setConvListLoading] = useState(true);
  const [convListError, setConvListError] = useState<string | null>(null);
  const [archivedConversations, setArchivedConversations] = useState<
    Conversation[]
  >([]);
  const [archivedExpanded, setArchivedExpanded] = useState(false);
  const [archivedLoading, setArchivedLoading] = useState(false);

  // ── Input state ─────────────────────────────────────────────────────────

  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);

  // ── Mobile ──────────────────────────────────────────────────────────────

  const [mobilePanelOpen, setMobilePanelOpen] = useState(false);

  // ── Load agent info (try UUID first, fallback to name lookup) ──────────

  useEffect(() => {
    if (!agentId) return;

    const controller = new AbortController();
    abortRef.current = controller;

    setAgentLoading(true);
    setAgentError(null);

    // Try UUID lookup first
    getAgent(agentId)
      .then((data) => {
        if (!controller.signal.aborted) {
          setAgent(data);
          setAgentLoading(false);
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
                setAgentLoading(false);
                return;
              }
            }
          } catch {
            // fall through to error
          }
          if (!controller.signal.aborted) {
            setAgentError("Agent 未找到");
          }
        } else if (!controller.signal.aborted) {
          setAgentError("加载失败，请检查网络连接");
        }
        if (!controller.signal.aborted) {
          setAgentLoading(false);
        }
      });

    return () => {
      controller.abort();
    };
  }, [agentId]);

  // ── Load conversation list (requires agent name) ────────────────────────

  const loadConversations = useCallback(async () => {
    if (!agent) return;
    setConvListLoading(true);
    setConvListError(null);
    try {
      const active = await listConversations(agent.name, "active");
      setConversations(active);
    } catch {
      setConvListError("加载对话列表失败");
    } finally {
      setConvListLoading(false);
    }
  }, [agent]);

  useEffect(() => {
    if (agent) {
      loadConversations();
    }
  }, [agent, loadConversations]);

  // ── Send handler ────────────────────────────────────────────────────────

  const handleSend = async () => {
    const query = input.trim();
    if (!query || !agent || sending) return;

    setSending(true);
    try {
      const conv = await createConversation(agent.name, generateTitle(query));
      navigate(`/chat/${encodeURIComponent(agent.name)}/${conv.id}`, {
        state: { initialQuery: query },
        replace: true,
      });
    } catch {
      toast.error("创建对话失败，请重试");
      setSending(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  // ── Conversation list handlers ──────────────────────────────────────────

  const handleSelectConversation = useCallback(
    (convId: string) => {
      if (!agent) return;
      navigate(`/chat/${encodeURIComponent(agent.name)}/${convId}`);
    },
    [agent, navigate],
  );

  const handleNewConversation = useCallback(() => {
    if (!agent) return;
    navigate(`/chat/${encodeURIComponent(agent.name)}`);
  }, [agent, navigate]);

  const handleRename = useCallback(
    async (convId: string, title: string) => {
      if (!agent) return;
      try {
        await updateConversation(agent.name, convId, { title });
        setConversations((prev) =>
          prev.map((c) => (c.id === convId ? { ...c, title } : c)),
        );
      } catch {
        toast.error("重命名失败");
      }
    },
    [agent],
  );

  const handleArchive = useCallback(
    async (convId: string) => {
      if (!agent) return;
      try {
        await updateConversation(agent.name, convId, { archive: true });
        setConversations((prev) => prev.filter((c) => c.id !== convId));
        toast.success("已归档");
      } catch {
        toast.error("归档失败");
      }
    },
    [agent],
  );

  const handleDelete = useCallback(
    async (convId: string) => {
      if (!agent) return;
      try {
        await deleteConversationApi(agent.name, convId);
        setConversations((prev) => prev.filter((c) => c.id !== convId));
        toast.success("已删除");
      } catch {
        toast.error("删除失败");
      }
    },
    [agent],
  );

  const handleRetryConversations = useCallback(() => {
    loadConversations();
  }, [loadConversations]);

  const handleToggleArchived = useCallback(async () => {
    if (!agent) return;
    if (!archivedExpanded) {
      setArchivedExpanded(true);
      setArchivedLoading(true);
      try {
        const archived = await listConversations(agent.name, "archived");
        setArchivedConversations(archived);
      } catch {
        // ignore
      } finally {
        setArchivedLoading(false);
      }
    } else {
      setArchivedExpanded(false);
    }
  }, [agent, archivedExpanded]);

  // ── Render: full-page loading ───────────────────────────────────────────

  if (agentLoading) {
    return (
      <div className="flex items-center justify-center h-full">
        <div className="flex flex-col items-center gap-4 w-full max-w-xl">
          <Skeleton className="w-16 h-16 rounded-full" />
          <Skeleton className="h-6 w-40" />
          <Skeleton className="h-4 w-64" />
          <Skeleton className="h-10 w-full mt-4" />
        </div>
      </div>
    );
  }

  // ── Render: agent error ─────────────────────────────────────────────────

  if (agentError) {
    return (
      <div className="flex items-center justify-center h-full">
        <div className="flex flex-col items-center gap-4 text-center max-w-md">
          <p className="text-lg text-muted-foreground">{agentError}</p>
          {agentError === "Agent 未找到" ? (
            <Button
              variant="outline"
              onClick={() => navigate("/workspace/agents")}
            >
              <ArrowLeft className="h-4 w-4 mr-2" />
              返回 Agent 列表
            </Button>
          ) : (
            <Button
              variant="outline"
              onClick={() => {
                setAgentLoading(true);
                setAgentError(null);
              }}
            >
              重试
            </Button>
          )}
        </div>
      </div>
    );
  }

  // ── Render: dual-pane layout ────────────────────────────────────────────

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

      {/* Left panel: ConversationList */}
      <div
        className={`${
          mobilePanelOpen ? "block" : "hidden"
        } md:block w-[280px] lg:w-[280px] md:w-[220px] shrink-0`}
      >
        <ConversationList
          conversations={conversations}
          activeConvId={null}
          unreadCounts={new Map()}
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

      {/* Right panel: centered start-chat area */}
      <div className="flex-1 flex items-center justify-center">
        <div className="flex flex-col items-center gap-4 w-full max-w-xl px-4">
          {/* Agent info */}
          <AgentIcon
            icon={agent?.icon || "🤖"}
            backgroundColor={agent?.background_color}
            size="lg"
          />
          <h1 className="text-xl font-semibold">{agent?.name}</h1>
          {agent?.description && (
            <p className="text-sm text-muted-foreground text-center max-w-md">
              {agent.description}
            </p>
          )}

          {/* Input */}
          <div className="flex gap-2 w-full mt-4">
            <input
              className="flex-1 rounded-md border px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-primary bg-background"
              placeholder="输入你的问题..."
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={handleKeyDown}
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
    </div>
  );
}
