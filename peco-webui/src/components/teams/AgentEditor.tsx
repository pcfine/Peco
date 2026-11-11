// agent 装配编辑器。提交时只 PATCH 变化字段：后端按「省略即保留」合并，
// 全量提交会把别处刚改好的字段按旧快照写回去。

import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { AgentModelPicker } from "@/components/teams/AgentModelPicker";
import { ConfigSection, FieldRow } from "@/components/teams/config-form";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { updateAgent } from "@/api/agents";
import {
  type AgentDraft,
  diffDraft,
  draftFromDetail,
  toggleValue,
} from "@/lib/agentDraft";
import { errorText } from "@/lib/api";
import { TOOLS } from "@/lib/constants";
import { cn } from "@/lib/utils";
import {
  useAgentDetail,
  useKnowledgeBases,
  useMcpConfig,
  useTeamSkills,
} from "@/queries/teamDetail";
import type { UpdateAgentRequest } from "@/types/daemon";

interface CapabilityOption {
  value: string;
  label: string;
}

/** 库里存着但候选表里没有的项也要留一格：否则显示不出来，一保存就被删掉。 */
function withSelected(
  options: CapabilityOption[],
  selected: string[],
): CapabilityOption[] {
  const known = new Set(options.map((option) => option.value));
  return [
    ...options,
    ...selected
      .filter((value) => !known.has(value))
      .map((value) => ({ value, label: value })),
  ];
}

interface CapabilityGroupProps {
  title: string;
  options: CapabilityOption[];
  selected: string[];
  onToggle: (value: string) => void;
  empty: string;
  error?: string | null;
  disabled: boolean;
}

function CapabilityGroup({
  title,
  options,
  selected,
  onToggle,
  empty,
  error,
  disabled,
}: CapabilityGroupProps) {
  const merged = withSelected(options, selected);

  return (
    <div className="space-y-1">
      <span className="text-caption font-medium text-ink-faint">{title}</span>
      {merged.length === 0 ? (
        <p className="text-caption font-medium text-ink-faint">{empty}</p>
      ) : (
        <div className="flex flex-wrap gap-1">
          {merged.map((option) => {
            const on = selected.includes(option.value);
            return (
              <button
                key={option.value}
                type="button"
                aria-pressed={on}
                disabled={disabled}
                onClick={() => onToggle(option.value)}
                className={cn(
                  "h-7 rounded-md px-2 font-mono text-xs",
                  on
                    ? "bg-line text-ink"
                    : "bg-transparent text-ink-muted hover:bg-line/40",
                )}
              >
                {option.label}
              </button>
            );
          })}
        </div>
      )}
      {error && <p className="text-caption font-medium text-danger">{error}</p>}
    </div>
  );
}

interface AgentEditorProps {
  wsId: string;
  /** provider 配置按团队取；`wsId` 只是能力根的请求头。 */
  teamId: string;
  agentId: string;
  isMaster: boolean;
  /** 删除确认框归列表所有（卡上也能删），这里只负责把请求递出去。 */
  onRequestDelete: () => void;
}

