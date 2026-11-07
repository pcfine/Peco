// 新建 agent：只问建得出来的那几项，tools / skills / 运行参数留给建完的编辑器。
// 表单状态放在 DialogContent 里 —— 关掉即卸载，下次打开不会带着上一次的残留。

import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { AgentModelPicker } from "@/components/teams/AgentModelPicker";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { createAgent } from "@/api/agents";
import {
  EMPTY_NEW_AGENT,
  type NewAgentDraft,
  createRequestFromDraft,
} from "@/lib/agentDraft";
import { errorText } from "@/lib/api";

interface NewAgentFormProps {
  wsId: string;
  onCancel: () => void;
  onCreated: (agentId: string) => void;
}

function NewAgentForm({ wsId, onCancel, onCreated }: NewAgentFormProps) {
  const queryClient = useQueryClient();
  const [draft, setDraft] = useState<NewAgentDraft>(EMPTY_NEW_AGENT);

  const create = useMutation({
    mutationFn: () => createAgent(wsId, createRequestFromDraft(draft)),
    onSuccess: (agent) => {
      // 先塞详情再选它：列表要等一次重新拉取，编辑器不用等。
      queryClient.setQueryData(["agent", wsId, agent.id], agent);
      void queryClient.invalidateQueries({ queryKey: ["agents", wsId] });
      void queryClient.invalidateQueries({ queryKey: ["team"] });
      toast.success(`已新建 agent「${agent.name}」`);
      onCreated(agent.id);
    },
    onError: (error) => toast.error(errorText(error)),
  });

  const update = (patch: Partial<NewAgentDraft>) =>
    setDraft({ ...draft, ...patch });

  return (
    <form
      className="space-y-3"
      onSubmit={(event) => {
        event.preventDefault();
        create.mutate();
      }}
    >
      <div className="grid grid-cols-[2fr_3fr_auto] gap-2">
        <label className="space-y-1">
          <span className="text-2xs text-ink-faint">名称（必填）</span>
          <Input
            value={draft.name}
            onChange={(event) => update({ name: event.target.value })}
            disabled={create.isPending}
            autoComplete="off"
            className="h-8 font-mono text-xs"
          />
        </label>
        <label className="space-y-1">
          <span className="text-2xs text-ink-faint">描述</span>
          <Input
            value={draft.description}
            onChange={(event) => update({ description: event.target.value })}
            disabled={create.isPending}
            autoComplete="off"
            className="h-8 text-xs"
          />
        </label>
        <label className="space-y-1">
          <span className="text-2xs text-ink-faint">图标</span>
          <Input
            value={draft.icon}
            onChange={(event) => update({ icon: event.target.value })}
            placeholder="🤖"
            disabled={create.isPending}
            autoComplete="off"
            className="h-8 w-16 text-center text-xs"
          />
        </label>
      </div>

      <div className="grid grid-cols-2 gap-2">
        <AgentModelPicker
          provider={draft.provider}
          model={draft.model}
          onProviderChange={(provider) => update({ provider })}
          onModelChange={(model) => update({ model })}
          disabled={create.isPending}
        />
      </div>

      <label className="block space-y-1">
        <span className="text-2xs text-ink-faint">正文（system_prompt）</span>
        <Textarea
          value={draft.system_prompt}
          onChange={(event) => update({ system_prompt: event.target.value })}
          placeholder="你是一位……"
          disabled={create.isPending}
          className="min-h-[120px] font-mono text-mono-sm"
        />
      </label>

      <DialogFooter>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={create.isPending}
          onClick={onCancel}
        >
          取消
        </Button>
        <Button
          type="submit"
          size="sm"
          disabled={create.isPending || draft.name.trim() === ""}
        >
          创建
        </Button>
      </DialogFooter>
    </form>
  );
}

interface NewAgentDialogProps {
  wsId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: (agentId: string) => void;
}

export function NewAgentDialog({
  wsId,
  open,
  onOpenChange,
  onCreated,
}: NewAgentDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>新建 agent</DialogTitle>
          <DialogDescription>
            只有名称必填，其余留空由服务端补默认值；建好后可继续补能力与运行参数。
          </DialogDescription>
        </DialogHeader>
        <NewAgentForm
          wsId={wsId}
          onCancel={() => onOpenChange(false)}
          onCreated={(agentId) => {
            // 先关再选：关掉即卸载表单，下次打开是干净的一份。
            onOpenChange(false);
            onCreated(agentId);
          }}
        />
      </DialogContent>
    </Dialog>
  );
}
