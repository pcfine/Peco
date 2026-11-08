// dock 的产物 tab：该对话登记的产物。

import { useQuery } from "@tanstack/react-query";
import { listConversationArtifacts } from "@/api/conversations";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { errorText } from "@/lib/api";
import { formatBytes, formatCreatedAt } from "@/lib/format";

export function DockArtifacts({
  agentId,
  convId,
  wsId,
}: {
  agentId: string;
  convId: string;
  wsId: string;
}) {
  const { data, error, isPending, refetch } = useQuery({
    queryKey: ["conversation-artifacts", wsId, agentId, convId],
    queryFn: () => listConversationArtifacts(agentId, convId, wsId),
    refetchInterval: 15_000,
  });

  if (error) {
    return (
      <div className="p-3">
        <ErrorBanner
          message={errorText(error)}
          onRetry={() => void refetch()}
        />
      </div>
    );
  }

  if (isPending) {
    return (
      <div className="space-y-px p-3">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-9 w-full" />
        ))}
      </div>
    );
  }

  if (!data || data.length === 0) {
    return (
      <p className="max-w-narrow p-3 text-sm text-ink-muted">
        任务完成后，产物会出现在这里。
      </p>
    );
  }

  return (
    <ul className="divide-y divide-line">
      {data.map((item) => (
        <li key={item.id} className="flex h-9 items-center gap-2 px-3">
          <span className="shrink-0 font-mono text-2xs uppercase text-ink-faint">
            {item.kind}
          </span>
          <a
            href={item.url}
            target="_blank"
            rel="noreferrer"
            className="min-w-0 flex-1 truncate text-sm text-ink hover:underline"
          >
            {item.title}
          </a>
          <span className="shrink-0 tabular-nums text-2xs text-ink-faint">
            {formatBytes(item.size_bytes)}
          </span>
          <span className="w-24 shrink-0 text-right text-2xs text-ink-faint">
            {formatCreatedAt(item.created_at)}
          </span>
        </li>
      ))}
    </ul>
  );
}
