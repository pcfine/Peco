// `/schedules` —— 定时任务管理：列表、新建/编辑、立即运行、运行历史、删除。

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
import { PageFrame } from "@/components/layout/PageFrame";
import { ScheduleFormDialog } from "@/components/schedules/ScheduleFormDialog";
import { ScheduleList } from "@/components/schedules/ScheduleList";
import { useDeleteSchedule, useSchedules } from "@/queries/schedules";
import type { ScheduleView } from "@/types/daemon";

/** 删除的二次确认：历史记录随之消失这件事必须写在文案里。 */
function DeleteScheduleDialog({
  schedule,
  onClose,
}: {
  schedule: ScheduleView;
  onClose: () => void;
}) {
  const remove = useDeleteSchedule();

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next && !remove.isPending) onClose();
      }}
    >
      <DialogContent className="max-w-narrow">
        <DialogHeader>
          <DialogTitle>删除「{schedule.name}」？</DialogTitle>
          <DialogDescription>
            该任务的运行历史记录一并删除，正在跑的那一轮会被中断。此操作不可撤销。
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button
            variant="ghost"
            size="sm"
            disabled={remove.isPending}
            onClick={onClose}
          >
            取消
          </Button>
          <Button
            variant="destructive"
            size="sm"
            disabled={remove.isPending}
            onClick={() => remove.mutate(schedule.id, { onSuccess: onClose })}
          >
            删除定时任务
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function SchedulesPage() {
  const schedules = useSchedules();
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<ScheduleView | null>(null);
  const [deleting, setDeleting] = useState<ScheduleView | null>(null);

  const openCreate = () => {
    setEditing(null);
    setFormOpen(true);
  };

  const openEdit = (schedule: ScheduleView) => {
    setEditing(schedule);
    setFormOpen(true);
  };

  return (
    <PageFrame
      title="定时任务"
      contentWidth="mid"
      actions={
        <Button size="sm" onClick={openCreate}>
          新建定时任务
        </Button>
      }
    >
      <ScheduleList
        schedules={schedules.data ?? []}
        isPending={schedules.isPending}
        error={schedules.error}
        onRetry={() => void schedules.refetch()}
        onEdit={openEdit}
        onDelete={setDeleting}
      />

      <ScheduleFormDialog
        open={formOpen}
        schedule={editing}
        onOpenChange={setFormOpen}
      />

      {deleting && (
        <DeleteScheduleDialog
          schedule={deleting}
          onClose={() => setDeleting(null)}
        />
      )}
    </PageFrame>
  );
}
