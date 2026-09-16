// ProviderSection — Provider 配置管理区块
//
// 表单以「类型」为起点：类型一定，默认地址与默认模型就按目录（`GET /providers/types`）
// 自动填好，用户通常只需要粘一个 API Key。地址与模型仍可手改（自建兼容端点场景），
// 但不会出现让用户对着空输入框手打默认值的状态。

import { useEffect, useState } from "react";
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
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import {
  listProviders,
  listProviderTypes,
  upsertProvider,
  deleteProvider,
  testProviderConnection,
  testProviderDraft,
} from "@/api/providers";
import type {
  ProviderInfo,
  ProviderTypeInfo,
  TestConnectionResponse,
  UpsertProviderRequest,
  TestProviderRequest,
} from "@/api/providers";
import {
  Plus,
  Pencil,
  Trash2,
  Wifi,
  AlertCircle,
  CheckCircle2,
  ChevronDown,
  Loader2,
} from "lucide-react";
import { toast } from "sonner";
import { cn } from "@/lib/utils";

/** `api` 选择器里代表「跟随类型默认风格」的哨兵值。
 *
 * radix Select 不接受空字符串 value，而载荷里的空串含义正是"清空该字段
 * → 适配器用类型默认风格"，所以在选择器边界上做一次映射。 */
const API_DEFAULT = "__type_default__";

// ── 表单模型（纯函数，供测试直接调用） ─────────────────────────────────────

/** 配置表单状态 —— 与 provider 的可写字段一一对应。 */
export interface FormState {
  name: string;
  providerType: string;
  /** 留空 = 保存时不改动已存密钥（密钥不回读，无法回显旧值）。 */
  apiKey: string;
  baseUrl: string;
  /** `""` = 跟随类型默认风格。 */
  api: string;
  model: string;
  setDefault: boolean;
}

/** 按类型取目录条目。 */
export function typeMeta(
  types: ProviderTypeInfo[],
  providerType: string,
): ProviderTypeInfo | undefined {
  return types.find((t) => t.provider_type === providerType);
}

/** 在 `taken` 中找一个未占用的名字（`base`、`base-2`、`base-3`…）。 */
export function uniqueName(base: string, taken: string[]): string {
  if (!taken.includes(base)) return base;
  for (let i = 2; i <= taken.length + 2; i += 1) {
    const candidate = `${base}-${i}`;
    if (!taken.includes(candidate)) return candidate;
  }
  return base;
}

/** 新建时的初始表单：首个类型 + 它的默认地址与默认模型。 */
export function blankForm(
  types: ProviderTypeInfo[],
  taken: string[] = [],
): FormState {
  const first = types[0];
  return {
    name: first ? uniqueName(first.provider_type, taken) : "",
    providerType: first?.provider_type ?? "",
    apiKey: "",
    baseUrl: first?.default_base_url ?? "",
    api: "",
    model: first?.default_model ?? "",
    setDefault: false,
  };
}

/**
 * 已存条目 → 表单。密钥不回读，`apiKey` 一律留空。
 *
 * 只回显**已存值**，不拿目录默认值补空：补空会让"只改密钥"这种编辑在保存时
 * 把目录默认地址/模型显式写进配置（原本为空表示"跟随类型默认"），
 * 等于用户没改的字段被静默改掉。空字段的语义由输入框下方的说明文字交代。
 */
export function formFromProvider(provider: ProviderInfo): FormState {
  return {
    name: provider.name,
    providerType: provider.provider_type,
    apiKey: "",
    baseUrl: provider.base_url ?? "",
    api: provider.api ?? "",
    model: provider.default_model ?? "",
    setDefault: provider.is_default,
  };
}

/** 字段为空、或还停留在上一个类型的默认值时，才跟随新类型的默认值。 */
function adoptDefault(
  current: string,
  prevDefault: string | undefined,
  nextDefault: string,
): string {
  const trimmed = current.trim();
  return trimmed === "" || trimmed === prevDefault ? nextDefault : current;
}

/**
 * 切换类型时的字段重填。
 *
 * 只覆盖"为空或仍是上一个类型默认值"的字段 —— 用户手改过的地址/模型
 * （自建兼容端点）不会被切类型冲掉。
 */
