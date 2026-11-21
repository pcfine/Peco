// 两个删除动作的确认：移入回收站可撤销，永久销毁不可撤销 —— 代价不同，文案必须
// 各自把「什么会留下、什么会消失」写全，用户不该靠猜。
//
// 被在飞任务门槛拦下时这个弹窗不关：它就地改成「强制」，让用户看清是哪些任务挡着的。

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { inflightGate } from "@/components/spaces/useSpaceLifecycle";
import { errorText } from "@/lib/api";

type RemovalMode = "trash" | "purge";

const COPY: Record<
  RemovalMode,
  {
    title: (name: string) => string;
    description: string;
    confirm: string;
    forced: string;
  }
> = {
  trash: {
    title: (name) => `把「${name}」移入回收站？`,
    description: "会话与记忆将保留在回收站；工作目录里的文件不会被删除。",
    confirm: "移入回收站",
    forced: "强制移入回收站",
  },
  purge: {
    title: (name) => `永久销毁「${name}」？`,
    description: "会话与记忆将被删除；工作目录里的文件会保留。此操作不可撤销。",
    confirm: "永久删除",
    forced: "强制永久删除",
  },
};

interface SpaceRemovalDialogProps {
  name: string;
  mode: RemovalMode;
  /** 用户确认；`force` 是门槛拦下之后的第二次确认。 */
  run: (force: boolean) => void;
  pending: boolean;
  error: unknown;
  onClose: () => void;
}

export function SpaceRemovalDialog({
  name,
  mode,
  run,
  pending,
  error,
  onClose,
}: SpaceRemovalDialogProps) {
  const copy = COPY[mode];
  const gate = inflightGate(error);
  return (
    <Dialog
      open
      onOpenChange={(next) => {
        // 请求在途时不许点外面关掉：结果已经和这个弹窗绑在一起了。
        if (!next && !pending) onClose();
      }}
    >
      <DialogContent className="max-w-narrow">
        <DialogHeader>
          <DialogTitle>{copy.title(name)}</DialogTitle>
          <DialogDescription>{copy.description}</DialogDescription>
        </DialogHeader>
        {gate && (
          <p className="text-sm text-ink-muted">
            仍有 {gate.dispatch + gate.foreign} 个任务在飞（Peco 派发{" "}
            {gate.dispatch} 个，外来 {gate.foreign}{" "}
            个）。强制意味着不等它们跑完。
          </p>
        )}
        {error ? <ErrorBanner message={errorText(error)} /> : null}
        <DialogFooter>
          <Button
            variant="ghost"
            size="sm"
            disabled={pending}
            onClick={onClose}
          >
            取消
          </Button>
          <Button
            variant="destructive"
            size="sm"
            disabled={pending}
            onClick={() => run(gate !== null)}
          >
            {gate ? copy.forced : copy.confirm}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
