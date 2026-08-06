import { useEffect, useRef, useState, useMemo, useCallback } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { YamlEditor } from "@/components/workflow/YamlEditor";
import { DagPreview } from "@/components/workflow/DagPreview";
import { StepConfigForm } from "@/components/workflow/StepConfigForm";
import type { StepFormData } from "@/components/workflow/StepConfigForm";
import type { DagNode, DagEdge } from "@/types/workflow";
import { getWorkflow, createWorkflow, updateWorkflow } from "@/api/workflows";
import { ArrowLeft, Save, Eye, Code2, FormInput } from "lucide-react";
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

type EditorMode = "form" | "yaml";

const DEFAULT_YAML = `---
workflow:
  name: ""
  description: ""
  version: "1.0"
  timeout_seconds: 600
  steps: []
---`;

export function WorkflowEditorPage() {
  const navigate = useNavigate();
  const { name } = useParams<{ name: string }>();
  const isEdit = !!name;

  const [loading, setLoading] = useState(isEdit);
  const [saving, setSaving] = useState(false);
  const [yaml, setYaml] = useState(DEFAULT_YAML);
  const [mode, setMode] = useState<EditorMode>("yaml");
  const unmountedRef = useRef(false);

  // Load existing workflow for editing
  useEffect(() => {
    if (!name) return;
    unmountedRef.current = false;
    getWorkflow(name)
      .then((detail) => {
        if (!unmountedRef.current) setYaml(detail.yaml);
      })
      .catch((err) => {
        if (!unmountedRef.current)
          toast.error(getApiErrorMessage(err) || "加载工作流失败");
      })
      .finally(() => {
        if (!unmountedRef.current) setLoading(false);
      });
    return () => {
      unmountedRef.current = true;
    };
  }, [name]);

  // Parse DAG from YAML (best-effort)
  const dagData = useMemo(() => {
    try {
      const parsed: any = (() => {
        // Simple YAML-like parsing for the workflow section
        // Just try JSON parse after finding the workflow object
        // For now, we do a basic extraction of steps
        return null;
      })();

      // Try to parse with serde-like extraction
      const nodes: DagNode[] = [];
      const edges: DagEdge[] = [];

      // Extract steps from YAML using regex (simplistic but works for preview)
      const stepMatches = yaml.match(/^\s*- id:\s*(\S+)/gm);
      const stepNames = yaml.match(/^\s*name:\s*(\S.*)/gm);
      const stepTypes = yaml.match(/^\s*type:\s*(\S+)/gm);

      if (stepMatches) {
        stepMatches.forEach((m, i) => {
          const id = m.replace(/^\s*- id:\s*/, "").trim();
          const nameMatch = stepNames?.[i]?.replace(/^\s*name:\s*/, "").trim();
          const typeMatch = stepTypes?.find((t) => {
            // Crude: match type that appears after this step's id
            const tIdx = yaml.indexOf(t);
            const mIdx = yaml.indexOf(m);
            // Find next step's "- id:" or end of string
            const nextStep = stepMatches
              .map((s) => yaml.indexOf(s))
              .find((idx) => idx > mIdx);
            const sectionEnd = nextStep ?? yaml.length;
            return tIdx > mIdx && tIdx < sectionEnd;
          });

          nodes.push({
            id: id || `step_${i}`,
            name: nameMatch || id || `Step ${i + 1}`,
            type:
              (typeMatch
                ?.replace(/^\s*type:\s*/, "")
                .trim() as DagNode["type"]) || "shell",
          });
        });

        // Extract depends_on edges
        const depMatches = yaml.match(/^\s*depends_on:\s*\[(.*?)\]/gm);
        if (depMatches) {
          // Find which step each depends_on belongs to
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
        }
      }

      return { nodes, edges };
    } catch {
      return { nodes: [] as DagNode[], edges: [] as DagEdge[] };
    }
  }, [yaml]);

  const handleSave = async () => {
    setSaving(true);
    try {
      if (isEdit && name) {
        await updateWorkflow(name, { yaml });
        toast.success("工作流已更新");
      } else {
        const result = await createWorkflow({ yaml });
        toast.success("工作流已创建");
        navigate(`/workflows/${encodeURIComponent(result.name)}`, {
          replace: true,
        });
      }
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "保存失败");
    } finally {
      if (!unmountedRef.current) setSaving(false);
    }
  };

  if (loading) return <LoadingSpinner />;

  return (
    <div className="max-w-7xl mx-auto space-y-4">
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
          <h2 className="text-xl font-bold">
            {isEdit ? `编辑 ${name}` : "新建工作流"}
          </h2>
        </div>
        <div className="flex items-center gap-2">
          <div className="flex items-center border rounded-md p-0.5 mr-2">
            <button
              type="button"
              className={`px-2.5 py-1 text-xs rounded-sm transition-colors ${
                mode === "form"
                  ? "bg-primary text-primary-foreground"
                  : "hover:bg-muted"
              }`}
              onClick={() => setMode("form")}
            >
              <FormInput className="h-3.5 w-3.5 inline mr-1" />
              表单
            </button>
            <button
              type="button"
              className={`px-2.5 py-1 text-xs rounded-sm transition-colors ${
                mode === "yaml"
                  ? "bg-primary text-primary-foreground"
                  : "hover:bg-muted"
              }`}
              onClick={() => setMode("yaml")}
            >
              <Code2 className="h-3.5 w-3.5 inline mr-1" />
              YAML
            </button>
          </div>
          <Button onClick={handleSave} disabled={saving}>
            <Save className="mr-2 h-4 w-4" />
            {saving ? "保存中..." : "保存"}
          </Button>
        </div>
      </div>

      {/* Editor + Preview */}
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        {/* Left: Editor */}
        <div className="space-y-3">
          {mode === "yaml" ? (
            <YamlEditor
              value={yaml}
              onChange={setYaml}
              className="min-h-[500px]"
              placeholder={DEFAULT_YAML}
            />
          ) : (
            <div className="border rounded-md p-4 min-h-[500px] space-y-4">
              {/* Basic settings */}
              <div className="space-y-3">
                <h4 className="text-sm font-medium">基本设置</h4>
                <div className="grid grid-cols-2 gap-3">
                  <div className="space-y-1">
                    <Label className="text-xs">名称</Label>
                    <Input className="h-8 text-xs" placeholder="my-workflow" />
                  </div>
                  <div className="space-y-1">
                    <Label className="text-xs">版本</Label>
                    <Input className="h-8 text-xs" placeholder="1.0" />
                  </div>
                  <div className="space-y-1 col-span-2">
                    <Label className="text-xs">描述</Label>
                    <Input className="h-8 text-xs" placeholder="工作流描述" />
                  </div>
                </div>
              </div>

              {/* Steps form (Beta) */}
              <StepConfigForm steps={[]} allStepIds={[]} onChange={() => {}} />

              {/* Beta warning */}
              <p className="text-xs text-amber-600 bg-amber-50 rounded p-2">
                ⚠️ 表单模式目前为 Beta 版本。复杂工作流建议使用 YAML 模式编辑。
              </p>
            </div>
          )}
        </div>

        {/* Right: DAG Preview */}
        <div className="space-y-3">
          <div className="flex items-center gap-2">
            <Eye className="h-4 w-4 text-muted-foreground" />
            <span className="text-sm font-medium">DAG 预览</span>
            <span className="text-xs text-muted-foreground">
              {dagData.nodes.length} 步骤
            </span>
          </div>
          <div className="border rounded-md p-4 bg-background min-h-[300px]">
            <DagPreview
              nodes={dagData.nodes}
              edges={dagData.edges}
              className="w-full"
            />
          </div>

          {/* Info */}
          <div className="text-xs text-muted-foreground space-y-1">
            <p>
              💡 <strong>提示：</strong>调度配置在 Workflow 详情页单独管理，
              不出现在 workflow.md 定义中。
            </p>
            <p>
              📖 步骤类型：<code>shell</code>（命令行）、<code>agent</code>（AI
              Agent）、
              <code>llm</code>（纯 LLM 调用）、<code>tool</code>（工具调用）
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}