export function formForTypeChange(
  form: FormState,
  prev: ProviderTypeInfo | undefined,
  next: ProviderTypeInfo | undefined,
  opts: { isNew: boolean; taken: string[] },
): FormState {
  if (!next) return form;
  const nameFollowsType =
    form.name.trim() === "" || form.name === prev?.provider_type;
  return {
    ...form,
    providerType: next.provider_type,
    // 名字是 providers.toml 的 key（编辑时即主键），只在新建且仍是默认值时跟随类型
    name:
      opts.isNew && nameFollowsType
        ? uniqueName(next.provider_type, opts.taken)
        : form.name,
    baseUrl: adoptDefault(
      form.baseUrl,
      prev?.default_base_url,
      next.default_base_url,
    ),
    model: adoptDefault(form.model, prev?.default_model, next.default_model),
    // api 档位是类型特有的，新类型没有该档位就退回"跟随默认"
    api: next.api_modes.includes(form.api) ? form.api : "",
  };
}

/** 表单 → `PUT /providers` 请求体。 */
export function toUpsertRequest(form: FormState): UpsertProviderRequest {
  const req: UpsertProviderRequest = {
    name: form.name.trim() || form.providerType,
    type: form.providerType,
    // 空串 = 使用该类型的默认地址（后端把空串当"清空该字段"）
    base_url: form.baseUrl.trim(),
    api: form.api,
    default_model: form.model.trim(),
  };
  // 省略 api_key = 保留已存密钥；新建时省略等价于"没有密钥"，语义一致。
  // 注意不能传空串：后端把空串解释为"清空该字段"，编辑时留空会静默删掉已存密钥。
  if (form.apiKey.trim()) req.api_key = form.apiKey.trim();
  if (form.setDefault) req.set_default = true;
  return req;
}

/** 表单 → `POST /providers/test` 请求体。草稿没有已存值可回退，字段直传。 */
export function toTestRequest(form: FormState): TestProviderRequest {
  return {
    name: form.name.trim() || form.providerType,
    type: form.providerType,
    api_key: form.apiKey.trim(),
    base_url: form.baseUrl.trim(),
    api: form.api,
    default_model: form.model.trim(),
  };
}

/**
 * 保存前的本地校验，返回第一条不满足的说明；全部通过返回 null。
 *
 * 密钥与模型只在新建时必填：编辑时两者留空都表示"保持已存值"
 * （后端把空串解释为保留），而历史配置里本就可以没有默认模型 ——
 * 强制填会让"只想换个密钥"的编辑保存不出去。
 */
export function validateForm(form: FormState, isNew: boolean): string | null {
  if (!form.providerType.trim()) return "请选择 Provider 类型";
  if (!form.name.trim()) return "请填写名称";
  if (!isNew) return null;
  if (!form.apiKey.trim()) {
    return "请填写 API Key（也可填 ${ENV_VAR} 由服务端从环境变量读取）";
  }
  if (!form.model.trim()) return "请填写默认模型";
  return null;
}

/** 后端 `ApiError` 序列化为 `{ error, details }`，优先展示 details。 */
function apiErrorMessage(err: unknown, fallback: string): string {
  const details = (err as { response?: { data?: { details?: string } } })
    ?.response?.data?.details;
  return details ? String(details) : fallback;
}

// ── Component ─────────────────────────────────────────────────────────────

