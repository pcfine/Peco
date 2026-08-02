import { useRef } from "react";
import { useForm } from "react-hook-form";
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
import { agentSchema } from "@/lib/validators";
import { MODELS, PROVIDERS, TOOLS } from "@/lib/constants";
import api from "@/api/client";
import type { AgentDetail, CreateAgentRequest } from "@/types/agent";
import { Upload, Dice5 } from "lucide-react";
import { toast } from "sonner";

// ── 随机柔和色生成 ──────────────────────────────────────────────────────────
// 约束：不能白色/黑色/过亮/过暗；饱和度不能太高；深色文字在背景上可读
// L 55-70%（不太暗、不太白）S 30-50%（柔和不刺眼、不灰）
function randomPastel(): string {
  const h = Math.floor(Math.random() * 360);
  const s = 30 + Math.floor(Math.random() * 20); // 30–50%
  const l = 55 + Math.floor(Math.random() * 15); // 55–70%
  return `hsl(${h}, ${s}%, ${l}%)`;
}

// ── 客户端图片裁剪（居中正方形，缩放至 256×256）──────────────────────────
async function resizeImage(file: File, maxSize = 256): Promise<Blob> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => {
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
    img.onerror = () => reject(new Error("Image load failed"));
    img.src = URL.createObjectURL(file);
  });
}

// ── 判断 icon 是否为图片 URL ──────────────────────────────────────────────
function isImageUrl(icon: string): boolean {
  return icon.startsWith("/uploads/");
}

interface Props {
  defaultValues?: AgentDetail;
  onSubmit: (data: CreateAgentRequest) => Promise<void>;
}

export function AgentForm({ defaultValues, onSubmit }: Props) {
  const fileRef = useRef<HTMLInputElement>(null);

  const {
    register,
    handleSubmit,
    setValue,
    watch,
    formState: { errors },
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
      temperature: defaultValues?.temperature ?? undefined,
      max_tokens: defaultValues?.max_tokens ?? undefined,
    },
  });

  const icon = watch("icon");
  const color = watch("color");
  const bgColor = watch("background_color");
  const name = watch("name");

  const handleFormSubmit = async (data: Record<string, unknown>) => {
    await onSubmit({
      name: data.name as string,
      description: data.description as string,
      system_prompt: data.system_prompt as string,
      model: data.model as string,
      provider: data.provider as string,
      icon: data.icon as string,
      color: data.color as string,
      background_color: data.background_color as string | undefined,
      temperature: data.temperature as number | undefined,
      max_tokens: data.max_tokens as number | undefined,
    });
  };

  // ── 图标上传 ────────────────────────────────────────────────────────────
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
    // 重置 input，允许重复选择同一文件
    if (fileRef.current) fileRef.current.value = "";
  };

  return (
    <form onSubmit={handleSubmit(handleFormSubmit)} className="space-y-6">
      {/* Preview card — 左方块 + 右信息 */}
      <Card className="overflow-hidden">
        <CardContent className="p-3 h-[130px] flex gap-3">
          {/* 左侧：正方形色块/图片 */}
          <div
            className="w-[100px] shrink-0 rounded-xl overflow-hidden flex items-center justify-center"
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
          {/* 右侧：信息 */}
          <div className="flex-1 flex flex-col justify-center min-w-0 py-1">
            <p className="font-semibold text-lg truncate">
              {name || "Agent 名称"}
            </p>
            <p className="text-sm text-muted-foreground mt-1">卡片预览效果</p>
          </div>
        </CardContent>
      </Card>

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

      <div className="space-y-2">
        <Label htmlFor="prompt">System Prompt</Label>
        <Textarea
          id="prompt"
          rows={8}
          className="font-mono"
          {...register("system_prompt")}
        />
      </div>

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

      {/* ── 图标上传 + 背景色 ───────────────────────────────────────────── */}
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

      <div className="space-y-2">
        <Label>工具选择</Label>
        <div className="flex flex-wrap gap-2">
          {TOOLS.map((t) => (
            <label
              key={t.value}
              className="flex items-center gap-1 rounded-md border px-3 py-1 text-sm cursor-pointer hover:bg-accent"
            >
              <input
                type="checkbox"
                defaultChecked={defaultValues?.tools?.includes(t.value)}
                className="h-3 w-3"
              />
              {t.label}
            </label>
          ))}
        </div>
      </div>

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

      <Button type="submit" className="w-full">
        {defaultValues ? "保存修改" : "创建 Agent"}
      </Button>
    </form>
  );
}
