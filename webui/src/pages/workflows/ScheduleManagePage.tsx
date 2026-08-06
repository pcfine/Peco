import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Button } from "@/components/ui/button";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { EmptyState } from "@/components/common/EmptyState";
import { ScheduleConfigForm } from "@/components/workflow/ScheduleConfigForm";
import {
  listSchedules,
  createSchedule,
  updateSchedule,
  deleteSchedule,
} from "@/api/schedules";
import type { ScheduleResponse } from "@/types/workflow";
import { Clock, Trash2, ExternalLink, ChevronRight } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { toast } from "sonner";
import axios from "axios";

function getApiErrorMessage(err: unknown): string | undefined {
  if (axios.isAxiosError(err)) {
    if (err.response?.data?.message) return String(err.response.data.message);
    if (err.message) return err.message;
  }
  if (err instanceof Error) return err.message;
  return undefined;
}

export function ScheduleManagePage() {
  const navigate = useNavigate();
  const [schedules, setSchedules] = useState<ScheduleResponse[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<ScheduleResponse | null>(
    null,
  );
  const [showCreate, setShowCreate] = useState(false);
  const [newWorkflowName, setNewWorkflowName] = useState("");
  const unmountedRef = useRef(false);

  const load = async () => {
    try {
      const list = await listSchedules();
      if (!unmountedRef.current) setSchedules(list);
    } catch {
      if (!unmountedRef.current) toast.error("加载调度列表失败");
    } finally {
      if (!unmountedRef.current) setLoading(false);
    }
  };

  useEffect(() => {
    unmountedRef.current = false;
    load();
    return () => {
      unmountedRef.current = true;
    };
  }, []);

  const handleCreate = async (data: {
    cron: string;
    enabled: boolean;
    timezone?: string;
  }) => {
    if (!newWorkflowName.trim()) return;
    setSaving(true);
    try {
      const created = await createSchedule({
        workflowName: newWorkflowName.trim(),
        ...data,
      });
      setSchedules((prev) => [...prev, created]);
      setShowCreate(false);
      setNewWorkflowName("");
      toast.success("调度已创建");
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "创建失败");
    } finally {
      if (!unmountedRef.current) setSaving(false);
    }
  };

  const handleUpdate = async (
    workflowName: string,
    data: { cron: string; enabled: boolean; timezone?: string },
  ) => {
    setSaving(true);
    try {
      const updated = await updateSchedule(workflowName, data);
      setSchedules((prev) =>
        prev.map((s) =>
          s.workflowName === workflowName ? { ...s, ...updated } : s,
        ),
      );
      toast.success("调度已更新");
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "更新失败");
    } finally {
      if (!unmountedRef.current) setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!deleteTarget) return;
    try {
      await deleteSchedule(deleteTarget.workflowName);
      setSchedules((prev) =>
        prev.filter((s) => s.workflowName !== deleteTarget.workflowName),
      );
      toast.success("调度已删除");
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "删除失败");
    } finally {
      setDeleteTarget(null);
    }
  };

  if (loading) return <LoadingSpinner />;

  return (
    <div className="max-w-3xl mx-auto space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-bold">定时调度管理</h2>
        <Button onClick={() => setShowCreate(true)}>+ 新建调度</Button>
      </div>

      {/* Create new schedule form */}
      {showCreate && (
        <div className="border rounded-md p-4 space-y-3">
          <h3 className="text-sm font-medium">新建调度</h3>
          <div className="space-y-1">
            <label className="text-xs">Workflow 名称</label>
            <input
              className="w-full h-8 rounded-md border px-3 text-xs"
              value={newWorkflowName}
              onChange={(e) => setNewWorkflowName(e.target.value)}
              placeholder="my-workflow"
            />
          </div>
          <ScheduleConfigForm
            workflowName={newWorkflowName}
            onSave={handleCreate}
            saving={saving}
          />
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setShowCreate(false)}
          >
            取消
          </Button>
        </div>
      )}

      {/* Empty state */}
      {schedules.length === 0 && !showCreate && (
        <EmptyState
          icon={Clock}
          title="暂无定时调度"
          description="为工作流添加定时调度，自动执行重复任务"
          action={
            <Button onClick={() => setShowCreate(true)}>+ 新建调度</Button>
          }
        />
      )}

      {/* Schedule list */}
      {schedules.length > 0 && (
        <div className="space-y-2">
          {schedules.map((sched) => (
            <div
              key={sched.workflowName}
              className="flex items-center gap-3 p-3 border rounded-md hover:border-primary/30 transition-colors"
            >
              <Clock className="h-5 w-5 text-muted-foreground shrink-0" />
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <button
                    className="font-medium text-sm hover:underline text-left truncate"
                    onClick={() =>
                      navigate(
                        `/workflows/${encodeURIComponent(sched.workflowName)}`,
                      )
                    }
                  >
                    {sched.workflowName}
                    <ExternalLink className="inline h-3 w-3 ml-1 text-muted-foreground" />
                  </button>
                  <span
                    className={`text-xs px-1.5 py-0.5 rounded ${
                      sched.enabled
                        ? "bg-green-100 text-green-700"
                        : "bg-amber-100 text-amber-700"
                    }`}
                  >
                    {sched.enabled ? "已启用" : "已暂停"}
                  </span>
                </div>
                <p className="text-xs text-muted-foreground font-mono mt-0.5">
                  {sched.cron}
                  {sched.timezone && ` (${sched.timezone})`}
                </p>
                <ScheduleConfigForm
                  workflowName={sched.workflowName}
                  existing={sched}
                  onSave={(data) => handleUpdate(sched.workflowName, data)}
                  onDelete={() => setDeleteTarget(sched)}
                  saving={saving}
                />
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Delete Confirmation Dialog */}
      <Dialog open={!!deleteTarget} onOpenChange={() => setDeleteTarget(null)}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>确认删除调度</DialogTitle>
            <DialogDescription>
              确定要删除{" "}
              <span className="font-semibold">
                {deleteTarget?.workflowName}
              </span>{" "}
              的定时调度吗？Workflow 本身不会被删除。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>
              取消
            </Button>
            <Button variant="destructive" onClick={handleDelete}>
              删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
