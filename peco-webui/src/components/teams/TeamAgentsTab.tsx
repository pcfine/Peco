// agents tab：卡片列表 → 点卡进同一个位置的编辑器。列表是装配入口 —— 新建、改、删。
// 只在团队有已打开空间时可用：请求头 `X-Peco-Space` 决定读哪个团队的能力根。

import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, Bot, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { CardGrid, EntityCard } from "@/components/common/CardGrid";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { AgentEditor } from "@/components/teams/AgentEditor";
import { NewAgentDialog } from "@/components/teams/NewAgentDialog";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { deleteAgent } from "@/api/agents";
import { errorText } from "@/lib/api";
import { cn } from "@/lib/utils";
import { useTeamAgents } from "@/queries/teamDetail";
import type { AgentListItem } from "@/types/daemon";

/** 元信息行里最多摆几个 chip，其余折成 `+N`。 */
const META_CHIPS = 2;

function sortAgents(agents: AgentListItem[]): AgentListItem[] {
  return [...agents].sort((a, b) =>
    a.is_master === b.is_master
      ? a.name.localeCompare(b.name)
      : a.is_master
        ? -1
        : 1,
  );
}

/** 删除确认只认 id 与名字：卡与编辑器两处入口都用得上，别处不必知道更多。 */
interface DeleteTarget {
  id: string;
  name: string;
}

function MetaChip({ children }: { children: string }) {
  return (
    <span className="rounded bg-line/60 px-1 font-mono text-caption text-ink-muted">
      {children}
    </span>
  );
}

/**
 * 卡左侧的色块：`icon` 是可填的短图标串（emoji 之类），塞不下就退到名字首字，
 * 都没有时给一个通用图标 —— 空方块比「猜错了」更难认。
 */
function AgentMark({ agent }: { agent: AgentListItem }) {
  const icon = agent.icon.trim();
  const glyph =
    icon && Array.from(icon).length <= 4
      ? icon
      : Array.from(agent.name.trim())[0];

  return (
    <span
      aria-hidden
      className={cn(
        "flex size-14 shrink-0 items-center justify-center rounded-[6px] text-xl",
        !agent.background_color && "bg-line/40",
      )}
      style={
        agent.background_color
          ? { backgroundColor: agent.background_color }
          : undefined
      }
    >
      {glyph ?? <Bot className="size-5" />}
    </span>
  );
}

interface AgentCardProps {
  agent: AgentListItem;
  onOpen: () => void;
  onDelete: () => void;
}

function AgentCard({ agent, onOpen, onDelete }: AgentCardProps) {
  return (
    <EntityCard
      onClick={onOpen}
      actions={
        agent.is_master ? null : (
          <Button
            variant="ghost"
            size="icon-xs"
            className="text-ink-faint hover:text-danger"
            aria-label={`删除 ${agent.name}`}
            onClick={onDelete}
          >
            <Trash2 />
          </Button>
        )
      }
    >
      <div className="flex min-w-0 items-start gap-2">
        <AgentMark agent={agent} />
        <div className="min-w-0 flex-1 space-y-1">
          {/* 右上角留给操作按钮，名字行不铺到那里。 */}
          <div className="flex min-w-0 items-center gap-1 pr-6">
            <span className="truncate text-sm font-semibold text-ink">
              {agent.name}
            </span>
            {agent.is_master && (
              <span className="shrink-0 rounded-[4px] bg-line/60 px-1 text-2xs font-medium text-ink-faint">
                @master
              </span>
            )}
          </div>
          {agent.description && (
            <p className="line-clamp-2 text-caption text-ink-muted">
              {agent.description}
            </p>
          )}
        </div>
      </div>
      <div className="mt-auto flex min-w-0 flex-wrap items-center gap-1">
        {agent.model ? (
          <span className="truncate font-mono text-2xs text-ink-faint">
            {agent.model}
          </span>
        ) : (
          <span className="text-caption text-ink-faint">未设模型</span>
        )}
        {agent.tools.slice(0, META_CHIPS).map((tool) => (
          <MetaChip key={tool}>{tool}</MetaChip>
        ))}
        {agent.tools.length > META_CHIPS && (
          <MetaChip>{`+${agent.tools.length - META_CHIPS}`}</MetaChip>
        )}
        {agent.knowledge_bases.slice(0, META_CHIPS).map((kb) => (
          <MetaChip key={kb}>{kb}</MetaChip>
        ))}
        {agent.knowledge_bases.length > META_CHIPS && (
          <MetaChip>{`+${agent.knowledge_bases.length - META_CHIPS}`}</MetaChip>
        )}
      </div>
    </EntityCard>
  );
}

interface TeamAgentsTabProps {
  /** `X-Peco-Space`：哪个已打开空间的能力根。 */
  wsId: string;
  /** provider 配置按团队取，取这个名字下的 providers.toml。 */
  teamId: string;
  /** 团队没有 `@master` 时派发不了任务，列表顶部要把这句话说出来。 */
  noMaster?: boolean;
}

