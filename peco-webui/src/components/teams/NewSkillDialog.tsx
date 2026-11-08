// 新建技能：只问名称与 SKILL.md 正文，Tier-3 资源文件本轮不涉及。
// 表单状态放在 DialogContent 里 —— 关掉即卸载，下次打开不会带着上一次的残留。

import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
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
import { Textarea } from "@/components/ui/textarea";
import { saveSkill } from "@/api/skills";
import { errorText } from "@/lib/api";

/** 与后端 `validate_name` 同一套规则：先在这里挡住，省一次 400 往返。 */
const VALID_SKILL_NAME = /^[a-z0-9]+(-[a-z0-9]+)*$/;

function skillNameError(name: string): string | null {
  if (name.trim() === "") return "名称不能为空";
  if (name.length > 64) return "名称最长 64 个字符";
  if (!VALID_SKILL_NAME.test(name)) {
    return "只有小写字母、数字和连字符可用，连字符不能开头、结尾或连续";
  }
  return null;
}

const SKILL_MD_HINT = `---
name: my-skill
description: 什么时候该用这个技能
---

# 步骤
`;

interface NewSkillFormProps {
  wsId: string;
  onCancel: () => void;
  onCreated: (name: string) => void;
}

function NewSkillForm({ wsId, onCancel, onCreated }: NewSkillFormProps) {
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [content, setContent] = useState("");

  const create = useMutation({
    mutationFn: () => saveSkill(wsId, name, content),
    onSuccess: () => {
      // 先塞预览再选它：列表要等一次重新拉取，预览不用等。
      queryClient.setQueryData(["skill", wsId, name], { name, content });
      void queryClient.invalidateQueries({ queryKey: ["skills", wsId] });
      void queryClient.invalidateQueries({ queryKey: ["team"] });
      toast.success(`已新建 skill「${name}」`);
      onCreated(name);
    },
    onError: (error) => toast.error(errorText(error)),
  });

  const nameError = skillNameError(name);

  return (
    <form
      className="space-y-3"
      onSubmit={(event) => {
        event.preventDefault();
        create.mutate();
      }}
    >
      <label className="block space-y-1">
        <span className="text-caption font-medium text-ink-faint">
          名称（必填）
        </span>
        <Input
          value={name}
          onChange={(event) => setName(event.target.value)}
          disabled={create.isPending}
          autoComplete="off"
          spellCheck={false}
          className="h-8 font-mono text-xs"
        />
      </label>
      {name && nameError && (
        <p className="text-caption font-medium text-danger">{nameError}</p>
      )}

      <label className="block space-y-1">
        <span className="text-caption font-medium text-ink-faint">
          内容（SKILL.md）
        </span>
        <Textarea
          value={content}
          onChange={(event) => setContent(event.target.value)}
          placeholder={SKILL_MD_HINT}
          disabled={create.isPending}
          spellCheck={false}
          className="min-h-[200px] font-mono text-mono-sm"
        />
      </label>
      <p className="text-caption font-medium text-ink-faint">
        SKILL.md 必须以 frontmatter 开头，其中 name
        与上面的名称一致、description 不能为空。
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
        <Button
          type="submit"
          size="sm"
          disabled={
            create.isPending || nameError !== null || content.trim() === ""
          }
        >
          创建
        </Button>
      </DialogFooter>
    </form>
  );
}

interface NewSkillDialogProps {
  wsId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: (name: string) => void;
}

export function NewSkillDialog({
  wsId,
  open,
  onOpenChange,
  onCreated,
}: NewSkillDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>新建 skill</DialogTitle>
          <DialogDescription>
            名称即目录名，改不了；正文就是这个技能的 SKILL.md。
          </DialogDescription>
        </DialogHeader>
        <NewSkillForm
          wsId={wsId}
          onCancel={() => onOpenChange(false)}
          onCreated={(name) => {
            // 先关再选：关掉即卸载表单，下次打开是干净的一份。
            onOpenChange(false);
            onCreated(name);
          }}
        />
      </DialogContent>
    </Dialog>
  );
}
