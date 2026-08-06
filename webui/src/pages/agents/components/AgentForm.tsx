import { useEffect, useRef, useState } from "react";
import { useForm, useController } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Slider } from "@/components/ui/slider";
import { Card, CardContent } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { agentSchema } from "@/lib/validators";
import { MODELS, PROVIDERS, TOOLS } from "@/lib/constants";
import { listSkills } from "@/api/skills";
import { listKnowledgeBases } from "@/api/knowledge";
import { getMcpConfig } from "@/api/mcp";
import api from "@/api/client";
import type { AgentDetail, CreateAgentRequest } from "@/types/agent";
import type { SkillListItem } from "@/types/skill";
import type { KnowledgeBase } from "@/types/knowledge";
import type { McpConfigResponse } from "@/types/mcp";
import {
  Upload,
  Dice5,
  RefreshCw,
  AlertCircle,
  ExternalLink,
} from "lucide-react";
import { toast } from "sonner";

// ── Reasoning effort options ──────────────────────────────────────────────

const REASONING_OPTIONS = [
  { value: "", label: "默认（使用 Provider 默认值）" },
  { value: "disabled", label: "disabled — 禁用推理" },
  { value: "low", label: "low — 低强度推理" },
  { value: "medium", label: "medium — 中等强度推理" },
  { value: "high", label: "high — 高强度推理" },
  { value: "xhigh", label: "xhigh — 极高强度推理" },
  { value: "max", label: "max — 最大强度推理" },
];

// ── Random pastel color ────────────────────────────────────────────────────

function randomPastel(): string {
  const h = Math.floor(Math.random() * 360);
  const s = 30 + Math.floor(Math.random() * 20);
  const l = 55 + Math.floor(Math.random() * 15);
  return `hsl(${h}, ${s}%, ${l}%)`;
}

// ── Client-side image resize ──────────────────────────────────────────────

async function resizeImage(file: File, maxSize = 256): Promise<Blob> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => {
      URL.revokeObjectURL(img.src);
      const size = Math.min(img.width, img.height);
      const sx = (img.width - size) / 2;
      const sy = (img.height - size) / 2;
      const canvas = document.createElement("canvas");
      canvas.width = maxSize;
      canvas.height = maxSize;
      const ctx = canvas.getContext("2d")!;
      ctx.drawImage(img, sx, sy, size, size, 0, 0, maxSize, maxSize);
      canvas.toBlob((blob) => {
        if (blob) resolve(blob);
        else reject(new Error("Canvas toBlob failed"));
      }, "image/png");
    };
    img.onerror = () => {
      URL.revokeObjectURL(img.src);
      reject(new Error("Image load failed"));
    };
    img.src = URL.createObjectURL(file);
  });
}

// ── Icon helpers ──────────────────────────────────────────────────────────

function isImageUrl(icon: string): boolean {
  return icon.startsWith("/uploads/");
}

// ── Multi-checkbox section ────────────────────────────────────────────────

interface CheckboxSectionProps {
  title: string;
  hint?: string;
  hintLink?: string;
  loading: boolean;
  error: string | null;
  onRetry: () => void;
  emptyMessage: string;
  onRefresh?: () => void;
  items: { value: string; label: string; meta?: string }[];
  selected: string[];
  onChange: (selected: string[]) => void;
}

