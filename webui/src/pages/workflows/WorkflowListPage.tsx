import { useEffect, useRef, useState, useMemo } from "react";
import { useNavigate } from "react-router-dom";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { EmptyState } from "@/components/common/EmptyState";
import { PageHeader } from "@/components/common/PageHeader";
import { PageContainer } from "@/components/common/PageContainer";
import { CardGrid } from "@/components/common/CardGrid";
import { WorkflowCard } from "@/components/workflow/WorkflowCard";
import { listWorkflows, executeWorkflow } from "@/api/workflows";
import type { WorkflowListItem } from "@/types/workflow";
import { Plus, Search, Workflow } from "lucide-react";
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

export function WorkflowListPage() {
  const navigate = useNavigate();
  const [workflows, setWorkflows] = useState<WorkflowListItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [search, setSearch] = useState("");
  const [executing, setExecuting] = useState<string | null>(null);
  const unmountedRef = useRef(false);

  const load = () => {
    listWorkflows()
      .then((list) => {
        if (!unmountedRef.current) setWorkflows(list);
      })
      .catch(() => {
        if (!unmountedRef.current) toast.error("加载工作流列表失败");
      })
      .finally(() => {
        if (!unmountedRef.current) setLoading(false);
      });
  };

  useEffect(() => {
    unmountedRef.current = false;
    load();
    return () => {
      unmountedRef.current = true;
    };
  }, []);

  const filtered = useMemo(() => {
    if (!search.trim()) return workflows;
    const q = search.toLowerCase();
    return workflows.filter(
      (w) =>
        w.name.toLowerCase().includes(q) ||
        w.description.toLowerCase().includes(q),
    );
  }, [workflows, search]);

  const handleExecute = async (name: string) => {
    setExecuting(name);
    try {
      const result = await executeWorkflow(name);
      toast.success(`执行已触发 — ${result.runId.slice(0, 8)}...`);
      navigate(`/workflows/executions/${result.runId}`);
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "执行失败");
    } finally {
      if (!unmountedRef.current) setExecuting(null);
    }
  };

  const handleEdit = (name: string) => {
    navigate(`/workflows/${encodeURIComponent(name)}/edit`);
  };

  const handleView = (name: string) => {
    navigate(`/workflows/${encodeURIComponent(name)}`);
  };

  // Compute summary stats
  const runningCount = filtered.filter(
    (w) => w.lastExecution?.status === "running",
  ).length;
  const enabledScheduleCount = filtered.filter(
    (w) => w.schedule?.enabled,
  ).length;
  const totalExecs = filtered.reduce(
    (sum, w) => sum + (w.lastExecution ? 1 : 0),
    0,
  );

  if (loading) return <LoadingSpinner />;

  return (
    <PageContainer>
      {/* Header */}
      <PageHeader
        title="工作流"
        actions={
          <Button onClick={() => navigate("/workflows/new")}>
            <Plus className="mr-2 h-4 w-4" />
            新建工作流
          </Button>
        }
      />

      {/* 汇总条：细线分隔的四段，避免宽屏下四个空荡的居中数字卡 */}
      <div className="grid grid-cols-2 gap-px overflow-hidden rounded-lg border bg-border @min-[740px]:grid-cols-4">
        <div className="bg-card px-4 py-3">
          <p className="text-2xl font-semibold leading-none tabular-nums">
            {filtered.length}
          </p>
          <p className="mt-1.5 text-xs text-muted-foreground">全部</p>
        </div>
        <div className="bg-card px-4 py-3">
          <p className="text-2xl font-semibold leading-none tabular-nums">
            {runningCount}
          </p>
          <p className="mt-1.5 text-xs text-muted-foreground">运行中</p>
        </div>
        <div className="bg-card px-4 py-3">
          <p className="text-2xl font-semibold leading-none tabular-nums">
            {enabledScheduleCount}
          </p>
          <p className="mt-1.5 text-xs text-muted-foreground">已启用调度</p>
        </div>
        <div className="bg-card px-4 py-3">
          <p className="text-2xl font-semibold leading-none tabular-nums">
            {totalExecs}
          </p>
          <p className="mt-1.5 text-xs text-muted-foreground">有执行记录</p>
        </div>
      </div>

      {/* Search */}
      <div className="relative">
        <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
        <Input
          className="pl-9 h-9"
          placeholder="搜索工作流..."
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
      </div>

      {/* Empty state */}
      {filtered.length === 0 && !search && (
        <EmptyState
          icon={Workflow}
          title="暂无工作流"
          description="创建您的第一个工作流，统一管理手动触发和定时执行"
          action={
            <Button onClick={() => navigate("/workflows/new")}>
              <Plus className="mr-2 h-4 w-4" />
              新建工作流
            </Button>
          }
        />
      )}

      {filtered.length === 0 && search && (
        <div className="text-center py-12 text-muted-foreground text-sm">
          未找到匹配 "{search}" 的工作流
        </div>
      )}

      {/* Card grid */}
      {filtered.length > 0 && (
        <CardGrid>
          {filtered.map((wf) => (
            <WorkflowCard
              key={wf.name}
              workflow={wf}
              onExecute={handleExecute}
              onEdit={handleEdit}
              onView={handleView}
            />
          ))}
        </CardGrid>
      )}
    </PageContainer>
  );
}
