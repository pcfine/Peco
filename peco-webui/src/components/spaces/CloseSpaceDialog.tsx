// 关闭空间被 409 拦下时的二次确认。两个动作的代价不同：
// 「等待完成」什么都不做，「强制关闭」会把在飞任务置 orphan —— 措辞不能含糊。

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
import { errorText } from "@/lib/api";
import type { InflightCounts } from "@/components/spaces/useSpaceLifecycle";

interface CloseSpaceDialogProps {
  open: boolean;
  inflight: InflightCounts | null;
  onWait: () => void;
  onForce: () => void;
  forcing: boolean;
  error: unknown;
}

export function CloseSpaceDialog({
  open,
  inflight,
  onWait,
  onForce,
  forcing,
  error,
}: CloseSpaceDialogProps) {
  const total = (inflight?.dispatch ?? 0) + (inflight?.foreign ?? 0);
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onWait()}>
      <DialogContent className="max-w-[420px]">
        <DialogHeader>
          <DialogTitle>仍有 {total} 个任务在飞</DialogTitle>
          <DialogDescription>
            Peco 派发 {inflight?.dispatch ?? 0} 个，外来{" "}
            {inflight?.foreign ?? 0} 个。等待完成不会打断它们；强制关闭会把它们
            记为孤儿。
          </DialogDescription>
        </DialogHeader>
        {error ? <ErrorBanner message={errorText(error)} /> : null}
        <DialogFooter>
          <Button variant="outline" size="sm" onClick={onWait}>
            等待完成
          </Button>
          <Button
            variant="outline"
            size="sm"
            className="border-danger/40 text-danger"
            disabled={forcing}
            onClick={onForce}
          >
            强制关闭
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
