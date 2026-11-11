// 新建知识库：名称 + 描述，字段与 webui 的「新建知识库」弹窗同构。
// 名称规则先在前端挡一道（与后端 validate_kb_name 同一套），省一次 400 往返。

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
import { createKnowledgeBase } from "@/api/knowledge";
import { errorText } from "@/lib/api";

/** 与后端一致：字母/数字开头结尾，中间只允许字母、数字、`.`、`_`、`-`。 */
const VALID_KB_NAME = /^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$/;

function kbNameError(name: string): string | null {
  if (name === "") return "名称不能为空";
  if ([...name].length > 64) return "名称最多 64 个字符";
  if (!VALID_KB_NAME.test(name)) {
    return "需以字母或数字开头和结尾，只能包含字母、数字、.、_、-（不能以 @ 开头）";
  }
  return null;
}

interface NewKnowledgeFormProps {
  wsId: string;
  onCancel: () => void;
  onCreated: () => void;
}

function NewKnowledgeForm({
  wsId,
  onCancel,
  onCreated,
}: NewKnowledgeFormProps) {
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");

  const create = useMutation({
    mutationFn: () =>
      createKnowledgeBase(wsId, { name: name.trim(), description }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["knowledge", wsId] });
      // 团队详情里也摊着知识库目录，扇出刷新一次。
      void queryClient.invalidateQueries({ queryKey: ["team"] });
      toast.success(`已新建知识库「${name.trim()}」`);
      onCreated();
    },
    onError: (error) => toast.error(errorText(error)),
  });

  const trimmed = name.trim();
  const nameError = trimmed === "" ? null : kbNameError(trimmed);

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
          placeholder="例如 my-kb-01"
          disabled={create.isPending}
          autoComplete="off"
          spellCheck={false}
          className="h-8 font-mono text-xs"
        />
      </label>
      <p className="text-caption text-ink-faint">
        以字母或数字开头，仅可包含字母、数字、.、_、-，最多 64 个字符（不能以 @
        开头）。
      </p>
      {nameError && (
        <p className="text-caption font-medium text-danger">{nameError}</p>
      )}

      <label className="block space-y-1">
        <span className="text-caption font-medium text-ink-faint">描述</span>
        <Textarea
          value={description}
          onChange={(event) => setDescription(event.target.value)}
          disabled={create.isPending}
          className="min-h-[72px] text-sm"
        />
      </label>

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
          disabled={create.isPending || trimmed === "" || nameError !== null}
        >
          创建
        </Button>
      </DialogFooter>
    </form>
  );
}

interface NewKnowledgeDialogProps {
  wsId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function NewKnowledgeDialog({
  wsId,
  open,
  onOpenChange,
}: NewKnowledgeDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>新建知识库</DialogTitle>
          <DialogDescription>
            知识库按团队隔离，同团队内名称唯一。
          </DialogDescription>
        </DialogHeader>
        {/* 表单状态放在 DialogContent 里：关掉即卸载，下次打开是干净的一份。 */}
        <NewKnowledgeForm
          wsId={wsId}
          onCancel={() => onOpenChange(false)}
          onCreated={() => onOpenChange(false)}
        />
      </DialogContent>
    </Dialog>
  );
}
