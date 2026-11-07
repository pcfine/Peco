// skills tab：清单 + SKILL.md 预览，新建 / 编辑 / 删除都在这里 ——
// 技能目录归团队的能力根，装配页就是它的入口。

import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Trash2 } from "lucide-react";
import { toast } from "sonner";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { MarkdownRenderer } from "@/components/chat/MarkdownRenderer";
import { NewSkillDialog } from "@/components/teams/NewSkillDialog";
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
import { Textarea } from "@/components/ui/textarea";
import { getAgent } from "@/api/agents";
import { deleteSkill, saveSkill } from "@/api/skills";
import { errorText } from "@/lib/api";
import { cn } from "@/lib/utils";
import { useSkillMd, useTeamAgents, useTeamSkills } from "@/queries/teamDetail";
import type { AgentDetail } from "@/types/daemon";

export function TeamSkillsTab({ wsId }: { wsId: string }) {
  const queryClient = useQueryClient();
  const skills = useTeamSkills(wsId);
  const agents = useTeamAgents(wsId);
  const [selected, setSelected] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  /** 正在二次确认删除的技能名。 */
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);

  const rows = skills.data ?? [];
  const active = selected ?? rows[0]?.name ?? null;
  const body = useSkillMd(wsId, active);

  const save = useMutation({
    mutationFn: (next: { name: string; content: string }) =>
      saveSkill(wsId, next.name, next.content),
    onSuccess: (_res, next) => {
      // 描述住在 frontmatter 里：正文一改可能连描述一起改，列表也要重取。
      queryClient.setQueryData(["skill", wsId, next.name], {
        name: next.name,
        content: next.content,
      });
      void queryClient.invalidateQueries({ queryKey: ["skills", wsId] });
      setEditing(false);
      toast.success("SKILL.md 已保存");
    },
  });

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
          wsId,
          agent.id,
        ]);
        const detail = cached ?? (await getAgent(wsId, agent.id));
        if (detail.skills.includes(skill)) names.push(detail.name);
      }
      return names;
    },
  });

  const remove = useMutation({
    mutationFn: (name: string) => deleteSkill(wsId, name),
    onSuccess: (_res, name) => {
      queryClient.removeQueries({ queryKey: ["skill", wsId, name] });
      void queryClient.invalidateQueries({ queryKey: ["skills", wsId] });
      void queryClient.invalidateQueries({ queryKey: ["team"] });
      setPendingDelete(null);
      if (active === name) {
        // 删掉的正是当前预览的那个：回落到剩下第一项，没有就回空态。
        setEditing(false);
        setSelected(null);
      }
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

  const select = (name: string) => {
    setSelected(name);
    // 草稿跟着选中项走，不带到下一个技能身上。
    setEditing(false);
    save.reset();
  };

  const startEditing = () => {
    setDraft(body.data?.content ?? "");
    save.reset();
    setEditing(true);
  };

  if (skills.error) {
    return (
      <div className="p-3">
        <ErrorBanner
          message={errorText(skills.error)}
          onRetry={() => void skills.refetch()}
        />
      </div>
    );
  }
  if (skills.isPending) {
    return (
      <div className="space-y-px p-3">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-9 w-full" />
        ))}
      </div>
    );
  }

  const dialog = (
    <>
      <NewSkillDialog
        wsId={wsId}
        open={creating}
        onOpenChange={setCreating}
        onCreated={select}
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
              variant="outline"
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

  if (rows.length === 0) {
    return (
      <>
        <div className="p-3">
          <EmptyState
            title="这个团队还没有安装 skill。"
            action={
              <Button
                size="sm"
                variant="outline"
                className="h-7"
                onClick={() => setCreating(true)}
              >
                ＋ 新建技能
              </Button>
            }
          />
        </div>
        {dialog}
      </>
    );
  }

  return (
    <>
      <div className="flex h-full min-h-0 flex-col @min-[900px]/main:flex-row">
        <div className="shrink-0 border-line @min-[900px]/main:w-64 @min-[900px]/main:border-r">
          <div className="flex h-9 items-center border-b border-line px-2">
            <Button
              size="sm"
              variant="ghost"
              className="h-7 px-2 text-xs"
              onClick={() => setCreating(true)}
            >
              ＋ 新建技能
            </Button>
          </div>
          <ul>
            {rows.map((skill) => (
              <li key={skill.name} className="border-b border-line">
                <div
                  className={cn(
                    "flex items-center gap-1 pr-1",
                    skill.name === active ? "bg-line/40" : "hover:bg-line/20",
                  )}
                >
                  <button
                    type="button"
                    onClick={() => select(skill.name)}
                    className="flex h-9 min-w-0 flex-1 flex-col justify-center px-3 text-left"
                  >
                    <span className="truncate font-mono text-sm text-ink">
                      {skill.name}
                    </span>
                    <span className="truncate text-2xs text-ink-faint">
                      {skill.description || "没有描述"}
                    </span>
                  </button>
                  <Button
                    variant="ghost"
                    size="icon-xs"
                    className="shrink-0 text-ink-faint hover:text-danger"
                    aria-label={`删除 ${skill.name}`}
                    onClick={() => askDelete(skill.name)}
                  >
                    <Trash2 />
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        </div>

        <div className="flex min-h-0 flex-1 flex-col">
          <div className="flex h-9 shrink-0 items-center gap-2 border-b border-line px-3">
            <span className="min-w-0 flex-1 truncate font-mono text-2xs text-ink-faint">
              SKILL.md
            </span>
            {editing ? (
              <>
                <Button
                  size="xs"
                  disabled={
                    save.isPending || draft === (body.data?.content ?? null)
                  }
                  onClick={() => {
                    if (active !== null) {
                      save.mutate({ name: active, content: draft });
                    }
                  }}
                >
                  保存
                </Button>
                <Button
                  variant="outline"
                  size="xs"
                  disabled={save.isPending}
                  onClick={() => {
                    save.reset();
                    setEditing(false);
                  }}
                >
                  撤销
                </Button>
              </>
            ) : (
              <Button
                variant="outline"
                size="xs"
                disabled={!body.data}
                onClick={startEditing}
              >
                编辑
              </Button>
            )}
          </div>
          {save.error !== null && (
            <p className="shrink-0 border-b border-line px-3 py-1 text-2xs text-danger">
              {errorText(save.error)}
            </p>
          )}
          <div className="min-h-0 flex-1 overflow-y-auto p-3">
            {body.error !== null && (
              <ErrorBanner message={errorText(body.error)} />
            )}
            {body.isPending && <Skeleton className="h-16 w-full" />}
            {body.data &&
              (editing ? (
                <Textarea
                  aria-label="SKILL.md"
                  value={draft}
                  onChange={(event) => setDraft(event.target.value)}
                  disabled={save.isPending}
                  spellCheck={false}
                  className="min-h-[320px] font-mono text-mono-sm"
                />
              ) : (
                <MarkdownRenderer content={body.data.content} />
              ))}
          </div>
        </div>
      </div>
      {dialog}
    </>
  );
}
