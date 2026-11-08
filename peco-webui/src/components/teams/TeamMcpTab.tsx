// MCP tab：server 名单 + 试连 + 整份配置的 JSON 编辑。
// 不做表单化新增：stdio / sse / streamable_http 三种形态的字段差异太大，
// 与 mcpconfig.json 同构的 JSON 才是最小充分入口。

import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
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

export function TeamMcpTab({ wsId }: { wsId: string }) {
  const queryClient = useQueryClient();
  const config = useMcpConfig(wsId);
  const [expanded, setExpanded] = useState<string | null>(null);
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

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-9 shrink-0 items-center border-b border-line px-2">
        <Button
          size="sm"
          variant="ghost"
          className="h-7 px-2 text-xs"
          disabled={draft !== null}
          onClick={startEditing}
        >
          编辑配置
        </Button>
      </div>

      {draft !== null && (
        <div className="shrink-0 space-y-2 border-b border-line p-3">
          <Textarea
            aria-label="mcpconfig.json"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            disabled={save.isPending}
            spellCheck={false}
            className="min-h-[200px] font-mono text-mono-sm"
          />
          <div className="flex items-center gap-2">
            <Button size="sm" disabled={save.isPending} onClick={submit}>
              保存
            </Button>
            <Button
              variant="ghost"
              size="sm"
              disabled={save.isPending}
              onClick={() => {
                setDraft(null);
                setSyntaxError(null);
                save.reset();
              }}
            >
              撤销
            </Button>
            {syntaxError && (
              <span className="text-caption font-medium text-danger">
                {syntaxError}
              </span>
            )}
            {save.error !== null && (
              <span className="text-caption font-medium text-danger">
                {errorText(save.error)}
              </span>
            )}
          </div>
        </div>
      )}

      {servers.length === 0 ? (
        <p className="max-w-[480px] p-3 text-sm text-ink-muted">
          这个团队的 mcpconfig.json 里没有配
          server。点「编辑配置」可以直接粘一份 JSON 进来。
        </p>
      ) : (
        <ul className="min-h-0 flex-1 overflow-y-auto">
          {servers.map(([name, value]) => {
            const line =
              test.variables === name ? testLine(test.data, test.error) : null;
            const testing = test.isPending && test.variables === name;
            return (
              <li key={name} className="border-b border-line">
                <div className="flex h-9 items-center gap-2 px-3 hover:bg-line/30">
                  <button
                    type="button"
                    aria-expanded={expanded === name}
                    onClick={() => setExpanded(expanded === name ? null : name)}
                    className="flex h-full min-w-0 flex-1 items-center gap-2 text-left"
                  >
                    <span className="min-w-0 flex-1 truncate font-mono text-sm text-ink">
                      {name}
                    </span>
                    <span className="shrink-0 text-caption font-medium text-ink-faint">
                      {expanded === name ? "收起" : "看配置"}
                    </span>
                  </button>
                  {/* 试连一次要真起进程 / 打网络：并发的第二个没有意义。 */}
                  <Button
                    variant="ghost"
                    size="xs"
                    className="shrink-0"
                    disabled={test.isPending}
                    onClick={() => test.mutate(name)}
                  >
                    {testing ? "试连中…" : "试连"}
                  </Button>
                </div>
                {expanded === name && (
                  <pre className="max-h-64 overflow-auto border-t border-line bg-canvas/40 px-3 py-2 font-mono text-2xs text-ink-muted">
                    {JSON.stringify(value, null, 2)}
                  </pre>
                )}
                {line && (
                  <p
                    className={cn(
                      "border-t border-line px-3 py-1 text-caption font-medium",
                      line.ok ? "text-ink-muted" : "text-danger",
                    )}
                  >
                    {line.text}
                  </p>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
