// 删除团队确认弹窗，只用于有绑定空间的团队。

import { useState } from "react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { errorText } from "@/lib/api";
import { useDeleteTeam } from "@/queries/teams";
import type { Team } from "@/types/daemon";

interface DeleteTeamDialogProps {
  /** `null` 表示关闭；非空即打开，并展示该团队。 */
  team: Team | null;
  onOpenChange: (open: boolean) => void;
  onDeleted: () => void;
}

export function DeleteTeamDialog({
  team,
  onOpenChange,
  onDeleted,
}: DeleteTeamDialogProps) {
  const remove = useDeleteTeam();
  const [error, setError] = useState<unknown>(null);
  const spaces = team?.bound_spaces ?? [];

  function close(next: boolean) {
    if (!next) setError(null);
    onOpenChange(next);
  }

  function confirm() {
    if (!team) return;
    setError(null);
    remove.mutate(team.team_id, {
      onSuccess: () => onDeleted(),
      onError: (mutationError) => setError(mutationError),
    });
  }

  return (
    <Dialog open={team !== null} onOpenChange={close}>
      <DialogContent className="max-w-narrow">
        <DialogHeader>
          <DialogTitle>删除团队「{team?.display_name}」？</DialogTitle>
          <DialogDescription>
            团队的能力目录与索引会被删除，此操作不可撤销。
          </DialogDescription>
        </DialogHeader>

        {spaces.length > 0 && (
          <div className="space-y-2 rounded-[6px] border border-destructive/40 bg-destructive/10 p-3">
            <p className="text-xs font-medium text-ink">
              这 {spaces.length} 个工作空间会从空间索引中移除并失效：
            </p>
            <ul className="space-y-1">
              {spaces.map((wsId) => (
                <li
                  key={wsId}
                  className="font-mono text-mono-sm text-ink-muted"
                >
                  {wsId}
                </li>
              ))}
            </ul>
            <p className="text-caption font-medium text-ink-faint">
              工作目录与 `.workspace.json` 不会被删除。
            </p>
          </div>
        )}

        {error != null && <ErrorBanner message={errorText(error)} />}

        <DialogFooter>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={remove.isPending}
            onClick={() => close(false)}
          >
            取消
          </Button>
          <Button
            type="button"
            variant="destructive"
            size="sm"
            disabled={remove.isPending}
            onClick={confirm}
          >
            {remove.isPending ? "删除中…" : "删除团队"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
