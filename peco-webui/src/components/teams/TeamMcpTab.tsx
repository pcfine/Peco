// MCP tab：卡片列表（状态 / transport / 地址）+ 表单化新增。
// 新增走 NewMcpDialog（与 webui 的 MCP 新建弹窗同构），按钮即唯一入口。

import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { CardGrid, EntityCard } from "@/components/common/CardGrid";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { NewMcpDialog } from "@/components/teams/NewMcpDialog";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { testMcpServer } from "@/api/mcp";
import { errorText, type ResourceScope } from "@/lib/api";
import { cn } from "@/lib/utils";
import { useMcpConfig } from "@/queries/teamDetail";
import type { McpTestResult } from "@/types/daemon";

/** 试连结果压成一行：成功给工具数与耗时，失败给服务端那句话。 */
function testLine(
  result: McpTestResult | undefined,
  error: unknown,
): { ok: boolean; text: string } | null {
  if (result) {
    return result.success
      ? {
          ok: true,
          text: `连接成功 · ${result.tool_count} 个工具 · ${result.duration_ms}ms`,
        }
      : { ok: false, text: `连接失败 · ${result.message}` };
  }
  if (error) return { ok: false, text: `连接失败 · ${errorText(error)}` };
  return null;
}

/** 卡片要从配置里取的那几项，取不到就是 `null` —— 缺字段的配置也得能摆出来。 */
interface ServerSummary {
  enabled: boolean;
  transport: string | null;
  endpoint: string | null;
  timeoutSecs: number | null;
  maxRetries: number | null;
}

/**
 * 配置值是磁盘上的原始 JSON，字段可能缺、类型可能不对（文件是手写的）。
 * 逐项判类型，缺的当没有：一张卡渲染不出来不值得把整个面板打崩。
 */
function summarize(value: unknown): ServerSummary {
  const record =
    typeof value === "object" && value !== null
      ? (value as Record<string, unknown>)
      : {};
  const transport =
    typeof record.transport === "string" ? record.transport : null;
  const command = typeof record.command === "string" ? record.command : null;
  const url = typeof record.url === "string" ? record.url : null;
  const remote = transport === "sse" || transport === "streamable_http";

  return {
    enabled: record.enabled !== false,
    transport,
    // 远端 transport 认 url，其余认 command；没有 transport 的配置先看 command。
    endpoint: remote ? url : (command ?? url),
    timeoutSecs:
      typeof record.timeoutSecs === "number" ? record.timeoutSecs : null,
    maxRetries:
      typeof record.maxRetries === "number" ? record.maxRetries : null,
  };
}

interface ServerCardProps {
  name: string;
  config: unknown;
  /** 试连中的那一张卡换成「试连中…」，其余同批禁用。 */
  testing: boolean;
  disabled: boolean;
  line: { ok: boolean; text: string } | null;
  onTest: () => void;
}

function ServerCard({
  name,
  config,
  testing,
  disabled,
  line,
  onTest,
}: ServerCardProps) {
  const summary = summarize(config);

  return (
    <EntityCard
      actions={
        // 试连一次要真起进程 / 打网络：并发的第二个没有意义。
        <Button variant="ghost" size="xs" disabled={disabled} onClick={onTest}>
          {testing ? "试连中…" : "试连"}
        </Button>
      }
    >
      {/* 右上角留给试连按钮（「试连中…」是最宽的一档），名字行不铺到那里。 */}
      <div className="flex min-w-0 items-center gap-1.5 pr-14">
        <span
          aria-hidden
          className={cn(
            "size-2 shrink-0 rounded-full",
            summary.enabled ? "bg-success" : "bg-ink-faint",
          )}
        />
        {/* 状态不能只靠颜色：读屏靠这句。 */}
        <span className="sr-only">{summary.enabled ? "已启用" : "已停用"}</span>
        <span className="min-w-0 truncate font-mono text-sm text-ink">
          {name}
        </span>
        {summary.transport && (
          <span className="shrink-0 rounded-sm bg-line/60 px-1 text-2xs font-mono text-ink-muted">
            {summary.transport}
          </span>
        )}
      </div>
      <div className="mt-auto flex min-w-0 items-baseline gap-1 font-mono text-caption text-ink-faint">
        {summary.endpoint && (
          <span className="min-w-0 truncate">{summary.endpoint}</span>
        )}
        {summary.timeoutSecs !== null && (
          <span className="shrink-0 tabular-nums">
            · {summary.timeoutSecs}s
          </span>
        )}
        {summary.maxRetries !== null && (
          <span className="shrink-0 tabular-nums">
            · 重试 {summary.maxRetries} 次
          </span>
        )}
      </div>
      {line && (
        <p
          className={cn(
            "text-caption font-medium",
            line.ok ? "text-ink-muted" : "text-danger",
          )}
        >
          {line.text}
        </p>
      )}
    </EntityCard>
  );
}

export function TeamMcpTab({ scope }: { scope: ResourceScope }) {
  const config = useMcpConfig(scope);
  const [adding, setAdding] = useState(false);

  const test = useMutation({
    mutationFn: (name: string) => testMcpServer(scope, name),
  });

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

  const mcpConfig = config.data ?? { mcpServers: {} };
  const servers = Object.entries(mcpConfig.mcpServers);

  return (
    <div className="p-3">
      <div className="mb-3 flex items-center gap-2">
        <Button size="sm" onClick={() => setAdding(true)}>
          ＋ 添加MCP
        </Button>
        <span className="ml-auto flex items-center gap-2">
          <span className="text-sm font-medium text-ink">MCP</span>
          <span className="text-caption text-ink-faint">{servers.length}</span>
        </span>
      </div>
      {servers.length === 0 ? (
        <EmptyState title="这里什么也没有" />
      ) : (
        <CardGrid>
          {servers.map(([name, value]) => (
            <ServerCard
              key={name}
              name={name}
              config={value}
              testing={test.isPending && test.variables === name}
              disabled={test.isPending}
              line={
                test.variables === name ? testLine(test.data, test.error) : null
              }
              onTest={() => test.mutate(name)}
            />
          ))}
        </CardGrid>
      )}
      <NewMcpDialog
        scope={scope}
        config={mcpConfig}
        open={adding}
        onOpenChange={setAdding}
      />
    </div>
  );
}
