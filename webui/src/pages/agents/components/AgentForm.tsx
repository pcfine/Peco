import { useEffect, useRef, useState } from "react";
import { useForm, useController } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Slider } from "@/components/ui/slider";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { PageHeader } from "@/components/common/PageHeader";
import { PixelIdenticon } from "@/components/common/PixelIdenticon";
import { cn } from "@/lib/utils";
import { agentSchema } from "@/lib/validators";
import {
  DEFAULT_MODEL,
  MODELS,
  PROVIDERS,
  TOOLS,
  normalizeModel,
} from "@/lib/constants";
import {
  pixelIconValue,
  pixelSeed,
  resolveAgentIcon,
} from "@/lib/identicon";
import { listSkills } from "@/api/skills";
import { listKnowledgeBases } from "@/api/knowledge";
import { getMcpConfig } from "@/api/mcp";
import api from "@/api/client";
import type { AgentDetail, CreateAgentRequest } from "@/types/agent";
import type { SkillListItem } from "@/types/skill";
import type { KnowledgeBase } from "@/types/knowledge";
import type { McpConfigResponse } from "@/types/mcp";
import {
  Camera,
  Dice5,
  RefreshCw,
  AlertCircle,
  Plus,
  Search,
  X,
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

// ── Config section wrapper ────────────────────────────────────────────────

function ConfigSection({
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

// ── Field row ─────────────────────────────────────────────────────────────

function FieldRow({
  label,
  htmlFor,
  children,
}: {
  label: string;
  htmlFor?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex min-h-9 items-center gap-2">
      <Label
        htmlFor={htmlFor}
        className="w-[104px] shrink-0 text-right text-xs whitespace-nowrap"
      >
        {label}
      </Label>
      <div className="flex max-w-[220px] min-w-0 flex-1 items-center gap-2 @max-[769px]:max-w-[300px]">
        {children}
      </div>
    </div>
  );
}

// ── Capability picker ─────────────────────────────────────────────────────

interface PickerOption {
  value: string;
  label: string;
  meta?: string;
}

interface CapabilityPickerProps {
  title: string;
  hint?: string;
  options: PickerOption[];
  selected: string[];
  onChange: (next: string[]) => void;
  loading?: boolean;
  error?: string | null;
  onRetry?: () => void;
  onRefresh?: () => void;
  /** 候选集为空时的说明，仅在弹窗里出现。 */
  emptyMessage?: string;
}

/**
 * 只展示已选项，全量清单收进弹窗。
 *
 * 勾选是高频动作，判断依据只有名字——把每个候选项连同描述一起铺在表单里，
 * 会让 4 个能力区把左栏撑成两三屏，真正要复核的描述反而没人看。
 */
function CapabilityPicker({
  title,
  hint,
  options,
  selected,
  onChange,
  loading = false,
  error = null,
  onRetry,
  onRefresh,
  emptyMessage,
}: CapabilityPickerProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [staged, setStaged] = useState<string[]>([]);

  const byValue = new Map(options.map((o) => [o.value, o]));
  const keyword = query.trim().toLowerCase();
  const filtered = keyword
    ? options.filter(
        (o) =>
          o.label.toLowerCase().includes(keyword) ||
          o.value.toLowerCase().includes(keyword) ||
          (o.meta ?? "").toLowerCase().includes(keyword),
      )
    : options;

  const openPicker = () => {
    setStaged(selected);
    setQuery("");
    setOpen(true);
  };

  const toggle = (value: string) =>
    setStaged((prev) =>
      prev.includes(value)
        ? prev.filter((v) => v !== value)
        : [...prev, value],
    );

  const disabled = loading || Boolean(error);

  return (
    <div data-picker={title} className="space-y-2">
      <div className="flex items-center gap-2">
        <span className="text-sm font-medium">{title}</span>
        <span className="text-xs text-muted-foreground tabular-nums">
          已选 {selected.length}
        </span>
        <Button
          type="button"
          variant="outline"
          size="icon-xs"
          className="ml-auto"
          title={`添加 ${title}`}
          aria-label={`添加 ${title}`}
          disabled={disabled}
          onClick={openPicker}
        >
          <Plus className="h-3.5 w-3.5" />
        </Button>
      </div>

      {loading && (
        <div className="flex gap-1.5">
          <Skeleton className="h-7 w-24" />
          <Skeleton className="h-7 w-32" />
        </div>
      )}

      {!loading && error && (
        <div className="flex items-center gap-2 py-1 text-sm text-destructive">
          <AlertCircle className="h-4 w-4" />
          <span>{error}</span>
          {onRetry && (
            <Button type="button" variant="outline" size="sm" onClick={onRetry}>
              重试
            </Button>
          )}
        </div>
      )}

      {!loading && !error && selected.length === 0 && (
        <p className="text-xs leading-4 text-muted-foreground">未添加</p>
      )}

      {!loading && !error && selected.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {selected.map((value) => {
            const option = byValue.get(value);
            const label = option?.label ?? value;
            return (
              <span
                key={value}
                data-chip
                className="inline-flex h-7 max-w-[220px] items-center gap-1 rounded-md border bg-background pr-1 pl-2 text-xs"
              >
                <span
                  className="truncate"
                  title={option?.meta || label}
                >
                  {label}
                </span>
                <button
                  type="button"
                  title={`移除 ${label}`}
                  aria-label={`移除 ${label}`}
                  className="shrink-0 rounded-sm p-0.5 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                  onClick={() =>
                    onChange(selected.filter((v) => v !== value))
                  }
                >
                  <X className="h-3 w-3" />
                </button>
              </span>
            );
          })}
        </div>
      )}

      {hint && !loading && !error && (
        <p className="text-xs leading-4 text-muted-foreground">{hint}</p>
      )}

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="gap-0 p-0 sm:max-w-lg">
          <DialogHeader className="border-b px-5 py-4">
            <DialogTitle className="text-base">添加 {title}</DialogTitle>
            <DialogDescription>
              勾选后点击确定，写入当前 Agent。
            </DialogDescription>
          </DialogHeader>

          <div className="flex items-center gap-2 border-b px-5 py-3">
            <div className="relative flex-1">
              <Search className="pointer-events-none absolute top-1/2 left-2.5 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                autoFocus
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={`搜索 ${title}`}
                aria-label={`搜索 ${title}`}
                className="h-9 pl-8"
              />
            </div>
            {onRefresh && (
              <Button
                type="button"
                variant="outline"
                size="icon-sm"
                title={`重新加载 ${title}`}
                aria-label={`重新加载 ${title}`}
                onClick={onRefresh}
              >
                <RefreshCw className="h-3.5 w-3.5" />
              </Button>
            )}
          </div>

          <div className="max-h-[min(46vh,360px)] overflow-y-auto p-2">
            {filtered.length === 0 ? (
              <p className="px-3 py-6 text-center text-sm text-muted-foreground">
                {options.length === 0 && emptyMessage
                  ? emptyMessage
                  : `没有匹配的 ${title}`}
              </p>
            ) : (
              filtered.map((option) => {
                const checked = staged.includes(option.value);
                return (
                  <label
                    key={option.value}
                    data-option={option.value}
                    className="flex cursor-pointer items-start gap-2.5 rounded-md px-3 py-2 transition-colors hover:bg-accent"
                  >
                    <input
                      type="checkbox"
                      checked={checked}
                      onChange={() => toggle(option.value)}
                      className="mt-0.5 h-3.5 w-3.5 shrink-0"
                    />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-sm">
                        {option.label}
                      </span>
                      {option.meta && (
                        <span className="mt-0.5 block truncate text-xs text-muted-foreground">
                          {option.meta}
                        </span>
                      )}
                    </span>
                  </label>
                );
              })
            )}
          </div>

          <DialogFooter className="flex-row items-center justify-between border-t px-5 py-3">
            <span className="text-xs text-muted-foreground tabular-nums">
              已选 {staged.length}
            </span>
            <div className="flex gap-2">
              <Button
                type="button"
                variant="outline"
                onClick={() => setOpen(false)}
              >
                取消
              </Button>
              <Button
                type="button"
                onClick={() => {
                  onChange(staged);
                  setOpen(false);
                }}
              >
                确定
              </Button>
            </div>
          </DialogFooter>
        </DialogContent>
      </Dialog>
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
      model: normalizeModel(defaultValues?.model),
      provider: defaultValues?.provider ?? "deepseek",
      icon: defaultValues?.icon ?? pixelIconValue(pixelSeed()),
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
  const name = watch("name");
  const bgColor = watch("background_color");
  const promptLength = watch("system_prompt")?.length ?? 0;

  // 模型下拉的当前值与候选：库里存的旧别名已归一，未知自定义值补一项，
  // 避免下拉显示空白、保存时静默丢失。
  const modelValue = watch("model") || DEFAULT_MODEL;
  const modelOptions = MODELS.some((m) => m.value === modelValue)
    ? MODELS
    : [...MODELS, { value: modelValue, label: `${modelValue}（自定义）` }];

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
      setValue("icon", res.data.url, { shouldDirty: true });
      toast.success("图标上传成功");
    } catch {
      toast.error("图标上传失败");
    }
    if (fileRef.current) fileRef.current.value = "";
  };

  // ── MCP options ────────────────────────────────────────────────────────

  const mcpItems = Object.keys(mcpOptions.mcpServers).map((name) => ({
    value: name,
    label: name,
  }));

  // 未设置图标时按名称生成，和列表页、对话页保持同一张图。
  const resolvedIcon = resolveAgentIcon(icon, name);

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

      <div className="grid min-h-0 flex-1 grid-cols-1 gap-4 @min-[960px]:grid-cols-2 @max-[959px]:flex-none">
        {/* 左：属性配置 */}
        <section
          data-testid="agent-config"
          className="@container flex min-h-0 flex-col overflow-hidden rounded-xl border bg-card"
        >
          <div className="flex h-11 shrink-0 items-center justify-between gap-3 border-b px-5">
            <h2 className="text-sm font-medium">Agent 配置</h2>
            <span className="text-xs text-muted-foreground">
              身份 · 模型 · 能力
            </span>
          </div>

          <div className="min-h-0 flex-1 divide-y overflow-y-auto [scrollbar-gutter:stable]">
            {/* ① 身份 */}
            <ConfigSection title="身份">
              <div className="flex items-stretch gap-4">
                <input
                  ref={fileRef}
                  type="file"
                  accept="image/*"
                  className="hidden"
                  onChange={handleIconUpload}
                />
                {/* 头像与右侧字段区同高：36 + 8 + 58 = 102，两者上下边缘自然对齐。 */}
                <div className="relative size-[102px] shrink-0">
                  <button
                    type="button"
                    data-testid="icon-upload"
                    title="点击上传图片"
                    aria-label="上传图标"
                    onClick={() => fileRef.current?.click()}
                    className="group relative flex h-full w-full items-center justify-center overflow-hidden rounded-xl focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background focus-visible:outline-none"
                    style={{
                      background:
                        resolvedIcon.kind === "emoji"
                          ? bgColor || undefined
                          : undefined,
                    }}
                  >
                    {resolvedIcon.kind === "image" && (
                      <img
                        src={resolvedIcon.value}
                        alt=""
                        className="h-full w-full object-cover"
                      />
                    )}
                    {resolvedIcon.kind === "pixel" && (
                      <PixelIdenticon
                        identicon={resolvedIcon.identicon!}
                        className="h-full w-full"
                      />
                    )}
                    {resolvedIcon.kind === "emoji" && (
                      <span className="text-4xl select-none">
                        {resolvedIcon.value}
                      </span>
                    )}
                    <span className="absolute inset-0 flex items-center justify-center bg-black/50 opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100">
                      <Camera className="h-6 w-6 text-white" />
                    </span>
                  </button>
                  <button
                    type="button"
                    data-testid="icon-shuffle"
                    title="换一个随机像素图标"
                    aria-label="换一个随机像素图标"
                    onClick={() =>
                      setValue("icon", pixelIconValue(pixelSeed()), {
                        shouldDirty: true,
                      })
                    }
                    className="absolute -right-1.5 -bottom-1.5 flex size-7 items-center justify-center rounded-full border bg-background shadow-sm transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
                  >
                    <Dice5 className="h-4 w-4" />
                  </button>
                </div>

                {/* 两行均为「标签 + 控件」同行；标签同宽左对齐，控件左边界一致。 */}
                <div className="flex min-w-0 flex-1 flex-col gap-2">
                  <div className="flex items-center gap-2">
                    <Label
                      htmlFor="name"
                      className="w-10 shrink-0 text-xs leading-4"
                    >
                      名称 *
                    </Label>
                    <Input
                      id="name"
                      className="h-9"
                      aria-invalid={Boolean(errors.name)}
                      {...register("name")}
                    />
                  </div>
                  {errors.name && (
                    <p className="pl-12 text-xs text-destructive">
                      {errors.name.message}
                    </p>
                  )}
                  <div className="flex items-start gap-2">
                    <Label
                      htmlFor="desc"
                      className="w-10 shrink-0 pt-2.5 text-xs leading-4"
                    >
                      描述
                    </Label>
                    <Textarea
                      id="desc"
                      rows={2}
                      placeholder="简要描述 Agent 的用途"
                      className="field-sizing-fixed h-[58px] min-h-0 resize-none text-sm leading-5"
                      {...register("description")}
                    />
                  </div>
                </div>
              </div>
            </ConfigSection>

            {/* ② 模型与参数 */}
            <ConfigSection title="模型与参数">
              <div className="grid grid-cols-1 gap-x-8 gap-y-2 @min-[770px]:grid-cols-2">
                <FieldRow label="Provider">
                  <Select
                    defaultValue={defaultValues?.provider ?? "deepseek"}
                    onValueChange={(v) =>
                      setValue("provider", v, { shouldDirty: true })
                    }
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
                </FieldRow>

                <FieldRow label="Model">
                  <Select
                    value={modelValue}
                    onValueChange={(v) =>
                      setValue("model", v, { shouldDirty: true })
                    }
                  >
                    <SelectTrigger className="h-9 w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {modelOptions.map((m) => (
                        <SelectItem key={m.value} value={m.value}>
                          {m.label}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </FieldRow>

                <FieldRow label="Reasoning Effort">
                  <Select
                    value={watch("reasoning_effort") ?? ""}
                    onValueChange={(v) =>
                      setValue("reasoning_effort", v, { shouldDirty: true })
                    }
                  >
                    <SelectTrigger className="h-9 w-full">
                      <SelectValue placeholder="默认" />
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
                </FieldRow>

                <FieldRow label="Temperature">
                  <Slider
                    min={0}
                    max={2}
                    step={0.1}
                    defaultValue={[defaultValues?.temperature ?? 0.7]}
                    onValueChange={([v]) =>
                      setValue("temperature", v, { shouldDirty: true })
                    }
                  />
                  <span className="w-7 shrink-0 text-right text-xs tabular-nums text-muted-foreground">
                    {watch("temperature") ?? 0.7}
                  </span>
                </FieldRow>

                <div className="space-y-1">
                  <FieldRow label="Max Tokens" htmlFor="maxTokens">
                    <Input
                      id="maxTokens"
                      type="number"
                      className="h-9 w-[130px]"
                      placeholder="留空不限制"
                      {...register("max_tokens", {
                        setValueAs: (v) => (v === "" ? undefined : Number(v)),
                      })}
                    />
                  </FieldRow>
                  {errors.max_tokens && (
                    <p className="pl-[112px] text-xs text-destructive">
                      {errors.max_tokens.message}
                    </p>
                  )}
                </div>

                <div className="space-y-1">
                  <FieldRow label="Max Iterations" htmlFor="maxIterations">
                    <Input
                      id="maxIterations"
                      type="number"
                      min={1}
                      className="h-9 w-[130px]"
                      placeholder="默认 500"
                      {...register("max_iterations", {
                        setValueAs: (v) => (v === "" ? undefined : Number(v)),
                      })}
                    />
                  </FieldRow>
                  {errors.max_iterations && (
                    <p className="pl-[112px] text-xs text-destructive">
                      {errors.max_iterations.message}
                    </p>
                  )}
                </div>
              </div>
            </ConfigSection>

            {/* ③ 能力 */}
            <section data-section="capabilities" className="space-y-3 px-5 py-4">
              <h2 className="text-sm font-medium">能力</h2>
              {/* 四组之间空一行，避免「已选」chip 粘成一片。 */}
              <div className="space-y-6">
                <CapabilityPicker
                  title="Tools"
                  hint="web_search 需先在 providers.toml 配置 [web_search]，否则不生效。"
                  options={TOOLS.map((t) => ({ value: t.value, label: t.label }))}
                  selected={toolsField.value ?? []}
                  onChange={toolsField.onChange}
                />
                <CapabilityPicker
                  title="Skill"
                  emptyMessage="暂无可用 Skill，请先在「空间 > Skill」创建。"
                  options={skillOptions.map((s) => ({
                    value: s.name,
                    label: s.name,
                    meta: s.description,
                  }))}
                  selected={skillsField.value ?? []}
                  onChange={skillsField.onChange}
                  loading={skillLoading}
                  error={skillError}
                  onRetry={loadSkills}
                  onRefresh={loadSkills}
                />
                <CapabilityPicker
                  title="KnowledgeBase"
                  emptyMessage="暂无可用知识库，请先在「空间 > Knowledge」创建。"
                  options={kbOptions.map((kb) => ({
                    value: kb.name,
                    label: kb.name,
                    meta: kb.description,
                  }))}
                  selected={kbField.value ?? []}
                  onChange={kbField.onChange}
                  loading={kbLoading}
                  error={kbError}
                  onRetry={loadKb}
                  onRefresh={loadKb}
                />
                <CapabilityPicker
                  title="MCP"
                  emptyMessage="暂无可用 MCP 服务器，请先在「空间 > MCP」添加。"
                  options={mcpItems}
                  selected={mcpField.value ?? []}
                  onChange={mcpField.onChange}
                  loading={mcpLoading}
                  error={mcpError}
                  onRetry={loadMcp}
                  onRefresh={loadMcp}
                />
              </div>
            </section>
          </div>
        </section>

        {/* 右：提示词 */}
        <section
          data-testid="agent-prompt"
          className="flex min-h-0 flex-col overflow-hidden rounded-xl border bg-card @max-[959px]:h-[440px]"
        >
          <div className="flex h-11 shrink-0 items-center justify-between gap-3 border-b px-5">
            <h2 className="text-sm font-medium">提示词</h2>
            <span className="text-xs text-muted-foreground tabular-nums">
              {promptLength} 字符
            </span>
          </div>
          <textarea
            {...register("system_prompt")}
            aria-label="提示词"
            spellCheck={false}
            className="field-sizing-fixed min-h-0 w-full flex-1 resize-none rounded-none border-0 bg-transparent px-5 py-4 font-mono text-sm leading-5 outline-none [scrollbar-gutter:stable] focus-visible:ring-2 focus-visible:ring-ring/60 focus-visible:ring-inset"
          />
        </section>
      </div>
    </form>
  );
}
