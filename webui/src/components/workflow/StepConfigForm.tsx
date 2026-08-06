import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { ChevronDown, GripVertical, Trash2 } from "lucide-react";
import { cn } from "@/lib/utils";

// ── Types (subset of WorkflowStep for the form) ──────────────────

export interface StepFormData {
  id: string;
  name: string;
  type: "shell" | "agent" | "llm" | "tool";
  command?: string;
  agentName?: string;
  prompt?: string;
  maxTurns?: number;
  toolName?: string;
  toolArgs?: string;
  dependsOn: string[];
  condition?: string;
  timeoutSeconds?: number;
  onFailure: "continue" | "abort" | "retry" | "pause";
}

interface StepConfigFormProps {
  steps: StepFormData[];
  allStepIds: string[];
  onChange: (steps: StepFormData[]) => void;
  className?: string;
}

const STEP_TYPES = [
  { value: "shell", label: "Shell" },
  { value: "agent", label: "Agent" },
  { value: "llm", label: "LLM" },
  { value: "tool", label: "Tool" },
];

const FAILURE_POLICIES = [
  { value: "abort", label: "中止 (Abort)" },
  { value: "continue", label: "继续 (Continue)" },
  { value: "pause", label: "暂停 (Pause)" },
  { value: "retry", label: "重试 (Retry)" },
];

function defaultStep(): StepFormData {
  return {
    id: "",
    name: "",
    type: "shell",
    command: "",
    dependsOn: [],
    onFailure: "abort",
  };
}

/**
 * Phase 1 form-based step editor.
 * Marked "Beta" — the primary editing mode is YAML.
 *
 * Note: This is a Phase 1 MVP. Full bidirectional sync between form ↔ YAML
 * is a Phase 2 feature. The form currently reads from parsed YAML but changes
 * aren't written back to YAML automatically.
 */
