// agents tab：卡片列表 → 点卡进同一个位置的编辑器。列表是装配入口 —— 新建、改、删。
// 读写的是团队能力根：`scope` 决定请求认哪个团队，没有空间的团队照样能用。

import { useState, type CSSProperties } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Trash2 } from "lucide-react";
import { toast } from "sonner";
import { BackButton } from "@/components/common/BackButton";
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
import {
  agentDescriptionDetail,
  agentDisplayName,
  agentRoleName,
  firstGrapheme,
} from "@/lib/agentName";
import { errorText, type ResourceScope } from "@/lib/api";
import { seedHue } from "@/lib/identicon";
import { useTeamAgents } from "@/queries/teamDetail";
import type { AgentListItem } from "@/types/daemon";

function agentLabel(agent: AgentListItem): string {
  return agentDisplayName(agent.description, agent.name);
}

function sortAgents(agents: AgentListItem[]): AgentListItem[] {
  return [...agents].sort((a, b) => {
    if (a.is_master !== b.is_master) return a.is_master ? -1 : 1;
    return (
      agentLabel(a).localeCompare(agentLabel(b), "zh") ||
      a.id.localeCompare(b.id)
    );
  });
}

/** 删除确认只认 id 与展示名：卡与编辑器两处入口都用得上，别处不必知道更多。 */
interface DeleteTarget {
  id: string;
  label: string;
}

/**
 * 卡左侧的首字标识：色相由 agent id 派生（同 id 永远同色），首字取展示名。
 * 不再使用图标字段 —— 用户填的 emoji 辨识度不如「名字本身」。
 */
function AgentMonogram({ agent }: { agent: AgentListItem }) {
  return (
    <span
      aria-hidden
      className="agent-monogram -my-3 -ml-3 flex w-14 shrink-0 items-center justify-center self-stretch rounded-l-md text-2xl font-semibold"
      style={{ "--mono-hue": String(seedHue(agent.id)) } as CSSProperties}
    >
      {firstGrapheme(agentLabel(agent)) || "?"}
    </span>
  );
}

interface AgentCardProps {
  agent: AgentListItem;
  onOpen: () => void;
  onDelete: () => void;
}

function AgentCard({ agent, onOpen, onDelete }: AgentCardProps) {
  const label = agentLabel(agent);
  const detail = agentDescriptionDetail(agent.description);
  // 前端热更新可能先于 daemon 重启到达：旧接口没有这两个字段，缺值时按 0 计，
  // 不能让一个 undefined.length 把整页打成白屏。
  const toolCount = agent.tools?.length ?? 0;
  const mcpCount = agent.mcp_servers?.length ?? 0;
  const skillCount = agent.skills?.length ?? 0;
  // 标题回退成 id 说明名称没落在窗口里，副行改放角色名；两条分支都为的是同一个
  // 字符串不在卡上出现两次。
  const fallsBackToId = label === agent.name;
  const roleName = agentRoleName(agent.description);
  const secondary = fallsBackToId ? roleName : agent.name;

  return (
    <EntityCard
      onClick={onOpen}
      className="h-full min-h-[104px]"
      actions={
        agent.is_master ? null : (
          <Button
            variant="ghost"
            size="icon-xs"
            className="text-ink-faint hover:text-danger"
            aria-label={`删除 ${label}`}
            onClick={onDelete}
          >
            <Trash2 />
          </Button>
        )
      }
    >
      <div className="flex min-w-0 flex-1 items-stretch gap-3">
        <AgentMonogram agent={agent} />
        {/* 四行都只占一行高：同组卡等高靠这里，行内换行会把卡撑成不同高度。 */}
        <div className="flex min-w-0 flex-1 flex-col">
          {/* 右上角留给操作按钮，名字行不铺到那里。 */}
          <div className="flex min-w-0 items-center pr-6">
            <span className="truncate text-base font-semibold text-ink">
              {label}
            </span>
          </div>
          <p
            className={
              fallsBackToId
                ? "mt-0.5 truncate text-2xs text-ink-faint"
                : "mt-0.5 truncate font-mono text-2xs text-ink-faint"
            }
            title={secondary || undefined}
          >
            {secondary || "\u00a0"}
          </p>
          <p className="mt-1 truncate text-caption text-ink-muted">
            {detail || "\u00a0"}
          </p>
          {/* 计数压到卡底：短描述时也不留一截空白。 */}
          <p className="mt-auto flex items-center gap-x-3 overflow-hidden pt-2 font-mono text-2xs text-ink-faint">
            <span className="whitespace-nowrap">tools: {toolCount}</span>
            <span className="whitespace-nowrap">mcp: {mcpCount}</span>
            <span className="whitespace-nowrap">skill: {skillCount}</span>
          </p>
        </div>
      </div>
    </EntityCard>
  );
}

interface TeamAgentsTabProps {
  /** 能力根的落点：有已打开空间时借道空间，否则按团队寻址。 */
  scope: ResourceScope;
  /** provider 配置按团队取，取这个名字下的 providers.toml。 */
  teamId: string;
  /** 团队没有 `@master` 时派发不了任务，列表顶部要把这句话说出来。 */
  noMaster?: boolean;
}

export function TeamAgentsTab({
  scope,
  teamId,
  noMaster = false,
}: TeamAgentsTabProps) {
  const queryClient = useQueryClient();
  const agents = useTeamAgents(scope);
  const [selected, setSelected] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<DeleteTarget | null>(null);

  // 删掉一个 agent 也会改团队里的计数：列表与团队详情都要重取。
  const remove = useMutation({
    mutationFn: (target: DeleteTarget) => deleteAgent(scope, target.id),
    onSuccess: (_res, target) => {
      queryClient.removeQueries({
        queryKey: ["agent", scope.teamId, scope.wsId, target.id],
      });
      void queryClient.invalidateQueries({
        queryKey: ["agents", scope.teamId, scope.wsId],
      });
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
  const activeLabel = active ? agentLabel(active) : (selected ?? "");

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
          <DialogTitle>删除 agent「{pendingDelete?.label}」？</DialogTitle>
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
        {/* 与上一级 TeamConfigPanel 的侧栏头同形：44px 高、三栏等宽栅格，
            返回键与标题都靠 items-center 落在同一条中线上。 */}
        <div className="flex h-11 shrink-0 items-center border-b border-line px-3">
          <div className="grid w-full grid-cols-[28px_1fr_28px] items-center">
            <BackButton label="返回列表" onClick={() => setSelected(null)} />
            <h1 className="min-w-0 truncate text-center text-sm font-semibold text-ink">
              {activeLabel}
            </h1>
            <span aria-hidden />
          </div>
        </div>
        <AgentEditor
          // 换 agent 就重挂：上一个的草稿不留到下一个身上。
          key={selected}
          scope={scope}
          teamId={teamId}
          agentId={selected}
          isMaster={active?.is_master ?? false}
          onRequestDelete={() =>
            setPendingDelete({ id: selected, label: activeLabel })
          }
        />
        {confirmDialog}
      </div>
    );
  }

  return (
    <div className="p-3">
      {rows.length > 0 && (
        <div className="mb-3">
          <Button size="sm" onClick={() => setCreating(true)}>
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
          title="这个团队还没有 agent"
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
                setPendingDelete({ id: agent.id, label: agentLabel(agent) })
              }
            />
          ))}
        </CardGrid>
      )}

      <NewAgentDialog
        scope={scope}
        teamId={teamId}
        open={creating}
        onOpenChange={setCreating}
        onCreated={setSelected}
      />
      {confirmDialog}
    </div>
  );
}