export function TeamAgentsTab({
  wsId,
  teamId,
  noMaster = false,
}: TeamAgentsTabProps) {
  const queryClient = useQueryClient();
  const agents = useTeamAgents(wsId);
  const [selected, setSelected] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<DeleteTarget | null>(null);

  // 删掉一个 agent 也会改团队里的计数：列表与团队详情都要重取。
  const remove = useMutation({
    mutationFn: (target: DeleteTarget) => deleteAgent(wsId, target.id),
    onSuccess: (_res, target) => {
      queryClient.removeQueries({ queryKey: ["agent", wsId, target.id] });
      void queryClient.invalidateQueries({ queryKey: ["agents", wsId] });
      void queryClient.invalidateQueries({ queryKey: ["team"] });
      setPendingDelete(null);
      // 删掉的正是编辑器里那个：回到列表，不留一个指向空 agent 的表单。
      setSelected((current) => (current === target.id ? null : current));
      toast.success("已删除");
    },
    onError: (error) => toast.error(errorText(error)),
  });

  if (agents.error) {
    return (
      <div className="p-3">
        <ErrorBanner
          message={errorText(agents.error)}
          onRetry={() => void agents.refetch()}
        />
      </div>
    );
  }
  if (agents.isPending) {
    return (
      <div className="space-y-px p-3">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-9 w-full" />
        ))}
      </div>
    );
  }

  const rows = sortAgents(agents.data ?? []);
  // 刚建好的 agent 要等一次重新拉取才进列表：这段时间里标题回落成 id，编辑器照样能开。
  const active = rows.find((row) => row.id === selected) ?? null;
  const activeName = active?.name ?? selected ?? "";

  const confirmDialog = (
    <Dialog
      open={pendingDelete !== null}
      onOpenChange={(open) => {
        if (!open) {
          setPendingDelete(null);
          remove.reset();
        }
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>删除 agent「{pendingDelete?.name}」？</DialogTitle>
          <DialogDescription>
            连同它的 agent.md 一起删掉，不可恢复。
          </DialogDescription>
        </DialogHeader>
        {remove.error && (
          <p className="text-xs text-danger">{errorText(remove.error)}</p>
        )}
        <DialogFooter>
          <Button
            variant="ghost"
            size="sm"
            disabled={remove.isPending}
            onClick={() => setPendingDelete(null)}
          >
            取消
          </Button>
          <Button
            variant="destructive"
            size="sm"
            disabled={remove.isPending}
            onClick={() => {
              if (pendingDelete !== null) remove.mutate(pendingDelete);
            }}
          >
            确认删除
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );

  if (selected !== null) {
    return (
      <div className="flex h-full min-h-0 flex-col">
        <div className="flex h-9 shrink-0 items-center gap-2 border-b border-line px-2">
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label="返回列表"
            onClick={() => setSelected(null)}
          >
            <ArrowLeft />
          </Button>
          <span className="min-w-0 flex-1 truncate font-mono text-sm text-ink">
            {activeName}
          </span>
        </div>
        <AgentEditor
          // 换 agent 就重挂：上一个的草稿不留到下一个身上。
          key={selected}
          wsId={wsId}
          teamId={teamId}
          agentId={selected}
          isMaster={active?.is_master ?? false}
          onRequestDelete={() =>
            setPendingDelete({ id: selected, name: activeName })
          }
        />
        {confirmDialog}
      </div>
    );
  }

  return (
    <div className="p-3">
      {rows.length > 0 && (
        <div className="mb-3 flex items-center gap-2">
          <span className="text-sm font-medium text-ink">Agent</span>
          <span className="text-caption text-ink-faint">{rows.length}</span>
          <Button
            size="sm"
            className="ml-auto"
            onClick={() => setCreating(true)}
          >
            ＋ 新建 agent
          </Button>
        </div>
      )}
      {noMaster && (
        <p className="mb-3 border-l-2 border-danger pl-2 text-xs text-danger">
          这个团队没有 @master，Peco 无法向它派发任务。
        </p>
      )}
      {rows.length === 0 ? (
        <EmptyState
          title="这个团队还没有 agent。"
          action={
            <Button size="sm" onClick={() => setCreating(true)}>
              ＋ 新建 agent
            </Button>
          }
        />
      ) : (
        <CardGrid>
          {rows.map((agent) => (
            <AgentCard
              key={agent.id}
              agent={agent}
              onOpen={() => setSelected(agent.id)}
              onDelete={() =>
                setPendingDelete({ id: agent.id, name: agent.name })
              }
            />
          ))}
        </CardGrid>
      )}

      <NewAgentDialog
        wsId={wsId}
        teamId={teamId}
        open={creating}
        onOpenChange={setCreating}
        onCreated={setSelected}
      />
      {confirmDialog}
    </div>
  );
}
