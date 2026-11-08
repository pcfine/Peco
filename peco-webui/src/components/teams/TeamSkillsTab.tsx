// skills tab：卡片列表 → 同一个位置的「源码 + 预览」编辑器，与 agents 面板同一个形状。
// 新建 / 编辑 / 删除都在这里 —— 技能目录归团队的能力根，装配页就是它的入口。

import { useEffect, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, Puzzle, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { CardGrid, EntityCard } from "@/components/common/CardGrid";
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
import { useSkillMd, useTeamAgents, useTeamSkills } from "@/queries/teamDetail";
import type { AgentDetail, SkillInfo } from "@/types/daemon";

/** 每敲一个字就重渲染整篇 markdown，长 SKILL.md 会拖住输入：预览跟着手停下来再更新。 */
const PREVIEW_DEBOUNCE_MS = 200;

interface SkillCardProps {
  skill: SkillInfo;
  onOpen: () => void;
  onDelete: () => void;
}

function SkillCard({ skill, onOpen, onDelete }: SkillCardProps) {
  return (
    <EntityCard
      onClick={onOpen}
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
          className="flex size-10 shrink-0 items-center justify-center rounded-[6px] bg-line/40"
        >
          <Puzzle className="size-4 text-ink-muted" />
        </span>
        <div className="min-w-0 flex-1 space-y-1">
          {/* 右上角留给删除按钮，名字行不铺到那里。 */}
          <div className="flex min-w-0 items-center pr-6">
            <span className="truncate font-mono text-sm text-ink">
              {skill.name}
            </span>
          </div>
          <p className="line-clamp-2 text-caption text-ink-muted">
            {skill.description || "无描述"}
          </p>
        </div>
      </div>
    </EntityCard>
  );
}

export function TeamSkillsTab({ wsId }: { wsId: string }) {
  const queryClient = useQueryClient();
  const skills = useTeamSkills(wsId);
  const agents = useTeamAgents(wsId);
  const [selected, setSelected] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  /** 预览用的正文：编辑时落后草稿一个防抖周期，不进编辑态时干脆不用它。 */
  const [preview, setPreview] = useState("");
  /** 正在二次确认删除的技能名。 */
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);

  const rows = skills.data ?? [];
  const body = useSkillMd(wsId, selected);

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
      setSelected((current) => (current === name ? null : current));
      toast.success(`已删除 skill「${name}」`);
    },
  });

  useEffect(() => {
    if (!editing) return;
    const timer = setTimeout(() => setPreview(draft), PREVIEW_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [draft, editing]);

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

  const backToList = () => {
    save.reset();
    setEditing(false);
    setSelected(null);
  };

  const startEditing = () => {
    const content = body.data?.content ?? "";
    setDraft(content);
    // 首屏直接给磁盘内容：等一次防抖再出字会闪一下空白。
    setPreview(content);
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
      <div className="flex h-full min-h-0 flex-col">
        <div className="flex h-9 shrink-0 items-center gap-2 border-b border-line px-2">
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label="返回列表"
            onClick={backToList}
          >
            <ArrowLeft />
          </Button>
          <span className="min-w-0 flex-1 truncate font-mono text-sm text-ink">
            {selected}
          </span>
          {editing ? (
            <>
              <Button
                size="xs"
                disabled={
                  save.isPending || draft === (body.data?.content ?? null)
                }
                onClick={() => save.mutate({ name: selected, content: draft })}
              >
                保存
              </Button>
              <Button
                variant="ghost"
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
              variant="ghost"
              size="xs"
              disabled={!body.data}
              onClick={startEditing}
            >
              编辑
            </Button>
          )}
        </div>
        {save.error !== null && (
          <p className="shrink-0 border-b border-line px-3 py-1 text-caption font-medium text-danger">
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
              // 宽面板左右分栏，窄面板上下堆叠：两种宽度下源码与预览都同时在场。
              <div className="grid min-w-0 grid-cols-1 gap-3 @min-[860px]/team:grid-cols-2">
                <Textarea
                  aria-label="SKILL.md"
                  value={draft}
                  onChange={(event) => setDraft(event.target.value)}
                  disabled={save.isPending}
                  spellCheck={false}
                  className="min-h-[420px] font-mono text-mono-sm"
                />
                <div className="min-w-0">
                  <MarkdownRenderer content={preview} />
                </div>
              </div>
            ) : (
              <MarkdownRenderer content={body.data.content} />
            ))}
        </div>
        {dialog}
      </div>
    );
  }

  if (rows.length === 0) {
    return (
      <>
        <div className="p-3">
          <EmptyState
            title="这个团队还没有安装 skill。"
            action={
              <Button size="sm" onClick={() => setCreating(true)}>
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
    <div className="p-3">
      <div className="mb-3 flex items-center gap-2">
        <span className="text-sm font-medium text-ink">Skill</span>
        <span className="text-caption text-ink-faint">{rows.length}</span>
        <Button size="sm" className="ml-auto" onClick={() => setCreating(true)}>
          ＋ 新建技能
        </Button>
      </div>
      <CardGrid>
        {rows.map((skill) => (
          <SkillCard
            key={skill.name}
            skill={skill}
            onOpen={() => select(skill.name)}
            onDelete={() => askDelete(skill.name)}
          />
        ))}
      </CardGrid>
      {dialog}
    </div>
  );
}