function CheckboxSection({
  title,
  hint,
  hintLink,
  loading,
  error,
  onRetry,
  emptyMessage,
  onRefresh,
  items,
  selected,
  onChange,
}: CheckboxSectionProps) {
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        <Label>{title}</Label>
        {onRefresh && (
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className="h-6 w-6"
            onClick={onRefresh}
          >
            <RefreshCw className="h-3 w-3" />
          </Button>
        )}
      </div>

      {loading && (
        <div className="space-y-1">
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-3/4" />
        </div>
      )}

      {!loading && error && (
        <div className="flex items-center gap-2 text-sm text-destructive py-1">
          <AlertCircle className="h-4 w-4" />
          <span>{error}</span>
          <Button type="button" variant="outline" size="sm" onClick={onRetry}>
            重试
          </Button>
        </div>
      )}

      {!loading && !error && items.length === 0 && (
        <div className="text-sm text-muted-foreground py-2">
          {emptyMessage}
          {hintLink && (
            <a
              href={hintLink}
              className="inline-flex items-center gap-1 ml-2 text-primary hover:underline"
            >
              <ExternalLink className="h-3 w-3" />
              前往管理
            </a>
          )}
        </div>
      )}

      {!loading && !error && items.length > 0 && (
        <div className="flex flex-wrap gap-2">
          {items.map((item) => {
            const checked = selected.includes(item.value);
            return (
              <label
                key={item.value}
                className="flex items-center gap-1.5 rounded-md border px-3 py-1.5 text-sm cursor-pointer hover:bg-accent transition-colors"
              >
                <input
                  type="checkbox"
                  checked={checked}
                  onChange={() => {
                    onChange(
                      checked
                        ? selected.filter((s) => s !== item.value)
                        : [...selected, item.value],
                    );
                  }}
                  className="h-3 w-3"
                />
                <span>{item.label}</span>
                {item.meta && (
                  <span className="text-xs text-muted-foreground">
                    {item.meta}
                  </span>
                )}
              </label>
            );
          })}
        </div>
      )}

      {hint && !loading && !error && (
        <p className="text-xs text-muted-foreground">{hint}</p>
      )}
    </div>
  );
}

// ── Props ─────────────────────────────────────────────────────────────────

interface Props {
  defaultValues?: AgentDetail;
  onSubmit: (data: CreateAgentRequest) => Promise<void>;
  onCancel?: () => void;
}

// ── Component ─────────────────────────────────────────────────────────────

