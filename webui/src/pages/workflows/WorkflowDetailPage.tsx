import { useEffect, useRef, useState, useMemo } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { Button } from "@/components/ui/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { YamlEditor } from "@/components/workflow/YamlEditor";
import { DagPreview } from "@/components/workflow/DagPreview";
import { ExecutionHistoryTable } from "@/components/workflow/ExecutionHistoryTable";
import { StatisticsPanel } from "@/components/workflow/StatisticsPanel";
import { ScheduleConfigForm } from "@/components/workflow/ScheduleConfigForm";
import {
  getWorkflow,
  deleteWorkflow,
  executeWorkflow,
  listExecutions,
  getStatistics,
} from "@/api/workflows";
import {
  listSchedules,
  createSchedule,
  updateSchedule,
  deleteSchedule,
} from "@/api/schedules";
import type {
  WorkflowDetailResponse,
  ExecutionSummary,
  StatisticsResponse,
  ScheduleResponse,
  DagNode,
  DagEdge,
} from "@/types/workflow";
import {
  ArrowLeft,
  Play,
  Pencil,
  Trash2,
  Clock,
  RefreshCw,
} from "lucide-react";
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
    // 后端 ApiError 序列化为 { "error": "...", "details": "..." }
    if (err.response?.data?.details) return String(err.response.data.details);
    if (err.response?.data?.message) return String(err.response.data.message);
    if (err.message) return err.message;
  }
  if (err instanceof Error) return err.message;
  return undefined;
}