export function AgentEditor({
  wsId,
  teamId,
  agentId,
  isMaster,
  onRequestDelete,
}: AgentEditorProps) {
  const queryClient = useQueryClient();
  const detail = useAgentDetail(wsId, agentId);
  const skills = useTeamSkills(wsId);
  const mcp = useMcpConfig(wsId);
  const knowledge = useKnowledgeBases(wsId);
  const [draft, setDraft] = useState<AgentDraft | null>(null);

  const save = useMutation({
    mutationFn: (body: UpdateAgentRequest) => updateAgent(wsId, agentId, body),
    onSuccess: (updated) => {
      setDraft(null);
      queryClient.setQueryData(["agent", wsId, agentId], updated);
      // 「列表里的名字 / 团队里的 agent 数」都变了，扇出刷新一次。
      void queryClient.invalidateQueries({ queryKey: ["agents", wsId] });
      void queryClient.invalidateQueries({ queryKey: ["team"] });
      toast.success("已保存");
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

  const loaded = detail.data;
  const current = draft ?? draftFromDetail(loaded);
  const diff = diffDraft(loaded, current);
  const dirty = Object.keys(diff.body).length > 0;
  const busy = save.isPending;
  const update = (patch: Partial<AgentDraft>) =>
    setDraft({ ...current, ...patch });

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="min-h-0 flex-1 overflow-y-auto">
        <ConfigSection title="基本信息">
          <FieldRow label="名称" htmlFor="agent-name">
            <Input
              id="agent-name"
              value={current.name}
              onChange={(event) => update({ name: event.target.value })}
              disabled={busy}
              className="h-8 font-mono text-xs"
            />
          </FieldRow>
          <FieldRow label="描述" htmlFor="agent-description">
            <Input
              id="agent-description"
              value={current.description}
              onChange={(event) => update({ description: event.target.value })}
              disabled={busy}
              className="h-8 text-xs"
            />
          </FieldRow>
          <FieldRow label="图标" htmlFor="agent-icon">
            <Input
              id="agent-icon"
              value={current.icon}
              onChange={(event) => update({ icon: event.target.value })}
              disabled={busy}
              className="h-8 w-16 text-center text-xs"
            />
          </FieldRow>
        </ConfigSection>

        <ConfigSection title="模型">
          <div className="grid grid-cols-1 gap-2 @min-[560px]/team:grid-cols-2">
            <AgentModelPicker
              teamId={teamId}
              provider={current.provider}
              model={current.model}
              onProviderChange={(provider) => update({ provider })}
              onModelChange={(model) => update({ model })}
              disabled={busy}
            />
          </div>
        </ConfigSection>

        <ConfigSection title="能力">
          <CapabilityGroup
            title="tools"
            options={TOOLS.map((tool) => ({
              value: tool.value,
              label: tool.label,
            }))}
            selected={current.tools}
            onToggle={(value) =>
              update({ tools: toggleValue(current.tools, value) })
            }
            empty="没有可选的工具。"
            disabled={busy}
          />
          <CapabilityGroup
            title="skills"
            options={(skills.data ?? []).map((skill) => ({
              value: skill.name,
              label: skill.name,
            }))}
            selected={current.skills}
            onToggle={(value) =>
              update({ skills: toggleValue(current.skills, value) })
            }
            empty="这个团队还没有安装 skill。"
            error={
              skills.error
                ? `skill 列表读取失败：${errorText(skills.error)}`
                : null
            }
            disabled={busy}
          />
          <CapabilityGroup
            title="MCP"
            options={Object.keys(mcp.data?.mcpServers ?? {}).map((name) => ({
              value: name,
              label: name,
            }))}
            selected={current.mcp_servers}
            onToggle={(value) =>
              update({ mcp_servers: toggleValue(current.mcp_servers, value) })
            }
            empty="这个团队的 mcpconfig.json 里没有配 server。"
            error={
              mcp.error ? `MCP 配置读取失败：${errorText(mcp.error)}` : null
            }
            disabled={busy}
          />
          <CapabilityGroup
            title="知识库"
            options={(knowledge.data ?? []).map((kb) => ({
              value: kb.name,
              label: kb.name,
            }))}
            selected={current.knowledge_bases}
            onToggle={(value) =>
              update({
                knowledge_bases: toggleValue(current.knowledge_bases, value),
              })
            }
            empty="这个团队还没有知识库。"
            error={
              knowledge.error
                ? `知识库列表读取失败：${errorText(knowledge.error)}`
                : null
            }
            disabled={busy}
          />
        </ConfigSection>

        <ConfigSection title="参数">
          <FieldRow label="temperature" htmlFor="agent-temperature">
            <Input
              id="agent-temperature"
              type="number"
              step="0.1"
              value={current.temperature}
              onChange={(event) => update({ temperature: event.target.value })}
              placeholder="不改"
              disabled={busy}
              className="h-8 font-mono text-xs"
            />
          </FieldRow>
          <FieldRow label="max_tokens" htmlFor="agent-max-tokens">
            <Input
              id="agent-max-tokens"
              type="number"
              value={current.max_tokens}
              onChange={(event) => update({ max_tokens: event.target.value })}
              placeholder="不改"
              disabled={busy}
              className="h-8 font-mono text-xs"
            />
          </FieldRow>
          <FieldRow label="max_iterations" htmlFor="agent-max-iterations">
            <Input
              id="agent-max-iterations"
              type="number"
              value={current.max_iterations}
              onChange={(event) =>
                update({ max_iterations: event.target.value })
              }
              placeholder="不改"
              disabled={busy}
              className="h-8 font-mono text-xs"
            />
          </FieldRow>
        </ConfigSection>

        {/* 正文是全宽的：标签摞在框上面，不占 FieldRow 那条定宽的标签列。 */}
        <ConfigSection title="正文">
          <div className="space-y-1">
            <Label
              htmlFor="agent-system-prompt"
              className="text-caption font-medium text-ink-faint"
            >
              正文（system_prompt）
            </Label>
            <Textarea
              id="agent-system-prompt"
              value={current.system_prompt}
              onChange={(event) =>
                update({ system_prompt: event.target.value })
              }
              disabled={busy}
              className="min-h-[260px] font-mono text-mono-sm"
            />
          </div>
        </ConfigSection>
      </div>

      <div className="flex shrink-0 items-center gap-2 border-t border-line px-3 py-2">
        <Button
          size="sm"
          disabled={!dirty || busy || diff.invalid.length > 0}
          onClick={() => save.mutate(diff.body)}
        >
          保存
        </Button>
        <Button
          variant="ghost"
          size="sm"
          disabled={!dirty || busy}
          onClick={() => setDraft(null)}
        >
          放弃修改
        </Button>
        {diff.invalid.length > 0 ? (
          <span className="text-caption font-medium text-danger">
            {diff.invalid[0]}
          </span>
        ) : (
          dirty && (
            <span className="text-caption font-medium text-warn">未保存</span>
          )
        )}
        {!isMaster && (
          <Button
            variant="ghost"
            size="sm"
            className="ml-auto text-danger"
            disabled={busy}
            onClick={onRequestDelete}
          >
            删除
          </Button>
        )}
      </div>
    </div>
  );
}
