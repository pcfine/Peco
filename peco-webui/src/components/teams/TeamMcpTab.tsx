// MCP tab：server 名字一行一个，原始配置折在下面只读。
// 「连得上吗」不在这里回答 —— 试连是写动作，团队页只读。

import { useState } from "react";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Skeleton } from "@/components/ui/skeleton";
import { useMcpConfig } from "@/queries/teamDetail";
import { errorText } from "@/lib/api";

export function TeamMcpTab({ wsId }: { wsId: string }) {
  const config = useMcpConfig(wsId);
  const [expanded, setExpanded] = useState<string | null>(null);

  if (config.error) {
    return (
      <div className="p-3">
        <ErrorBanner
          message={errorText(config.error)}
          onRetry={() => void config.refetch()}
        />
      </div>
    );
  }
  if (config.isPending) {
    return (
      <div className="space-y-px p-3">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-9 w-full" />
        ))}
      </div>
    );
  }

  const servers = Object.entries(config.data?.mcpServers ?? {});
  if (servers.length === 0) {
    return (
      <p className="max-w-[480px] p-3 text-sm text-ink-muted">
        这个团队的 mcpconfig.json 里没有配 server。
      </p>
    );
  }

  return (
    <ul>
      {servers.map(([name, value]) => (
        <li key={name} className="border-b border-line">
          <button
            type="button"
            aria-expanded={expanded === name}
            onClick={() => setExpanded(expanded === name ? null : name)}
            className="flex h-9 w-full items-center gap-2 px-3 text-left hover:bg-line/30"
          >
            <span className="min-w-0 flex-1 truncate font-mono text-sm text-ink">
              {name}
            </span>
            <span className="shrink-0 text-2xs text-ink-faint">
              {expanded === name ? "收起" : "看配置"}
            </span>
          </button>
          {expanded === name && (
            <pre className="max-h-64 overflow-auto border-t border-line bg-canvas/40 px-3 py-2 font-mono text-2xs text-ink-muted">
              {JSON.stringify(value, null, 2)}
            </pre>
          )}
        </li>
      ))}
    </ul>
  );
}
