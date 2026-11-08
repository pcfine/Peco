// MCP tab：卡片列表（状态 / transport / 地址）+ 整份配置的 JSON 编辑。
// 不做表单化新增：stdio / sse / streamable_http 三种形态的字段差异太大，
// 与 mcpconfig.json 同构的 JSON 才是最小充分入口。

import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft } from "lucide-react";
import { toast } from "sonner";
import { CardGrid, EntityCard } from "@/components/common/CardGrid";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { testMcpServer, updateMcpConfig } from "@/api/mcp";
import { errorText } from "@/lib/api";
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
          <span className="shrink-0 rounded-[4px] bg-line/60 px-1 text-2xs font-mono text-ink-muted">
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

export function TeamMcpTab({ wsId }: { wsId: string }) {
  const queryClient = useQueryClient();
  const config = useMcpConfig(wsId);
  /** 非 null 即「正在编辑」：内容是整份 mcpconfig.json 的文本。 */
  const [draft, setDraft] = useState<string | null>(null);
  const [syntaxError, setSyntaxError] = useState<string | null>(null);

  const test = useMutation({
    mutationFn: (name: string) => testMcpServer(wsId, name),
  });

  const save = useMutation({
    mutationFn: (parsed: unknown) => updateMcpConfig(wsId, parsed),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["mcp", wsId] });
      // agent 编辑器的 MCP 候选也读这份配置，扇出刷新一次。
      void queryClient.invalidateQueries({ queryKey: ["team"] });
      setDraft(null);
      setSyntaxError(null);
      toast.success("MCP 配置已保存");
    },
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

  const servers = Object.entries(config.data?.mcpServers ?? {});

  const startEditing = () => {
    // 草稿就是整份文件：顶层除了 mcpServers 还有别的键时，编辑不能把它们弄丢。
    setDraft(JSON.stringify(config.data ?? { mcpServers: {} }, null, 2));
    setSyntaxError(null);
    save.reset();
  };

  const cancelEditing = () => {
    setDraft(null);
    setSyntaxError(null);
    save.reset();
  };

  // 本地只挡语法：字段对不对由服务端说了算，400 的原文要给用户看到。
  const submit = () => {
    if (draft === null) return;
    let parsed: unknown;
    try {
      parsed = JSON.parse(draft);
    } catch (error) {
      setSyntaxError(`JSON 语法错误：${errorText(error)}`);
      return;
    }
    setSyntaxError(null);
    save.mutate(parsed);
  };

  if (draft !== null) {
    return (
      <div className="flex h-full min-h-0 flex-col">
        <div className="flex h-9 shrink-0 items-center gap-2 border-b border-line px-2">
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label="返回列表"
            onClick={cancelEditing}
          >
            <ArrowLeft />
          </Button>
          <span className="min-w-0 flex-1 truncate font-mono text-2xs text-ink-faint">
            mcpconfig.json
          </span>
          <Button size="xs" disabled={save.isPending} onClick={submit}>
            保存
          </Button>
          <Button
            variant="ghost"
            size="xs"
            disabled={save.isPending}
            onClick={cancelEditing}
          >
            撤销
          </Button>
        </div>
        {syntaxError && (
          <p className="shrink-0 border-b border-line px-3 py-1 text-caption font-medium text-danger">
            {syntaxError}
          </p>
        )}
        {save.error !== null && (
          <p className="shrink-0 border-b border-line px-3 py-1 text-caption font-medium text-danger">
            {errorText(save.error)}
          </p>
        )}
        <div className="min-h-0 flex-1 overflow-y-auto p-3">
          <Textarea
            aria-label="mcpconfig.json"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            disabled={save.isPending}
            spellCheck={false}
            className="min-h-[420px] font-mono text-mono-sm"
          />
        </div>
      </div>
    );
  }

  return (
    <div className="p-3">
      <div className="mb-3 flex items-center gap-2">
        <span className="text-sm font-medium text-ink">MCP</span>
        <span className="text-caption text-ink-faint">{servers.length}</span>
        <Button size="sm" className="ml-auto" onClick={startEditing}>
          编辑配置
        </Button>
      </div>
      {servers.length === 0 ? (
        <p className="max-w-narrow text-sm text-ink-muted">
          这个团队的 mcpconfig.json 里没有配
          server。点「编辑配置」可以直接粘一份 JSON 进来。
        </p>
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
    </div>
  );
}
