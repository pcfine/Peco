import { useEffect, useRef, useState } from "react";
import { useForm, useController } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Slider } from "@/components/ui/slider";
import { Skeleton } from "@/components/ui/skeleton";
import { PageHeader } from "@/components/common/PageHeader";
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

// 背景色兜底基准色。留空时色块用其半透明变体（#6366f118），
// 颜色输入框（type="color" 只接受 6 位 hex）用纯色，二者同一色相。
const DEFAULT_BG_COLOR = "#6366f1";

/**
 * 把颜色归一成 `<input type="color">` 能接受的形式。
 *
 * 该控件只认 `#rrggbb`，而历史数据里存着 `hsl(...)`（`randomPastel` 的产物）。
 * 直接传 hsl 会触发浏览器校验错误，并把色板显示成黑色——看上去像颜色丢了。
 * 无法解析时回退到默认品牌色。
 */
function toHexColor(value: string | undefined): string {
  const raw = (value ?? "").trim();
  if (!raw) return DEFAULT_BG_COLOR;
  if (/^#[0-9a-f]{3}$/i.test(raw)) {
    return "#" + [...raw.slice(1)].map((c) => c + c).join("");
  }
  if (/^#[0-9a-f]{6}$/i.test(raw)) return raw;
  const m = raw.match(
    /^hsla?\(\s*([\d.]+)\s*[, ]\s*([\d.]+)%\s*[, ]\s*([\d.]+)%/i,
  );
  if (!m) return DEFAULT_BG_COLOR;
  const h = ((parseFloat(m[1]) % 360) + 360) % 360;
  const s = parseFloat(m[2]) / 100;
  const l = parseFloat(m[3]) / 100;
  const a = s * Math.min(l, 1 - l);
  const f = (n: number) => {
    const k = (n + h / 30) % 12;
    return Math.round(255 * (l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1))));
  };
  const hex = (n: number) => n.toString(16).padStart(2, "0");
  return `#${hex(f(0))}${hex(f(8))}${hex(f(4))}`;
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

// ── Inspector section wrapper ─────────────────────────────────────────────

function RailSection({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="space-y-3 px-5 py-4">
      <h2 className="text-sm font-medium">{title}</h2>
      {children}
    </section>
  );
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
  const hasItems = !loading && !error && items.length > 0;

  return (
    <div className="space-y-1.5">
      <div className="flex items-center gap-1.5">
        <span className="text-sm font-medium">{title}</span>
        {hasItems && (
          <span className="ml-auto text-xs text-muted-foreground tabular-nums">
            已选 {selected.length}/{items.length}
          </span>
        )}
        {onRefresh && (
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            className={hasItems ? "" : "ml-auto"}
            title={`重新加载${title}`}
            onClick={onRefresh}
          >
            <RefreshCw className="h-3 w-3" />
          </Button>
        )}
      </div>

      {loading && (
        <div className="flex gap-1.5">
          <Skeleton className="h-7 w-24" />
          <Skeleton className="h-7 w-32" />
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

      {hasItems && (
        <div className="flex flex-wrap gap-1.5">
          {items.map((item) => {
            const checked = selected.includes(item.value);
            return (
              <label
                key={item.value}
                data-chip
                title={item.meta || item.label}
                className="flex h-7 max-w-[180px] cursor-pointer items-center gap-1.5 rounded-md border px-2 text-xs transition-colors hover:bg-accent"
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
                  className="h-3 w-3 shrink-0"
                />
                <span className="truncate">{item.label}</span>
              </label>
            );
          })}
        </div>
      )}

      {hint && !loading && !error && (
        <p className="text-xs leading-4 text-muted-foreground">{hint}</p>
      )}
    </div>
  );
}

// ── Props ─────────────────────────────────────────────────────────────────

interface Props {
  title: string;
  defaultValues?: AgentDetail;
  onSubmit: (data: CreateAgentRequest) => Promise<void>;
  onCancel?: () => void;
}

// ── Component ─────────────────────────────────────────────────────────────

export function AgentForm({ title, defaultValues, onSubmit, onCancel }: Props) {
  const fileRef = useRef<HTMLInputElement>(null);

  // ── Form ──────────────────────────────────────────────────────────────

  const {
    register,
    handleSubmit,
    setValue,
    watch,
    control,
    formState: { errors, isDirty, isSubmitting },
  } = useForm({
    resolver: zodResolver(agentSchema),
    defaultValues: {
      name: defaultValues?.name ?? "",
      description: defaultValues?.description ?? "",
      system_prompt: defaultValues?.system_prompt ?? "",
      model: defaultValues?.model ?? "deepseek-v4-flash",
      provider: defaultValues?.provider ?? "deepseek",
      icon: defaultValues?.icon ?? "🤖",
      background_color: defaultValues?.background_color || randomPastel(),
      tools: defaultValues?.tools ?? [],
      skills: defaultValues?.skills ?? [],
      knowledge_bases: defaultValues?.knowledge_bases ?? [],
      mcp_servers: defaultValues?.mcp_servers ?? [],
      temperature: defaultValues?.temperature ?? undefined,
      max_tokens: defaultValues?.max_tokens ?? undefined,
      reasoning_effort: defaultValues?.reasoning_effort ?? "",
      max_iterations: defaultValues?.max_iterations ?? undefined,
    },
  });

  const icon = watch("icon");
  const bgColor = watch("background_color");
  const promptLength = watch("system_prompt")?.length ?? 0;

  // UseController for array fields (tools, skills, etc.)
  const { field: toolsField } = useController({ name: "tools", control });
  const { field: skillsField } = useController({ name: "skills", control });
  const { field: kbField } = useController({
    name: "knowledge_bases",
    control,
  });
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
      background_color: data.background_color as string | undefined,
      tools: (data.tools as string[]) ?? [],
      skills: (data.skills as string[]) ?? [],
      knowledge_bases: (data.knowledge_bases as string[]) ?? [],
      mcp_servers: (data.mcp_servers as string[]) ?? [],
      temperature: data.temperature as number | undefined,
      max_tokens: data.max_tokens as number | undefined,
      reasoning_effort: (data.reasoning_effort as string) || undefined,
      max_iterations: data.max_iterations as number | undefined,
    });
  };

  const submitLabel = defaultValues ? "保存修改" : "创建 Agent";

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (
        !(event.metaKey || event.ctrlKey) ||
        event.key.toLowerCase() !== "s"
      ) {
        return;
      }
      event.preventDefault();
      if (isSubmitting) return;
      void handleSubmit(handleFormSubmit)();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [handleSubmit, isSubmitting]);

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

  const mcpItems = Object.keys(mcpOptions.mcpServers).map((name) => ({
    value: name,
    label: name,
  }));

  const iconIsImage = isImageUrl(icon || "");

  // ── Render ─────────────────────────────────────────────────────────────

  return (
    <form
      onSubmit={handleSubmit(handleFormSubmit)}
      className="flex min-h-0 flex-1 flex-col"
    >
      <header className="sticky -top-4 z-20 bg-background pb-4 md:-top-6 xl:-top-8">
        <PageHeader
          title={title}
          actions={
            <div data-testid="action-bar" className="flex items-center gap-2">
              {isDirty && <span className="text-xs text-warning">●未保存</span>}
              {onCancel && (
                <Button type="button" variant="outline" onClick={onCancel}>
                  取消
                </Button>
              )}
              <Button
                type="submit"
                disabled={isSubmitting}
                title={`${submitLabel}（⌘S）`}
              >
                {isSubmitting ? "保存中…" : submitLabel}
              </Button>
            </div>
          }
        />
      </header>

      <div className="grid min-h-0 flex-1 grid-rows-none overflow-hidden rounded-xl border bg-card @min-[960px]:grid-rows-[minmax(0,1fr)] @min-[960px]:grid-cols-[minmax(0,1fr)_clamp(400px,38%,460px)]">
        {/* 创作面：系统提示词 */}
        <section
          data-testid="agent-canvas"
          className="flex h-[360px] min-w-0 flex-col @min-[660px]:h-[420px] @min-[960px]:h-auto"
        >
          <div className="flex h-11 shrink-0 items-center justify-between gap-3 border-b px-5">
            <h2 className="text-sm font-medium">系统提示词</h2>
            <span className="text-xs text-muted-foreground tabular-nums">
              {promptLength} 字符
            </span>
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4 [scrollbar-gutter:stable]">
            <div className="mx-auto h-full w-full max-w-[700px]">
              <textarea
                {...register("system_prompt")}
                aria-label="系统提示词"
                spellCheck={false}
                className="h-full w-full resize-none rounded-none border-0 bg-transparent px-0 py-0 font-mono text-sm leading-5 outline-none field-sizing-fixed focus-visible:ring-0"
              />
            </div>
          </div>
        </section>

        {/* 检查器 */}
        <aside
          data-testid="agent-rail"
          className="min-w-0 divide-y bg-muted/30 [overscroll-behavior:contain] [scrollbar-gutter:stable] @min-[960px]:overflow-y-auto @min-[960px]:border-l"
        >
          {/* ① 身份 */}
          <RailSection title="身份">
            <div className="flex gap-3">
              <input
                ref={fileRef}
                type="file"
                accept="image/*"
                className="hidden"
                onChange={handleIconUpload}
              />
              <button
                type="button"
                title="点击上传图标"
                onClick={() => fileRef.current?.click()}
                className="flex size-16 shrink-0 cursor-pointer items-center justify-center overflow-hidden rounded-xl"
                style={{
                  background: iconIsImage
                    ? "transparent"
                    : bgColor || `${DEFAULT_BG_COLOR}18`,
                }}
              >
                {iconIsImage ? (
                  <img
                    src={icon}
                    alt=""
                    className="h-full w-full object-cover"
                  />
                ) : (
                  <span className="text-3xl select-none">{icon || "🤖"}</span>
                )}
              </button>

              <div className="min-w-0 flex-1 space-y-2">
                <div className="space-y-1">
                  <Label htmlFor="name" className="text-xs">
                    名称 *
                  </Label>
                  <Input id="name" className="h-9" {...register("name")} />
                  {errors.name && (
                    <p className="text-sm text-destructive">
                      {errors.name.message}
                    </p>
                  )}
                </div>
                <div className="space-y-1">
                  <Label htmlFor="desc" className="text-xs">
                    描述
                  </Label>
                  <Input
                    id="desc"
                    className="h-9"
                    placeholder="简要描述 Agent 的用途"
                    {...register("description")}
                  />
                </div>
              </div>
            </div>

            <div className="mt-3 space-y-3">
              <div className="flex items-center gap-2">
                <Label htmlFor="icon" className="w-8 shrink-0 text-xs">
                  图标
                </Label>
                <Input
                  id="icon"
                  className="h-9 flex-1"
                  placeholder="🤖 或图片 URL"
                  {...register("icon")}
                />
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => fileRef.current?.click()}
                >
                  <Upload className="h-3.5 w-3.5" />
                  上传
                </Button>
              </div>

              <div className="flex items-center gap-2">
                <Label htmlFor="bgColor" className="w-8 shrink-0 text-xs">
                  背景
                </Label>
                <Input
                  id="bgColor"
                  type="color"
                  className="h-9 w-9 shrink-0 p-0.5"
                  value={toHexColor(bgColor)}
                  onChange={(e) => setValue("background_color", e.target.value)}
                />
                <Input
                  className="h-9 flex-1 font-mono"
                  placeholder={DEFAULT_BG_COLOR}
                  {...register("background_color")}
                />
                <Button
                  type="button"
                  variant="outline"
                  size="icon-sm"
                  className="shrink-0"
                  title="随机生成柔和色"
                  onClick={() => setValue("background_color", randomPastel())}
                >
                  <Dice5 className="h-4 w-4" />
                </Button>
              </div>
            </div>
          </RailSection>

          {/* ② 模型与参数 */}
          <RailSection title="模型与参数">
            <div className="grid grid-cols-1 gap-x-3 gap-y-3 @min-[660px]:grid-cols-2">
              <div className="space-y-1">
                <Label className="text-xs">模型</Label>
                <Select
                  defaultValue={defaultValues?.model ?? "deepseek-v4-flash"}
                  onValueChange={(v) => setValue("model", v)}
                >
                  <SelectTrigger className="h-9 w-full">
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

              <div className="space-y-1">
                <Label className="text-xs">Provider</Label>
                <Select
                  defaultValue={defaultValues?.provider ?? "deepseek"}
                  onValueChange={(v) => setValue("provider", v)}
                >
                  <SelectTrigger className="h-9 w-full">
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

              <div className="space-y-1">
                <Label className="text-xs">
                  Temperature ({watch("temperature") ?? 0.7})
                </Label>
                <Slider
                  min={0}
                  max={2}
                  step={0.1}
                  defaultValue={[defaultValues?.temperature ?? 0.7]}
                  onValueChange={([v]) => setValue("temperature", v)}
                />
                <p className="text-xs leading-4 text-muted-foreground">
                  留空使用 Provider 默认温度。
                </p>
              </div>

              <div className="space-y-1">
                <Label className="text-xs">Reasoning Effort</Label>
                <Select
                  value={watch("reasoning_effort") ?? ""}
                  onValueChange={(v) => setValue("reasoning_effort", v)}
                >
                  <SelectTrigger className="h-9 w-full">
                    <SelectValue placeholder="默认（使用 Provider 默认值）" />
                  </SelectTrigger>
                  <SelectContent>
                    {REASONING_OPTIONS.map((opt) => (
                      <SelectItem
                        key={opt.value || "_default"}
                        value={opt.value}
                      >
                        {opt.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-xs leading-4 text-muted-foreground">
                  推理深度，留空用 Provider 默认值。
                </p>
              </div>

              <div className="space-y-1">
                <Label htmlFor="maxTokens" className="text-xs">
                  Max Tokens
                </Label>
                <Input
                  id="maxTokens"
                  type="number"
                  className="h-9"
                  placeholder="留空不限制"
                  {...register("max_tokens", {
                    setValueAs: (v) => (v === "" ? undefined : Number(v)),
                  })}
                />
                <p className="text-xs leading-4 text-muted-foreground">
                  单次响应的最大输出 token 数。
                </p>
                {errors.max_tokens && (
                  <p className="text-sm text-destructive">
                    {errors.max_tokens.message}
                  </p>
                )}
              </div>

              <div className="space-y-1">
                <Label htmlFor="maxIterations" className="text-xs">
                  Max Iterations
                </Label>
                <Input
                  id="maxIterations"
                  type="number"
                  min={1}
                  className="h-9"
                  placeholder="默认 500"
                  {...register("max_iterations", {
                    setValueAs: (v) => (v === "" ? undefined : Number(v)),
                  })}
                />
                <p className="text-xs leading-4 text-muted-foreground">
                  单次对话最多执行的 ReAct 轮数。
                </p>
                {errors.max_iterations && (
                  <p className="text-sm text-destructive">
                    {errors.max_iterations.message}
                  </p>
                )}
              </div>
            </div>
          </RailSection>

          {/* ③ 能力 */}
          <div data-section="capabilities" className="space-y-2 px-5 py-3">
            <h2 className="text-sm font-medium">能力</h2>
            <CheckboxSection
              title="工具"
              hint="web_search 需先在 providers.toml 配置 [web_search]，否则不生效。"
              loading={false}
              error={null}
              onRetry={() => {}}
              emptyMessage=""
              items={TOOLS.map((t) => ({ value: t.value, label: t.label }))}
              selected={toolsField.value ?? []}
              onChange={toolsField.onChange}
            />
            <CheckboxSection
              title="Skill"
              hint="前往「空间 > Skill」管理 Skill。"
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
            <CheckboxSection
              title="知识库"
              hint="前往「空间 > Knowledge」管理知识库。"
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
            <CheckboxSection
              title="MCP"
              hint="前往「空间 > MCP」管理 MCP 服务器。"
              loading={mcpLoading}
              error={mcpError}
              onRetry={loadMcp}
              emptyMessage="暂无可用 MCP 服务器，前往「空间 > MCP」添加"
              onRefresh={loadMcp}
              items={mcpItems}
              selected={mcpField.value ?? []}
              onChange={mcpField.onChange}
            />
          </div>
        </aside>
      </div>
    </form>
  );
}
