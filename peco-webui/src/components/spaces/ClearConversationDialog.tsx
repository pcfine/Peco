// 清除对话的二次确认。清的是消息，会话与已产出的成果留着 —— 措辞要把这两件事
// 分开说，否则用户会以为「成果」区也跟着清空。

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";

interface ClearConversationDialogProps {
  open: boolean;
  pending: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}

export function ClearConversationDialog({
  open,
  pending,
  onCancel,
  onConfirm,
}: ClearConversationDialogProps) {
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => !next && !pending && onCancel()}
    >
      <DialogContent className="max-w-narrow">
        <DialogHeader>
          <DialogTitle>清除这段对话？</DialogTitle>
          <DialogDescription>
            当前对话的全部消息会被清空，会话与已产出的成果保留。此操作不可撤销。
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button
            variant="ghost"
            size="sm"
            disabled={pending}
            onClick={onCancel}
          >
            取消
          </Button>
          <Button
            variant="destructive"
            size="sm"
            disabled={pending}
            onClick={onConfirm}
          >
            清除对话
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
