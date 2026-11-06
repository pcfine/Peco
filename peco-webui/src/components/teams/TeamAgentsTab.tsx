// agents tab：`@master` 置顶，其余按名称。正文可直接改 —— 但只在团队有已打开
// 空间时可用：请求头 `X-Peco-Space` 决定读哪个团队的能力根，没有它就无从写起。

import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { updateAgentPrompt } from "@/api/agents";
import { useAgentDetail, useTeamAgents } from "@/queries/teamDetail";
import { errorText } from "@/lib/api";
import { cn } from "@/lib/utils";

function AgentEditor({ wsId, agentId }: { wsId: string; agentId: string }) {
  const queryClient = useQueryClient();
  const detail = useAgentDetail(wsId, agentId);
  const [draft, setDraft] = useState<string | null>(null);

  const save = useMutation({
    mutationFn: (prompt: string) => updateAgentPrompt(wsId, agentId, prompt),
    onSuccess: (updated) => {
      setDraft(null);
      queryClient.setQueryData(["agent", wsId, agentId], updated);
      toast.success("正文已保存");
    },
    onError: (error) => toast.error(errorText(error)),
  });

  if (detail.error) {
    return (
      <div className="p-3">
        <ErrorBanner
          message={errorText(detail.error)}
          onRetry={() => void detail.refetch()}
        />
      </div>
    );
  }
  if (detail.isPending || !detail.data) {
    return (
      <div className="space-y-px p-3">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-8 w-full" />
        ))}
      </div>
    );
  }

  const value = draft ?? detail.data.system_prompt;
  const dirty = draft !== null && draft !== detail.data.system_prompt;

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2 p-3">
      <p className="max-w-[640px] text-2xs text-ink-faint">
        {detail.data.name} · {detail.data.description || "没有描述"}
      </p>
      <Textarea
        value={value}
        onChange={(event) => setDraft(event.target.value)}
        className="min-h-[280px] flex-1 font-mono text-mono-sm"
        aria-label={`${detail.data.name} 的正文`}
      />
      <div className="flex items-center gap-2">
        <Button
          size="sm"
          disabled={!dirty || save.isPending}
          onClick={() => save.mutate(value)}
        >
          保存正文
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={!dirty || save.isPending}
          onClick={() => setDraft(null)}
        >
          放弃修改
        </Button>
        {dirty && <span className="text-2xs text-warn">未保存</span>}
      </div>
    </div>
  );
}

export function TeamAgentsTab({ wsId }: { wsId: string }) {
  const agents = useTeamAgents(wsId);
  const [selected, setSelected] = useState<string | null>(null);

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

  const rows = [...(agents.data ?? [])].sort((a, b) =>
    a.is_master === b.is_master
      ? a.name.localeCompare(b.name)
      : a.is_master
        ? -1
        : 1,
  );
  const active = selected ?? rows[0]?.id ?? null;

  return (
    <div className="flex h-full min-h-0 flex-col @min-[900px]/main:flex-row">
      <ul className="shrink-0 border-line @min-[900px]/main:w-64 @min-[900px]/main:border-r">
        {rows.map((agent) => (
          <li key={agent.id}>
            <button
              type="button"
              onClick={() => setSelected(agent.id)}
              className={cn(
                "flex h-9 w-full items-center gap-2 border-b border-line px-3 text-left",
                agent.id === active ? "bg-line/40" : "hover:bg-line/20",
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
      {active ? (
        <AgentEditor wsId={wsId} agentId={active} />
      ) : (
        <p className="max-w-[480px] p-3 text-sm text-ink-muted">
          这个团队还没有 agent。
        </p>
      )}
    </div>
  );
}
