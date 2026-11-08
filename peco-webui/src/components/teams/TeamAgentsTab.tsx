// agents tab：`@master` 置顶，其余按名称。列表是装配的入口 —— 新建、选一个进来改。
// 只在团队有已打开空间时可用：请求头 `X-Peco-Space` 决定读哪个团队的能力根。

import { useState } from "react";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { EmptyState } from "@/components/common/EmptyState";
import { AgentEditor } from "@/components/teams/AgentEditor";
import { NewAgentDialog } from "@/components/teams/NewAgentDialog";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useTeamAgents } from "@/queries/teamDetail";
import { errorText } from "@/lib/api";
import { cn } from "@/lib/utils";
import type { AgentListItem } from "@/types/daemon";

function sortAgents(agents: AgentListItem[]): AgentListItem[] {
  return [...agents].sort((a, b) =>
    a.is_master === b.is_master
      ? a.name.localeCompare(b.name)
      : a.is_master
        ? -1
        : 1,
  );
}

export function TeamAgentsTab({ wsId }: { wsId: string }) {
  const agents = useTeamAgents(wsId);
  const [selected, setSelected] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);

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
  // 选中项未必已在列表里（刚建好的 agent 要等一次重新拉取），编辑器按 id 照样能开。
  const activeId = selected ?? rows[0]?.id ?? null;
  const isMaster = rows.find((row) => row.id === activeId)?.is_master ?? false;

  return (
    <div className="flex h-full min-h-0 flex-col @min-[900px]/work:flex-row">
      <div className="shrink-0 border-line @min-[900px]/work:w-64 @min-[900px]/work:border-r">
        {/* 空列表时新建入口只在右栏出现一次，不摆两个同名按钮。 */}
        {rows.length > 0 && (
          <div className="flex h-9 items-center border-b border-line px-2">
            <Button
              size="sm"
              variant="ghost"
              className="h-7 px-2 text-xs"
              onClick={() => setCreating(true)}
            >
              ＋ 新建 agent
            </Button>
          </div>
        )}
        <ul>
          {rows.map((agent) => (
            <li key={agent.id}>
              <button
                type="button"
                onClick={() => setSelected(agent.id)}
                className={cn(
                  "flex h-9 w-full items-center gap-2 border-b border-line px-3 text-left",
                  agent.id === activeId ? "bg-line/40" : "hover:bg-line/20",
                )}
              >
                <span className="min-w-0 flex-1 truncate font-mono text-sm text-ink">
                  {agent.name}
                </span>
                {agent.is_master && (
                  <span className="shrink-0 rounded-[4px] border border-line px-1 text-2xs text-ink-faint">
                    总管
                  </span>
                )}
              </button>
            </li>
          ))}
        </ul>
      </div>

      {activeId ? (
        <AgentEditor
          // 换 agent 就重挂：上一个的草稿不留到下一个身上。
          key={activeId}
          wsId={wsId}
          agentId={activeId}
          isMaster={isMaster}
          onDeleted={() => setSelected(null)}
        />
      ) : (
        <div className="p-3">
          <EmptyState
            title="这个团队还没有 agent。"
            action={
              <Button
                size="sm"
                variant="outline"
                className="h-7"
                onClick={() => setCreating(true)}
              >
                ＋ 新建 agent
              </Button>
            }
          />
        </div>
      )}

      <NewAgentDialog
        wsId={wsId}
        open={creating}
        onOpenChange={setCreating}
        onCreated={setSelected}
      />
    </div>
  );
}