export function WorkflowDetailPage() {
  const navigate = useNavigate();
  const { name } = useParams<{ name: string }>();

  const [detail, setDetail] = useState<WorkflowDetailResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [executing, setExecuting] = useState(false);

  // Execution history
  const [executions, setExecutions] = useState<ExecutionSummary[]>([]);
  const [execTotal, setExecTotal] = useState(0);

  // Statistics
  const [stats, setStats] = useState<StatisticsResponse | null>(null);
  const [statsLoading, setStatsLoading] = useState(false);

  // Schedule
  const [schedule, setSchedule] = useState<ScheduleResponse | null>(null);
  const [scheduleSaving, setScheduleSaving] = useState(false);

  // Delete dialog
  const [deleteOpen, setDeleteOpen] = useState(false);

  const unmountedRef = useRef(false);

  const loadDetail = async () => {
    if (!name) return;
    try {
      const d = await getWorkflow(name);
      if (!unmountedRef.current) setDetail(d);
    } catch (err) {
      if (!unmountedRef.current)
        toast.error(getApiErrorMessage(err) || "加载工作流失败");
    } finally {
      if (!unmountedRef.current) setLoading(false);
    }
  };

  const loadExecutions = async () => {
    if (!name) return;
    try {
      const result = await listExecutions({ workflowName: name, limit: 10 });
      if (!unmountedRef.current) {
        setExecutions(result.executions);
        setExecTotal(result.total);
      }
    } catch {
      // Silently fail for executions
    }
  };

  const loadStats = async () => {
    if (!name) return;
    setStatsLoading(true);
    try {
      const s = await getStatistics(name);
      if (!unmountedRef.current) setStats(s);
    } catch {
      // Silently fail
    } finally {
      if (!unmountedRef.current) setStatsLoading(false);
    }
  };

  const loadSchedule = async () => {
    try {
      const schedules = await listSchedules();
      const match = schedules.find((s) => s.workflowName === name);
      if (!unmountedRef.current) setSchedule(match ?? null);
    } catch {
      // Silently fail
    }
  };

  useEffect(() => {
    unmountedRef.current = false;
    loadDetail();
    loadExecutions();
    loadSchedule();
    return () => {
      unmountedRef.current = true;
    };
  }, [name]);

  // Parse DAG from definition
  const dagData = useMemo(() => {
    if (!detail) return { nodes: [] as DagNode[], edges: [] as DagEdge[] };
    const nodes: DagNode[] = [];
    const edges: DagEdge[] = [];

    // Extract from YAML (best-effort)
    const yaml = detail.yaml;
    const stepIds = yaml.match(/^\s*- id:\s*(\S+)/gm);
    const stepNames = yaml.match(/^\s*name:\s*(\S.*)/gm);
    const stepTypes = yaml.match(/^\s*type:\s*(\S+)/gm);

    if (stepIds) {
      stepIds.forEach((m, i) => {
        const id = m.replace(/^\s*- id:\s*/, "").trim();
        nodes.push({
          id,
          name: stepNames?.[i]?.replace(/^\s*name:\s*/, "").trim() || id,
          type:
            (stepTypes?.[i]
              ?.replace(/^\s*type:\s*/, "")
              .trim() as DagNode["type"]) || "shell",
        });
      });
    }

    // Depends_on edges
    let nodeIdx = -1;
    for (const line of yaml.split("\n")) {
      if (line.match(/^\s*- id:/)) nodeIdx++;
      const depMatch = line.match(/^\s*depends_on:\s*\[(.*?)\]/);
      if (depMatch && nodeIdx >= 0 && nodeIdx < nodes.length) {
        const deps = depMatch[1]
          .split(",")
          .map((s) => s.trim().replace(/["']/g, ""))
          .filter(Boolean);
        for (const dep of deps) {
          edges.push({ from: dep, to: nodes[nodeIdx].id });
        }
      }
    }

    return { nodes, edges };
  }, [detail]);

  const handleExecute = async () => {
    if (!name) return;
    setExecuting(true);
    try {
      const result = await executeWorkflow(name);
      toast.success(`执行已触发 — ${result.runId.slice(0, 8)}...`);
      navigate(`/workflows/executions/${result.runId}`);
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "执行失败");
    } finally {
      if (!unmountedRef.current) setExecuting(false);
    }
  };

  const handleDelete = async () => {
    if (!name) return;
    try {
      await deleteWorkflow(name);
      toast.success("工作流已删除");
      navigate("/workflows");
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "删除失败");
    }
  };

  const handleScheduleSave = async (data: {
    cron: string;
    enabled: boolean;
    timezone?: string;
  }) => {
    if (!name) return;
    setScheduleSaving(true);
    try {
      if (schedule) {
        const updated = await updateSchedule(name, data);
        if (!unmountedRef.current) setSchedule(updated);
      } else {
        const created = await createSchedule({
          workflowName: name,
          ...data,
        });
        if (!unmountedRef.current) setSchedule(created);
      }
      toast.success(schedule ? "调度已更新" : "调度已创建");
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "保存调度失败");
    } finally {
      if (!unmountedRef.current) setScheduleSaving(false);
    }
  };

  const handleScheduleDelete = async () => {
    if (!name) return;
    try {
      await deleteSchedule(name);
      if (!unmountedRef.current) setSchedule(null);
      toast.success("调度已删除");
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "删除调度失败");
    }
  };

  if (loading) return <LoadingSpinner />;
  if (!detail) {
    return (
      <div className="text-center py-12 text-muted-foreground">
        工作流未找到
      </div>
    );
  }

  return (
    <div className="max-w-5xl mx-auto space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate("/workflows")}
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <h2 className="text-xl font-bold">{detail.name}</h2>
            {detail.description && (
              <p className="text-sm text-muted-foreground">
                {detail.description}
              </p>
            )}
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Button onClick={handleExecute} disabled={executing}>
            <Play className="mr-2 h-4 w-4" />
            {executing ? "触发中..." : "执行"}
          </Button>
          <Button
            variant="outline"
            onClick={() =>
              navigate(`/workflows/${encodeURIComponent(detail.name)}/edit`)
            }
          >
            <Pencil className="mr-2 h-4 w-4" />
            编辑
          </Button>
          <Button
            variant="ghost"
            size="icon"
            onClick={() => setDeleteOpen(true)}
          >
            <Trash2 className="h-4 w-4 text-destructive" />
          </Button>
        </div>
      </div>

      <Tabs defaultValue="overview">
        <TabsList>
          <TabsTrigger value="overview">概览</TabsTrigger>
          <TabsTrigger value="history">执行历史</TabsTrigger>
          <TabsTrigger
            value="statistics"
            onClick={() => {
              if (!stats) loadStats();
            }}
          >
            统计
          </TabsTrigger>
          <TabsTrigger value="definition">定义</TabsTrigger>
        </TabsList>

        {/* Tab: Overview */}
        <TabsContent value="overview" className="space-y-4 mt-4">
          {/* DAG */}
          {dagData.nodes.length > 0 && (
            <div className="border rounded-md p-4">
              <DagPreview
                nodes={dagData.nodes}
                edges={dagData.edges}
                className="w-full"
              />
            </div>
          )}

          {/* Schedule */}
          <div className="flex items-start gap-3 p-4 border rounded-md">
            <Clock className="h-5 w-5 text-muted-foreground mt-0.5" />
            <div className="flex-1">
              <h4 className="text-sm font-medium">调度状态</h4>
              {schedule ? (
                <p className="text-xs text-muted-foreground mt-1">
                  {schedule.cron}
                  {" · "}
                  {schedule.enabled ? (
                    <span className="text-green-600 font-medium">已启用</span>
                  ) : (
                    <span className="text-amber-600 font-medium">已暂停</span>
                  )}
                  {schedule.timezone && ` · ${schedule.timezone}`}
                </p>
              ) : (
                <p className="text-xs text-muted-foreground mt-1">
                  未配置定时调度 — 仅支持手动触发
                </p>
              )}
            </div>
            <div>
              <ScheduleConfigForm
                workflowName={detail.name}
                existing={schedule}
                onSave={handleScheduleSave}
                onDelete={schedule ? handleScheduleDelete : undefined}
                saving={scheduleSaving}
              />
            </div>
          </div>

          {/* Recent executions */}
          <div>
            <div className="flex items-center justify-between mb-2">
              <h4 className="text-sm font-medium">最近执行</h4>
              {execTotal > 0 && (
                <span className="text-xs text-muted-foreground">
                  共 {execTotal} 条记录
                </span>
              )}
            </div>
            <ExecutionHistoryTable
              executions={executions.slice(0, 5)}
              onViewRun={(runId) => navigate(`/workflows/executions/${runId}`)}
            />
          </div>
        </TabsContent>

        {/* Tab: History */}
        <TabsContent value="history" className="mt-4">
          <ExecutionHistoryTable
            executions={executions}
            onViewRun={(runId) => navigate(`/workflows/executions/${runId}`)}
          />
        </TabsContent>

        {/* Tab: Statistics */}
        <TabsContent value="statistics" className="mt-4">
          <div className="flex items-center justify-between mb-4">
            <h4 className="text-sm font-medium">统计信息</h4>
            <Button variant="ghost" size="sm" onClick={loadStats}>
              <RefreshCw className="mr-1 h-3 w-3" />
              刷新
            </Button>
          </div>
          <StatisticsPanel stats={stats} loading={statsLoading} />
        </TabsContent>

        {/* Tab: Definition (read-only YAML) */}
        <TabsContent value="definition" className="mt-4">
          <YamlEditor
            value={detail.yaml}
            onChange={() => {}}
            readOnly
            className="min-h-[400px]"
          />
        </TabsContent>
      </Tabs>

      {/* Delete Dialog */}
      <Dialog open={deleteOpen} onOpenChange={setDeleteOpen}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>确认删除</DialogTitle>
            <DialogDescription>
              确定要删除工作流{" "}
              <span className="font-semibold">{detail.name}</span>{" "}
              吗？此操作不可撤销。执行历史记录将保留。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteOpen(false)}>
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
