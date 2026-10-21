// 删除二次确认弹层 —— 纯展示，不含请求：两个入口（文档 / 实体）共用同一形态。

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

interface MemoryDeleteDialogProps {
  open: boolean;
  title: string;
  description: React.ReactNode;
  /** 确认按钮文案，默认「删除」。 */
  confirmLabel?: string;
  /** 提交中：两个按钮都禁用，确认按钮改文案，杜绝并发重复提交。 */
  busy: boolean;
  /** 失败留在弹层内（用户可原地重试或取消），不关层。 */
  error: string | null;
  onCancel: () => void;
  onConfirm: () => void;
  testId: string;
}

export function MemoryDeleteDialog({
  open,
  title,
  description,
  confirmLabel = "删除",
  busy,
  error,
  onCancel,
  onConfirm,
  testId,
}: MemoryDeleteDialogProps) {
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        // 提交中不允许 Esc / 点遮罩关闭：请求已在途，关层只会让结果无处落地。
        if (!next && !busy) onCancel();
      }}
    >
      <DialogContent className="sm:max-w-md" data-testid={testId}>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        {error && <p className="text-sm text-destructive">{error}</p>}
        <DialogFooter>
          <Button variant="outline" disabled={busy} onClick={onCancel}>
            取消
          </Button>
          <Button
            variant="destructive"
            disabled={busy}
            data-testid={`${testId}-confirm`}
            onClick={onConfirm}
          >
            {busy ? "删除中…" : confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
