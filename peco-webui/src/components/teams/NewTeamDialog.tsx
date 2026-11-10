// 新建团队：内置模板或从已有团队复制。

import { useState } from "react";
import { toast } from "sonner";
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
import { errorText } from "@/lib/api";
import { cn } from "@/lib/utils";
import { useCreateTeam } from "@/queries/teams";
import type { CreateTeamRequest, Team } from "@/types/daemon";

const SELECT_CLASS =
  "h-8 w-full min-w-0 rounded-md border border-input bg-card px-2 text-xs outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/25 disabled:pointer-events-none disabled:opacity-50";

/** 与后端 `create_team` 的校验保持一致；`peco` 是保留 id。 */
const TEAM_ID_RE = /^[a-z][a-z0-9-]{1,31}$/;
const RESERVED_TEAM_ID = "peco";
const TEMPLATE = "team_default";

type SourceMode = "template" | "copy";

function NewTeamForm({
  teams,
  onCancel,
  onCreated,
}: {
  teams: Team[];
  onCancel: () => void;
  onCreated: (team: Team) => void;
}) {
  const [id, setId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [mode, setMode] = useState<SourceMode>("template");
  const [sourceTeamId, setSourceTeamId] = useState("");
  const create = useCreateTeam();

  const trimmedId = id.trim();
  const idValid = TEAM_ID_RE.test(trimmedId) && trimmedId !== RESERVED_TEAM_ID;
  const sourceReady = mode !== "copy" || sourceTeamId !== "";
  const canSubmit = idValid && sourceReady && !create.isPending;

  async function submit() {
    if (!canSubmit) return;
    const body: CreateTeamRequest = {
      id: trimmedId,
      display_name: displayName.trim() || trimmedId,
      ...(mode === "copy"
        ? { from_team: sourceTeamId }
        : { from_template: TEMPLATE }),
    };
    try {
      const team = await create.mutateAsync(body);
      toast.success(`已新建团队「${team.display_name}」`);
      onCreated(team);
    } catch (error) {
      toast.error(errorText(error));
    }
  }

  return (
    <form
      className="space-y-3"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <div className="grid grid-cols-2 gap-2">
        <label className="space-y-1">
          <span className="text-caption font-medium text-ink-faint">
            团队 id（必填）
          </span>
          <Input
            value={id}
            onChange={(event) => setId(event.target.value)}
            placeholder="legal"
            disabled={create.isPending}
            autoComplete="off"
            autoFocus
            className="h-8 font-mono text-xs"
          />
        </label>
        <label className="space-y-1">
          <span className="text-caption font-medium text-ink-faint">
            显示名
          </span>
          <Input
            value={displayName}
            onChange={(event) => setDisplayName(event.target.value)}
            placeholder="法务"
            disabled={create.isPending}
            autoComplete="off"
            className="h-8 text-xs"
          />
        </label>
      </div>
      <p className="text-caption font-medium text-ink-faint">
        小写字母开头，可含数字与连字符，2–32 个字符；留空显示名时用 id。
      </p>

      <div className="grid grid-cols-2 gap-2">
        <label className="space-y-1">
          <span className="text-caption font-medium text-ink-faint">来源</span>
          <select
            value={mode}
            onChange={(event) => setMode(event.target.value as SourceMode)}
            disabled={create.isPending}
            aria-label="来源"
            className={cn(SELECT_CLASS)}
          >
            <option value="template">内置模板</option>
            <option value="copy" disabled={teams.length === 0}>
              从已有团队复制
            </option>
          </select>
        </label>

        {mode === "copy" && (
          <label className="space-y-1">
            <span className="text-caption font-medium text-ink-faint">
              源团队
            </span>
            <select
              value={sourceTeamId}
              onChange={(event) => setSourceTeamId(event.target.value)}
              disabled={create.isPending}
              aria-label="源团队"
              className={cn(SELECT_CLASS)}
            >
              <option value="">选择要复制的团队</option>
              {teams.map((team) => (
                <option key={team.team_id} value={team.team_id}>
                  {team.display_name}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>

      <p className="text-caption font-medium text-ink-faint">
        {mode === "copy"
          ? "复制会带走源团队的 agents、skills、知识库与配置；team.json 会换成新身份。"
          : "内置模板会安装 @master 与私有记忆库。"}
      </p>

      <DialogFooter>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={create.isPending}
          onClick={onCancel}
        >
          取消
        </Button>
        <Button type="submit" size="sm" disabled={!canSubmit}>
          {create.isPending ? "创建中…" : "创建"}
        </Button>
      </DialogFooter>
    </form>
  );
}

interface NewTeamDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  teams: Team[];
  onCreated: (team: Team) => void;
}

export function NewTeamDialog({
  open,
  onOpenChange,
  teams,
  onCreated,
}: NewTeamDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>新建团队</DialogTitle>
          <DialogDescription>
            团队是空间的能力根。可以从内置模板新建，也可以复制一份现有团队。
          </DialogDescription>
        </DialogHeader>
        <NewTeamForm
          teams={teams}
          onCancel={() => onOpenChange(false)}
          onCreated={(team) => {
            onOpenChange(false);
            onCreated(team);
          }}
        />
      </DialogContent>
    </Dialog>
  );
}