export function StepConfigForm({
  steps,
  allStepIds,
  onChange,
  className,
}: StepConfigFormProps) {
  const [expandedSteps, setExpandedSteps] = useState<Set<number>>(new Set());

  const toggleExpand = (idx: number) => {
    setExpandedSteps((prev) => {
      const next = new Set(prev);
      if (next.has(idx)) next.delete(idx);
      else next.add(idx);
      return next;
    });
  };

  const updateStep = (idx: number, patch: Partial<StepFormData>) => {
    const updated = steps.map((s, i) => (i === idx ? { ...s, ...patch } : s));
    onChange(updated);
  };

  const removeStep = (idx: number) => {
    onChange(steps.filter((_, i) => i !== idx));
  };

  const addStep = () => {
    onChange([...steps, { ...defaultStep(), id: `step_${steps.length + 1}` }]);
  };

  return (
    <div className={cn("space-y-3", className)}>
      <div className="flex items-center justify-between">
        <Label className="text-sm font-medium">步骤定义</Label>
        <span className="text-xs bg-amber-100 text-amber-800 px-2 py-0.5 rounded">
          Beta
        </span>
      </div>

      {steps.map((step, idx) => {
        const isExpanded = expandedSteps.has(idx);
        return (
          <Collapsible
            key={idx}
            open={isExpanded}
            onOpenChange={() => toggleExpand(idx)}
            className="border rounded-md"
          >
            <CollapsibleTrigger asChild>
              <button
                type="button"
                className="flex items-center gap-2 w-full p-3 text-left hover:bg-muted/50 transition-colors"
              >
                <GripVertical className="h-4 w-4 text-muted-foreground shrink-0" />
                <ChevronDown
                  className={cn(
                    "h-4 w-4 text-muted-foreground shrink-0 transition-transform",
                    isExpanded && "rotate-180",
                  )}
                />
                <span className="text-sm font-medium flex-1">
                  步骤 {idx + 1}
                </span>
                <span className="text-xs text-muted-foreground">
                  {step.name || "(未命名)"}
                </span>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6 shrink-0"
                  onClick={(e) => {
                    e.stopPropagation();
                    removeStep(idx);
                  }}
                >
                  <Trash2 className="h-3.5 w-3.5 text-destructive" />
                </Button>
              </button>
            </CollapsibleTrigger>
            <CollapsibleContent>
              <div className="p-3 pt-0 space-y-3 border-t">
                {/* ID + Name + Type */}
                <div className="grid grid-cols-3 gap-2">
                  <div className="space-y-1">
                    <Label className="text-xs">ID</Label>
                    <Input
                      className="h-8 text-xs"
                      value={step.id}
                      onChange={(e) => updateStep(idx, { id: e.target.value })}
                      placeholder="lint"
                    />
                  </div>
                  <div className="space-y-1">
                    <Label className="text-xs">名称</Label>
                    <Input
                      className="h-8 text-xs"
                      value={step.name}
                      onChange={(e) =>
                        updateStep(idx, { name: e.target.value })
                      }
                      placeholder="静态检查"
                    />
                  </div>
                  <div className="space-y-1">
                    <Label className="text-xs">类型</Label>
                    <Select
                      value={step.type}
                      onValueChange={(v) =>
                        updateStep(idx, { type: v as StepFormData["type"] })
                      }
                    >
                      <SelectTrigger className="h-8 text-xs">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {STEP_TYPES.map((t) => (
                          <SelectItem key={t.value} value={t.value}>
                            {t.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                </div>

                {/* Type-specific config */}
                {step.type === "shell" && (
                  <div className="space-y-1">
                    <Label className="text-xs">命令</Label>
                    <Input
                      className="h-8 text-xs font-mono"
                      value={step.command ?? ""}
                      onChange={(e) =>
                        updateStep(idx, { command: e.target.value })
                      }
                      placeholder="cargo clippy --workspace"
                    />
                  </div>
                )}
                {(step.type === "agent" || step.type === "llm") && (
                  <>
                    {step.type === "agent" && (
                      <div className="space-y-1">
                        <Label className="text-xs">Agent</Label>
                        <Input
                          className="h-8 text-xs"
                          value={step.agentName ?? ""}
                          onChange={(e) =>
                            updateStep(idx, { agentName: e.target.value })
                          }
                          placeholder="@code-reviewer"
                        />
                      </div>
                    )}
                    <div className="space-y-1">
                      <Label className="text-xs">Prompt</Label>
                      <Input
                        className="h-8 text-xs"
                        value={step.prompt ?? ""}
                        onChange={(e) =>
                          updateStep(idx, { prompt: e.target.value })
                        }
                        placeholder="请审查代码..."
                      />
                    </div>
                    {step.type === "agent" && (
                      <div className="space-y-1">
                        <Label className="text-xs">最大轮次</Label>
                        <Input
                          className="h-8 text-xs w-24"
                          type="number"
                          value={step.maxTurns ?? ""}
                          onChange={(e) =>
                            updateStep(idx, {
                              maxTurns: e.target.value
                                ? Number(e.target.value)
                                : undefined,
                            })
                          }
                        />
                      </div>
                    )}
                  </>
                )}
                {step.type === "tool" && (
                  <>
                    <div className="space-y-1">
                      <Label className="text-xs">工具名</Label>
                      <Input
                        className="h-8 text-xs"
                        value={step.toolName ?? ""}
                        onChange={(e) =>
                          updateStep(idx, { toolName: e.target.value })
                        }
                      />
                    </div>
                    <div className="space-y-1">
                      <Label className="text-xs">参数 (JSON)</Label>
                      <Input
                        className="h-8 text-xs font-mono"
                        value={step.toolArgs ?? ""}
                        onChange={(e) =>
                          updateStep(idx, { toolArgs: e.target.value })
                        }
                        placeholder='{"key": "value"}'
                      />
                    </div>
                  </>
                )}

                {/* Failure policy + depends */}
                <div className="grid grid-cols-2 gap-2">
                  <div className="space-y-1">
                    <Label className="text-xs">失败策略</Label>
                    <Select
                      value={step.onFailure}
                      onValueChange={(v) =>
                        updateStep(idx, {
                          onFailure: v as StepFormData["onFailure"],
                        })
                      }
                    >
                      <SelectTrigger className="h-8 text-xs">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {FAILURE_POLICIES.map((f) => (
                          <SelectItem key={f.value} value={f.value}>
                            {f.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="space-y-1">
                    <Label className="text-xs">依赖步骤 (逗号分隔)</Label>
                    <Input
                      className="h-8 text-xs"
                      value={step.dependsOn.join(", ")}
                      onChange={(e) =>
                        updateStep(idx, {
                          dependsOn: e.target.value
                            .split(",")
                            .map((s) => s.trim())
                            .filter(Boolean),
                        })
                      }
                      placeholder="lint, review"
                    />
                  </div>
                </div>

                {/* Condition + Timeout */}
                <div className="grid grid-cols-2 gap-2">
                  <div className="space-y-1">
                    <Label className="text-xs">条件表达式 (可选)</Label>
                    <Input
                      className="h-8 text-xs font-mono"
                      value={step.condition ?? ""}
                      onChange={(e) =>
                        updateStep(idx, {
                          condition: e.target.value || undefined,
                        })
                      }
                      placeholder="{{ steps.review.success }}"
                    />
                  </div>
                  <div className="space-y-1">
                    <Label className="text-xs">超时 (秒)</Label>
                    <Input
                      className="h-8 text-xs w-24"
                      type="number"
                      value={step.timeoutSeconds ?? ""}
                      onChange={(e) =>
                        updateStep(idx, {
                          timeoutSeconds: e.target.value
                            ? Number(e.target.value)
                            : undefined,
                        })
                      }
                    />
                  </div>
                </div>
              </div>
            </CollapsibleContent>
          </Collapsible>
        );
      })}

      <Button variant="outline" size="sm" className="w-full" onClick={addStep}>
        + 添加步骤
      </Button>
    </div>
  );
}
