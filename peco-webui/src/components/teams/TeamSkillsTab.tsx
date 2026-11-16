// skills tab：卡片列表 → 同一个位置的编辑/查看页。
// 新建与删除在列表侧完成；编辑与资源文件浏览交给 SkillEditor（从老 webui 移植）。
// 技能目录归团队的能力根，装配页就是它的入口。

import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Puzzle, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { CardGrid, EntityCard } from "@/components/common/CardGrid";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { NewSkillDialog } from "@/components/teams/NewSkillDialog";
import { SkillEditor } from "@/components/skills/SkillEditor";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { getAgent } from "@/api/agents";
import { deleteSkill } from "@/api/skills";
import { errorText, type ResourceScope } from "@/lib/api";
import { useTeamAgents, useTeamSkills } from "@/queries/teamDetail";
import type { AgentDetail, SkillInfo } from "@/types/daemon";

interface SkillCardProps {
  skill: SkillInfo;
  onOpen: () => void;
  onDelete: () => void;
}

function SkillCard({ skill, onOpen, onDelete }: SkillCardProps) {
  return (
    <EntityCard
      onClick={onOpen}
      className="min-h-[72px]"
      actions={
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-ink-faint hover:text-danger"
          aria-label={`删除 ${skill.name}`}
          onClick={(event) => {
            // 卡本体是「打开」，删除是压在它上面的一层：删完不该顺带进编辑器。
            event.stopPropagation();
            onDelete();
          }}
        >
          <Trash2 />
        </Button>
      }
    >
      <div className="flex min-w-0 items-start gap-2">
        <span
          aria-hidden
          className="flex size-10 shrink-0 items-center justify-center rounded-md bg-line/40"
        >
          <Puzzle className="size-4 text-ink-muted" />
        </span>
        <div className="min-w-0 flex-1">
          {/* 右上角留给删除按钮，名字行不铺到那里。 */}
          <div className="flex min-w-0 items-center pr-6">
            <span className="truncate font-mono text-sm text-ink">
              {skill.name}
            </span>
          </div>
        </div>
      </div>
    </EntityCard>
  );
}

export function TeamSkillsTab({ scope }: { scope: ResourceScope }) {
  const queryClient = useQueryClient();
  const skills = useTeamSkills(scope);
  const agents = useTeamAgents(scope);
  const [selected, setSelected] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  /** 正在二次确认删除的技能名。 */
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);

  const rows = skills.data ?? [];

  /**
   * 「删掉会让谁缺能力」—— skills 字段只在 agent 详情里，列表项没有。
   * 只在用户点删除时串行拉一次，不为一句提示扇出一批并发请求。
   */
  const references = useMutation({
    mutationFn: async (skill: string) => {
      if (agents.data === undefined) {
        throw new Error(
          agents.error ? errorText(agents.error) : "agent 列表还没读完",
        );
      }
      const names: string[] = [];
      for (const agent of agents.data) {
        const cached = queryClient.getQueryData<AgentDetail>([
          "agent",
          scope.teamId,
          scope.wsId,
          agent.id,
        ]);
        const detail = cached ?? (await getAgent(scope, agent.id));
        if (detail.skills.includes(skill)) names.push(detail.name);
      }
      return names;
    },
  });

  const remove = useMutation({
    mutationFn: (name: string) => deleteSkill(scope, name),
    onSuccess: (_res, name) => {
      void queryClient.invalidateQueries({
        queryKey: ["skills", scope.teamId, scope.wsId],
      });
      void queryClient.invalidateQueries({ queryKey: ["team"] });
      setPendingDelete(null);
      setSelected((current) => (current === name ? null : current));
      toast.success(`已删除 skill「${name}」`);
    },
  });

  const askDelete = (name: string) => {
    setPendingDelete(name);
    remove.reset();
    references.mutate(name);
  };

  const closeDelete = () => {
    setPendingDelete(null);
    references.reset();
    remove.reset();
  };

  const backToList = () => setSelected(null);

  if (skills.error) {
    return (
      <div className="h-full overflow-y-auto p-3">
        <ErrorBanner
          message={errorText(skills.error)}
          onRetry={() => void skills.refetch()}
        />
      </div>
    );
  }
  if (skills.isPending) {
    return (
      <div className="h-full space-y-px overflow-y-auto p-3">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-9 w-full" />
        ))}
      </div>
    );
  }

  const dialog = (
    <>
      <NewSkillDialog
        scope={scope}
        open={creating}
        onOpenChange={setCreating}
        onCreated={setSelected}
      />

      <Dialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) closeDelete();
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>删除 skill「{pendingDelete}」？</DialogTitle>
            <DialogDescription>
              连同它的 SKILL.md 与资源文件一起删掉，不可恢复。
            </DialogDescription>
          </DialogHeader>
          {references.isPending && (
            <p className="text-xs text-ink-faint">
              正在检查哪些 agent 引用了它…
            </p>
          )}
          {references.error !== null && (
            <p className="text-xs text-danger">
              没能检查 agent 引用：{errorText(references.error)}
            </p>
          )}
          {references.data &&
            (references.data.length > 0 ? (
              <div className="space-y-1 text-xs">
                <p className="text-danger">删除后这些 agent 会缺能力：</p>
                <p className="font-mono text-ink">
                  {references.data.join("、")}
                </p>
              </div>
            ) : (
              <p className="text-xs text-ink-faint">没有 agent 引用它。</p>
            ))}
          {remove.error !== null && (
            <p className="text-xs text-danger">{errorText(remove.error)}</p>
          )}
          <DialogFooter>
            <Button
              variant="ghost"
              size="sm"
              disabled={remove.isPending}
              onClick={closeDelete}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              size="sm"
              disabled={remove.isPending || references.isPending}
              onClick={() => {
                if (pendingDelete !== null) remove.mutate(pendingDelete);
              }}
            >
              确认删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );

  if (selected !== null) {
    return (
      <div className="h-full min-h-0">
        <SkillEditor scope={scope} name={selected} onBack={backToList} />
      </div>
    );
  }

  return (
    <div className="h-full overflow-y-auto p-3">
      <div className="mb-3 flex items-center gap-2">
        <Button size="sm" onClick={() => setCreating(true)}>
          ＋ 新建技能
        </Button>
        <span className="ml-auto flex items-center gap-2">
          <span className="text-sm font-medium text-ink">Skill</span>
          <span className="text-caption text-ink-faint">{rows.length}</span>
        </span>
      </div>
      {rows.length === 0 ? (
        <EmptyState title="这里什么也没有" />
      ) : (
        <CardGrid>
          {rows.map((skill) => (
            <SkillCard
              key={skill.name}
              skill={skill}
              onOpen={() => setSelected(skill.name)}
              onDelete={() => askDelete(skill.name)}
            />
          ))}
        </CardGrid>
      )}
      {dialog}
    </div>
  );
}