export function AgentForm({ defaultValues, onSubmit, onCancel }: Props) {
  const fileRef = useRef<HTMLInputElement>(null);

  // ── Form ──────────────────────────────────────────────────────────────

  const {
    register,
    handleSubmit,
    setValue,
    watch,
    control,
    formState: { errors, isDirty },
  } = useForm({
    resolver: zodResolver(agentSchema),
    defaultValues: {
      name: defaultValues?.name ?? "",
      description: defaultValues?.description ?? "",
      system_prompt: defaultValues?.system_prompt ?? "",
      model: defaultValues?.model ?? "deepseek-v4-flash",
      provider: defaultValues?.provider ?? "deepseek",
      icon: defaultValues?.icon ?? "🤖",
      color: defaultValues?.color ?? "#6366f1",
      background_color: defaultValues?.background_color || randomPastel(),
      tools: defaultValues?.tools ?? [],
      skills: defaultValues?.skills ?? [],
      knowledge_bases: defaultValues?.knowledge_bases ?? [],
      mcp_servers: defaultValues?.mcp_servers ?? [],
      temperature: defaultValues?.temperature ?? undefined,
      max_tokens: defaultValues?.max_tokens ?? undefined,
      reasoning_effort: defaultValues?.reasoning_effort ?? "",
      max_turns: defaultValues?.max_turns ?? undefined,
    },
  });

  const icon = watch("icon");
  const color = watch("color");
  const bgColor = watch("background_color");
  const name = watch("name");

  // UseController for array fields (tools, skills, etc.)
  const { field: toolsField } = useController({ name: "tools", control });
  const { field: skillsField } = useController({ name: "skills", control });
  const { field: kbField } = useController({ name: "knowledge_bases", control });
  const { field: mcpField } = useController({ name: "mcp_servers", control });

  // ── Dynamic option loading ─────────────────────────────────────────────

  const [skillOptions, setSkillOptions] = useState<SkillListItem[]>([]);
  const [skillLoading, setSkillLoading] = useState(true);
  const [skillError, setSkillError] = useState<string | null>(null);

  const [kbOptions, setKbOptions] = useState<KnowledgeBase[]>([]);
  const [kbLoading, setKbLoading] = useState(true);
  const [kbError, setKbError] = useState<string | null>(null);

  const [mcpOptions, setMcpOptions] = useState<McpConfigResponse>({
    mcpServers: {},
  });
  const [mcpLoading, setMcpLoading] = useState(true);
  const [mcpError, setMcpError] = useState<string | null>(null);

  const loadSkills = () => {
    setSkillLoading(true);
    setSkillError(null);
    listSkills()
      .then(setSkillOptions)
      .catch(() => setSkillError("加载 Skill 失败"))
      .finally(() => setSkillLoading(false));
  };

  const loadKb = () => {
    setKbLoading(true);
    setKbError(null);
    listKnowledgeBases()
      .then(setKbOptions)
      .catch(() => setKbError("加载知识库失败"))
      .finally(() => setKbLoading(false));
  };

  const loadMcp = () => {
    setMcpLoading(true);
    setMcpError(null);
    getMcpConfig()
      .then(setMcpOptions)
      .catch(() => setMcpError("加载 MCP 配置失败"))
      .finally(() => setMcpLoading(false));
  };

  useEffect(() => {
    loadSkills();
    loadKb();
    loadMcp();
  }, []);

  // ── Unsaved changes warning ────────────────────────────────────────────

  useEffect(() => {
    const handler = (e: BeforeUnloadEvent) => {
      if (isDirty) {
        e.preventDefault();
        e.returnValue = "";
      }
    };
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  }, [isDirty]);

  // ── Submit ─────────────────────────────────────────────────────────────

  const handleFormSubmit = async (data: Record<string, unknown>) => {
    await onSubmit({
      name: data.name as string,
      description: (data.description as string) || undefined,
      system_prompt: (data.system_prompt as string) || undefined,
      model: (data.model as string) || undefined,
      provider: (data.provider as string) || undefined,
      icon: data.icon as string,
      color: data.color as string,
      background_color: data.background_color as string | undefined,
      tools: (data.tools as string[]) ?? [],
      skills: (data.skills as string[]) ?? [],
      knowledge_bases: (data.knowledge_bases as string[]) ?? [],
      mcp_servers: (data.mcp_servers as string[]) ?? [],
      temperature: data.temperature as number | undefined,
      max_tokens: data.max_tokens as number | undefined,
      reasoning_effort: (data.reasoning_effort as string) || undefined,
      max_turns: data.max_turns as number | undefined,
    });
  };

  // ── Icon upload ────────────────────────────────────────────────────────

  const handleIconUpload = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    try {
      const resized = await resizeImage(file);
      const formData = new FormData();
      formData.append("file", resized, "icon.png");
      const res = await api.post<{ url: string }>("/upload", formData, {
        headers: { "Content-Type": "multipart/form-data" },
      });
      setValue("icon", res.data.url);
      toast.success("图标上传成功");
    } catch {
      toast.error("图标上传失败");
    }
    if (fileRef.current) fileRef.current.value = "";
  };

  // ── MCP options as checkbox items ──────────────────────────────────────

  const mcpItems = Object.entries(mcpOptions.mcpServers).map(
    ([name, config]) => ({
      value: name,
      label: name,
      meta: config.transport,
    }),
  );

  // ── Render ─────────────────────────────────────────────────────────────

  return (
    <form onSubmit={handleSubmit(handleFormSubmit)} className="space-y-6">
      {/* Preview card */}
      <Card className="overflow-hidden">
        <CardContent className="p-3 h-[130px] flex gap-3">
          <div
            className="aspect-square h-full shrink-0 rounded-xl overflow-hidden flex items-center justify-center"
            style={{
              background: isImageUrl(icon || "")
                ? "transparent"
                : bgColor || color + "18" || "#6366f118",
            }}
          >
            {isImageUrl(icon || "") ? (
              <img src={icon} alt="" className="h-full w-full object-cover" />
            ) : (
              <span className="text-5xl select-none">{icon || "🤖"}</span>
            )}
          </div>
          <div className="flex-1 flex flex-col justify-center min-w-0 py-1">
            <p className="font-semibold text-lg truncate">
              {name || "Agent 名称"}
            </p>
            <p className="text-sm text-muted-foreground mt-1">卡片预览效果</p>
          </div>
        </CardContent>
      </Card>

      {/* Basic info */}
      <div className="grid gap-4 md:grid-cols-2">
        <div className="space-y-2">
          <Label htmlFor="name">名称 *</Label>
          <Input id="name" {...register("name")} />
          {errors.name && (
            <p className="text-sm text-destructive">{errors.name.message}</p>
          )}
        </div>
        <div className="space-y-2 md:col-span-2">
          <Label htmlFor="desc">描述</Label>
          <Input
            id="desc"
            {...register("description")}
            placeholder="简要描述 Agent 的用途"
          />
        </div>
      </div>

      {/* System prompt */}
      <div className="space-y-2">
        <Label htmlFor="prompt">System Prompt</Label>
        <Textarea
          id="prompt"
          rows={8}
          className="font-mono"
          {...register("system_prompt")}
        />
      </div>

      {/* Model + Provider */}
      <div className="grid gap-4 md:grid-cols-2">
        <div className="space-y-2">
          <Label>模型</Label>
          <Select
            defaultValue={defaultValues?.model ?? "deepseek-v4-flash"}
            onValueChange={(v) => setValue("model", v)}
          >
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {MODELS.map((m) => (
                <SelectItem key={m.value} value={m.value}>
                  {m.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="space-y-2">
          <Label>Provider</Label>
          <Select
            defaultValue={defaultValues?.provider ?? "deepseek"}
            onValueChange={(v) => setValue("provider", v)}
          >
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {PROVIDERS.map((p) => (
                <SelectItem key={p.value} value={p.value}>
                  {p.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>

      {/* Icon + Background color + Accent color */}
      <div className="grid gap-4 md:grid-cols-2">
        <div className="space-y-2">
          <Label>图标</Label>
          <div className="flex gap-2 items-center">
            <input
              ref={fileRef}
              type="file"
              accept="image/*"
              className="hidden"
              onChange={handleIconUpload}
            />
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => fileRef.current?.click()}
            >
              <Upload className="mr-1 h-4 w-4" />
              上传图片
            </Button>
            <span className="text-xs text-muted-foreground">或输入 emoji</span>
          </div>
          <Input {...register("icon")} placeholder="🤖 或图片 URL" />
        </div>
        <div className="space-y-2">
          <Label>背景色</Label>
          <div className="flex gap-2 items-center">
            <Input
              type="color"
              className="w-10 h-10 p-0.5"
              value={bgColor || "#f0f4ff"}
              onChange={(e) => setValue("background_color", e.target.value)}
            />
            <Input {...register("background_color")} placeholder="#f0f4ff" />
            <Button
              type="button"
              variant="outline"
              size="icon"
              className="shrink-0"
              title="随机生成柔和色"
              onClick={() => setValue("background_color", randomPastel())}
            >
              <Dice5 className="h-4 w-4" />
            </Button>
          </div>
        </div>
      </div>

      {/* Accent color (previously missing widget) */}
      <div className="space-y-2 max-w-xs">
        <Label>主题色</Label>
        <div className="flex gap-2 items-center">
          <Input
            type="color"
            className="w-10 h-10 p-0.5"
            value={color || "#6366f1"}
            onChange={(e) => setValue("color", e.target.value)}
          />
          <Input {...register("color")} placeholder="#6366f1" />
          <Button
            type="button"
            variant="outline"
            size="icon"
            className="shrink-0"
            title="随机生成柔和色"
            onClick={() => setValue("color", randomPastel())}
          >
            <Dice5 className="h-4 w-4" />
          </Button>
        </div>
        <p className="text-xs text-muted-foreground">
          用于卡片上的名称文字颜色
        </p>
      </div>

      {/* Tools (fixed: now managed via react-hook-form) */}
      <CheckboxSection
        title="工具选择"
        loading={false}
        error={null}
        onRetry={() => {}}
        emptyMessage=""
        items={TOOLS.map((t) => ({ value: t.value, label: t.label }))}
        selected={toolsField.value ?? []}
        onChange={toolsField.onChange}
      />

      {/* Skills */}
      <CheckboxSection
        title="Skill"
        hint="💡 前往「空间 > Skill」管理 Skill"
        loading={skillLoading}
        error={skillError}
        onRetry={loadSkills}
        emptyMessage="暂无可用 Skill，前往「空间 > Skill」创建"
        onRefresh={loadSkills}
        items={skillOptions.map((s) => ({
          value: s.name,
          label: s.name,
          meta: s.description,
        }))}
        selected={skillsField.value ?? []}
        onChange={skillsField.onChange}
      />

      {/* Knowledge bases */}
      <CheckboxSection
        title="知识库"
        hint="💡 前往「空间 > Knowledge」管理知识库"
        loading={kbLoading}
        error={kbError}
        onRetry={loadKb}
        emptyMessage="暂无可用知识库，前往「空间 > Knowledge」创建"
        onRefresh={loadKb}
        items={kbOptions.map((kb) => ({
          value: kb.name,
          label: kb.name,
          meta: kb.description,
        }))}
        selected={kbField.value ?? []}
        onChange={kbField.onChange}
      />

      {/* MCP servers */}
      <CheckboxSection
        title="MCP"
        hint="💡 前往「空间 > MCP」管理 MCP 服务器"
        loading={mcpLoading}
        error={mcpError}
        onRetry={loadMcp}
        emptyMessage="暂无可用 MCP 服务器，前往「空间 > MCP」添加"
        onRefresh={loadMcp}
        items={mcpItems}
        selected={mcpField.value ?? []}
        onChange={mcpField.onChange}
      />

      {/* Parameters */}
      <div className="grid gap-4 md:grid-cols-2">
        <div className="space-y-2">
          <Label>Temperature ({watch("temperature") ?? 0.7})</Label>
          <Slider
            min={0}
            max={2}
            step={0.1}
            defaultValue={[defaultValues?.temperature ?? 0.7]}
            onValueChange={([v]) => setValue("temperature", v)}
          />
        </div>
        <div className="space-y-2">
          <Label htmlFor="maxTokens">Max Tokens</Label>
          <Input
            id="maxTokens"
            type="number"
            {...register("max_tokens", {
              valueAsNumber: true,
              setValueAs: (v) => (v === "" ? undefined : Number(v)),
            })}
          />
          {errors.max_tokens && (
            <p className="text-sm text-destructive">
              {errors.max_tokens.message}
            </p>
          )}
        </div>
      </div>

      {/* Reasoning Effort + Max Turns */}
      <div className="grid gap-4 md:grid-cols-2">
        <div className="space-y-2">
          <Label>Reasoning Effort</Label>
          <Select
            value={watch("reasoning_effort") ?? ""}
            onValueChange={(v) => setValue("reasoning_effort", v)}
          >
            <SelectTrigger>
              <SelectValue placeholder="默认（使用 Provider 默认值）" />
            </SelectTrigger>
            <SelectContent>
              {REASONING_OPTIONS.map((opt) => (
                <SelectItem key={opt.value || "_default"} value={opt.value}>
                  {opt.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <p className="text-xs text-muted-foreground">
            控制模型推理深度。留空使用 provider 默认值。
          </p>
        </div>
        <div className="space-y-2">
          <Label htmlFor="maxTurns">Max Turns</Label>
          <Input
            id="maxTurns"
            type="number"
            min={1}
            placeholder="默认 30"
            {...register("max_turns", {
              valueAsNumber: true,
              setValueAs: (v) => (v === "" ? undefined : Number(v)),
            })}
          />
          <p className="text-xs text-muted-foreground">
            单次对话最多执行多少轮 ReAct 循环。
          </p>
          {errors.max_turns && (
            <p className="text-sm text-destructive">
              {errors.max_turns.message}
            </p>
          )}
        </div>
      </div>

      <div className="flex gap-3 pt-4 border-t">
        {onCancel && (
          <Button type="button" variant="outline" className="flex-1" onClick={onCancel}>
            取消
          </Button>
        )}
        <Button type="submit" className="flex-1">
          {defaultValues ? "保存修改" : "创建 Agent"}
        </Button>
      </div>
    </form>
  );
}