export function ProviderSection() {
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [types, setTypes] = useState<ProviderTypeInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Dialog state
  const [dialogOpen, setDialogOpen] = useState(false);
  const [editing, setEditing] = useState<ProviderInfo | null>(null);
  const [form, setForm] = useState<FormState>(() => blankForm([]));
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [saving, setSaving] = useState(false);

  // Draft connection test (inside the dialog)
  const [draftTesting, setDraftTesting] = useState<string | null>(null);
  const [draftTest, setDraftTest] = useState<{
    signature: string;
    result: TestConnectionResponse;
  } | null>(null);

  // Delete confirm
  const [deleteTarget, setDeleteTarget] = useState<ProviderInfo | null>(null);
  const [deleting, setDeleting] = useState(false);

  // Testing a saved provider
  const [testing, setTesting] = useState<string | null>(null);

  // ── Load ─────────────────────────────────────────────────────────────

  const load = () => {
    setLoading(true);
    setError(null);
    Promise.all([listProviders(), listProviderTypes()])
      .then(([providerList, typeList]) => {
        setProviders(providerList);
        setTypes(typeList);
      })
      .catch(() => setError("加载 Provider 配置失败"))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    load();
  }, []);

  const update = (patch: Partial<FormState>) =>
    setForm((prev) => ({ ...prev, ...patch }));

  /** 当前表单对应的载荷指纹 —— 表单变了，旧的测试结论就不再适用。 */
  const draftSignature = JSON.stringify(toTestRequest(form));

  const draftResult =
    draftTest && draftTest.signature === draftSignature
      ? draftTest.result
      : null;

  // ── Dialog helpers ────────────────────────────────────────────────────

  const openCreate = () => {
    setEditing(null);
    setForm(
      blankForm(
        types,
        providers.map((p) => p.name),
      ),
    );
    setAdvancedOpen(false);
    setDraftTest(null);
    setDialogOpen(true);
  };

  const openEdit = (p: ProviderInfo) => {
    setEditing(p);
    setForm(formFromProvider(p));
    setAdvancedOpen(false);
    setDraftTest(null);
    setDialogOpen(true);
  };

  const handleTypeChange = (nextType: string) => {
    setForm((prev) =>
      formForTypeChange(
        prev,
        typeMeta(types, prev.providerType),
        typeMeta(types, nextType),
        { isNew: !editing, taken: providers.map((p) => p.name) },
      ),
    );
  };

  // ── Save ─────────────────────────────────────────────────────────────

  const handleSave = async () => {
    const invalid = validateForm(form, !editing);
    if (invalid) {
      toast.error(invalid);
      return;
    }
    setSaving(true);
    try {
      const res = await upsertProvider(toUpsertRequest(form));
      toast.success(
        res.message ?? (editing ? "Provider 已更新" : "Provider 已添加"),
      );
      setDialogOpen(false);
      load();
    } catch (err) {
      toast.error(apiErrorMessage(err, "保存失败"));
    } finally {
      setSaving(false);
    }
  };

  // ── Delete ───────────────────────────────────────────────────────────

  const handleDelete = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    try {
      await deleteProvider(deleteTarget.name);
      toast.success(`已删除 ${deleteTarget.name}`);
      setDeleteTarget(null);
      load();
    } catch (err) {
      toast.error(apiErrorMessage(err, "删除失败"));
    } finally {
      setDeleting(false);
    }
  };

  // ── Test connection ──────────────────────────────────────────────────

  /** 保存前测试：用表单当前值发一次真实请求，结果留在对话框里。 */
  const handleTestDraft = async () => {
    const signature = draftSignature;
    setDraftTesting(signature);
    try {
      const result = await testProviderDraft(toTestRequest(form));
      setDraftTest({ signature, result });
    } catch (err) {
      setDraftTest({
        signature,
        result: {
          success: false,
          message: apiErrorMessage(err, "连接测试请求失败"),
          provider_type: form.providerType,
          model: form.model,
        },
      });
    } finally {
      // 只清掉自己这一笔的忙碌标记：表单改过之后用户可能已经又点了一次，
      // 无条件置空会把新请求的"测试中…"提前抹掉，让人误以为可以再点一次。
      setDraftTesting((current) => (current === signature ? null : current));
    }
  };

  /** 测试已保存的配置（列表中每个条目）。 */
  const handleTest = async (name: string) => {
    setTesting(name);
    try {
      const result = await testProviderConnection(name);
      if (result.success) {
        toast.success(result.message);
      } else {
        toast.error(result.message);
      }
    } catch (err) {
      toast.error(apiErrorMessage(err, "连接测试失败"));
    } finally {
      setTesting(null);
    }
  };

  // ── Derived ──────────────────────────────────────────────────────────

  const meta = typeMeta(types, form.providerType);
  const draftTestBusy = draftTesting === draftSignature;
  // 探针要真的发一次请求，缺 Key 或模型时后端只会返回"不能为空"类结论
  const draftTestBlocked = !form.apiKey.trim()
    ? editing
      ? "草稿测试需要填入 API Key（已存密钥不回显）；保存后可用列表里的「测试」验证已存配置"
      : "填入 API Key 后可测试连接"
    : !form.model.trim()
      ? "填入默认模型后可测试连接"
      : null;
  const nameTaken =
    !editing && providers.some((p) => p.name === form.name.trim());

  // ── Render ───────────────────────────────────────────────────────────

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-medium">已配置的 Provider</h3>
        <Button
          variant="outline"
          size="sm"
          onClick={openCreate}
          disabled={types.length === 0}
        >
          <Plus className="h-4 w-4 mr-1" />
          添加 Provider
        </Button>
      </div>

      {/* Loading */}
      {loading && (
        <div className="space-y-2">
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
        </div>
      )}

      {/* Error */}
      {!loading && error && (
        <div className="flex items-center gap-2 text-sm text-destructive py-2">
          <AlertCircle className="h-4 w-4" />
          <span>{error}</span>
          <Button variant="outline" size="sm" onClick={load}>
            重试
          </Button>
        </div>
      )}

      {/* Empty */}
      {!loading && !error && providers.length === 0 && (
        <p className="text-sm text-muted-foreground py-4 text-center">
          暂无 Provider 配置，点击上方按钮添加
        </p>
      )}

      {/* Provider list */}
      {!loading &&
        !error &&
        providers.map((p) => {
          const pMeta = typeMeta(types, p.provider_type);
          return (
            <div
              key={p.name}
              className="flex items-start justify-between rounded-lg border p-4"
            >
              <div className="space-y-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="font-medium">{p.name}</span>
                  <Badge variant="secondary" className="text-xs">
                    {pMeta?.display_name ?? p.provider_type}
                  </Badge>
                  {p.is_default && (
                    <Badge variant="outline" className="text-xs">
                      默认
                    </Badge>
                  )}
                </div>
                <p className="text-xs text-muted-foreground truncate">
                  {p.base_url ??
                    `${pMeta?.default_base_url ?? "—"}（类型默认）`}
                  {p.api ? ` · ${p.api}` : ""}
                  {p.default_model ? ` · 模型 ${p.default_model}` : ""}
                </p>
                {!p.has_api_key && (
                  <p className="text-xs text-destructive">未配置 API Key</p>
                )}
              </div>
              <div className="flex items-center gap-1 shrink-0 ml-4">
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => handleTest(p.name)}
                  disabled={testing === p.name}
                >
                  <Wifi className="h-4 w-4 mr-1" />
                  {testing === p.name ? "测试中…" : "测试"}
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-8 w-8"
                  onClick={() => openEdit(p)}
                >
                  <Pencil className="h-4 w-4" />
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-8 w-8 text-destructive hover:text-destructive"
                  onClick={() => setDeleteTarget(p)}
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              </div>
            </div>
          );
        })}

      {/* Upsert Dialog */}
      <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {editing ? "编辑 Provider" : "添加 Provider"}
            </DialogTitle>
            <DialogDescription>
              {editing
                ? `修改「${editing.name}」的连接配置`
                : "选择类型后地址与模型已按默认值填好，填入 API Key 即可"}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="provider-type">Provider 类型</Label>
              <Select
                value={form.providerType}
                onValueChange={handleTypeChange}
              >
                <SelectTrigger id="provider-type">
                  <SelectValue placeholder="选择类型" />
                </SelectTrigger>
                <SelectContent>
                  {types.map((t) => (
                    <SelectItem key={t.provider_type} value={t.provider_type}>
                      {t.display_name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            <div className="space-y-2">
              <Label htmlFor="provider-name">名称</Label>
              <Input
                id="provider-name"
                value={form.name}
                onChange={(e) => update({ name: e.target.value })}
                placeholder={form.providerType || "deepseek"}
                disabled={!!editing}
                autoComplete="off"
              />
              <p className="text-xs text-muted-foreground">
                {editing
                  ? "名称是 agent.md 中 llm.provider 引用的 key，创建后不可修改"
                  : nameTaken
                    ? `已存在同名 Provider，保存将更新它的配置（不是新建）`
                    : "agent.md 中 llm.provider 引用的 key；同名端点请另起别名"}
              </p>
            </div>

            <div className="space-y-2">
              <Label htmlFor="provider-api-key">API Key</Label>
              <Input
                id="provider-api-key"
                type="password"
                value={form.apiKey}
                onChange={(e) => update({ apiKey: e.target.value })}
                placeholder={
                  editing
                    ? "留空则保留已存密钥"
                    : form.providerType === "deepseek"
                      ? "${DEEPSEEK_API_KEY}"
                      : "${ENV_VAR} 或直接填入密钥"
                }
                autoComplete="off"
              />
            </div>

            <div className="space-y-2">
              <Label htmlFor="provider-base-url">Base URL</Label>
              <Input
                id="provider-base-url"
                value={form.baseUrl}
                onChange={(e) => update({ baseUrl: e.target.value })}
                placeholder={meta?.default_base_url ?? "https://…"}
                autoComplete="off"
              />
              <p className="text-xs text-muted-foreground">
                {meta
                  ? `默认 ${meta.default_base_url}；留空则用该地址。自建兼容端点可改为自己的地址`
                  : "留空则用该类型的默认地址"}
              </p>
            </div>

            <div className="space-y-2">
              <Label htmlFor="provider-model">默认模型</Label>
              <Input
                id="provider-model"
                list="provider-model-options"
                value={form.model}
                onChange={(e) => update({ model: e.target.value })}
                placeholder={meta?.default_model ?? "deepseek-v4-flash"}
                autoComplete="off"
              />
              <datalist id="provider-model-options">
                {meta?.suggested_models.map((m) => (
                  <option key={m} value={m} />
                ))}
              </datalist>
              <p className="text-xs text-muted-foreground">
                Agent 未在 agent.md 指定 llm.model 时用它
              </p>
            </div>

            <Collapsible open={advancedOpen} onOpenChange={setAdvancedOpen}>
              <CollapsibleTrigger asChild>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  className="w-full justify-between px-0"
                >
                  高级选项
                  <ChevronDown
                    className={cn(
                      "h-4 w-4 transition-transform",
                      advancedOpen && "rotate-180",
                    )}
                  />
                </Button>
              </CollapsibleTrigger>
              <CollapsibleContent className="space-y-4 pt-2">
                <div className="space-y-2">
                  <Label htmlFor="provider-api">API 风格</Label>
                  <Select
                    value={form.api || API_DEFAULT}
                    onValueChange={(v) =>
                      update({ api: v === API_DEFAULT ? "" : v })
                    }
                  >
                    <SelectTrigger id="provider-api">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value={API_DEFAULT}>
                        {meta
                          ? `跟随类型默认（${meta.api_modes[0]}）`
                          : "跟随类型默认"}
                      </SelectItem>
                      {meta?.api_modes.map((m) => (
                        <SelectItem key={m} value={m}>
                          {m}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
                <div className="flex items-center justify-between">
                  <div className="space-y-0.5">
                    <Label htmlFor="provider-default">设为默认 Provider</Label>
                    <p className="text-xs text-muted-foreground">
                      agent.md 未指定 provider 的 Agent 走它
                    </p>
                  </div>
                  <Switch
                    id="provider-default"
                    checked={form.setDefault}
                    onCheckedChange={(v) => update({ setDefault: v })}
                    disabled={editing?.is_default}
                  />
                </div>
              </CollapsibleContent>
            </Collapsible>
          </div>

          {/* Draft test result — 失败原因（HTTP 状态 / 上游错误体）完整展示在这儿 */}
          {draftResult && (
            <div
              className={cn(
                "rounded-md border px-3 py-2 text-xs break-words whitespace-pre-wrap",
                draftResult.success
                  ? "border-emerald-500/40 bg-emerald-500/5 text-emerald-700 dark:text-emerald-400"
                  : "border-destructive/40 bg-destructive/5 text-destructive",
              )}
            >
              <div className="flex items-start gap-2">
                {draftResult.success ? (
                  <CheckCircle2 className="h-4 w-4 shrink-0 mt-px" />
                ) : (
                  <AlertCircle className="h-4 w-4 shrink-0 mt-px" />
                )}
                <span>{draftResult.message}</span>
              </div>
            </div>
          )}

          <DialogFooter className="sm:justify-between">
            <div className="flex flex-col gap-1">
              <Button
                type="button"
                variant="outline"
                onClick={handleTestDraft}
                disabled={draftTestBusy || !!draftTestBlocked}
              >
                {draftTestBusy ? (
                  <Loader2 className="h-4 w-4 mr-1 animate-spin" />
                ) : (
                  <Wifi className="h-4 w-4 mr-1" />
                )}
                {draftTestBusy ? "测试中…" : "测试连接"}
              </Button>
              {draftTestBlocked && (
                <span className="text-xs text-muted-foreground">
                  {draftTestBlocked}
                </span>
              )}
            </div>
            <div className="flex flex-col-reverse gap-2 sm:flex-row">
              <Button variant="outline" onClick={() => setDialogOpen(false)}>
                取消
              </Button>
              <Button onClick={handleSave} disabled={saving}>
                {saving ? "保存中…" : "保存"}
              </Button>
            </div>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Delete confirmation */}
      <Dialog
        open={!!deleteTarget}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>删除 Provider</DialogTitle>
            <DialogDescription>
              确定要删除 Provider「{deleteTarget?.name}」吗？此操作不可撤销。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setDeleteTarget(null)}
              disabled={deleting}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={deleting}
            >
              {deleting ? "删除中…" : "删除"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
