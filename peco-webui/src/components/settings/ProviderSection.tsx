// Provider 配置区：团队能力根下 providers.toml 的增删改与试连。
//
// 表单以「类型」为起点：类型一定，默认地址与默认模型就按目录自动填好，通常只需
// 粘一个 API Key。地址与模型仍可手改（自建兼容端点），但不会让人对着空输入框
// 手打默认值。
//
// 同一个组件服务两个配置范围：团队层与系统层（`scope`）。列表是生效并集：`team`
// 是当前团队文件里的条目，`peco` / `system` 是继承来的。能改的只有当前范围自己
// 写下的那几条 —— 继承条目在上层文件里，删除只会得到 400，唯一能做的是在本层写
// 下同名条目把它覆盖掉。

import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus, Trash2, Wifi } from "lucide-react";
import { toast } from "sonner";
import { EmptyState } from "@/components/common/EmptyState";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import {
  apiKeyStatus,
  deleteProviderFor,
  listProviderTypesFor,
  listProvidersFor,
  providerSource,
  providerTypesQueryKey,
  providersQueryKey,
  testProviderConnectionFor,
  testProviderDraftFor,
  upsertProviderFor,
} from "@/api/providers";
import type {
  ApiKeyStatus,
  ProviderInfo,
  ProviderScope,
  ProviderTypeInfo,
  TestDraftRequest,
  TestResponse,
  UpsertProviderRequest,
} from "@/api/providers";
import { errorText } from "@/lib/api";
import { cn } from "@/lib/utils";

/**
 * `api` 选择器里代表「跟随类型默认风格」的哨兵值。
 *
 * radix Select 不接受空字符串 value，而载荷里的空串含义正是「适配器用类型默认
 * 风格」，所以在选择器边界上做一次映射。
 */
const API_DEFAULT = "__type_default__";

// ── 表单模型（纯函数，测试直接调用） ──────────────────────────────────────

/** 表单状态 —— 与 provider 的可写字段一一对应。 */
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
 * 已存条目 → 表单；密钥不回读，`apiKey` 一律留空。
 *
 * 只回显**已存值**，不拿目录默认值补空：补空会让「只改密钥」这种编辑在保存时
 * 把目录默认地址/模型显式写进配置（原本为空表示跟随类型默认），用户没改的字段
 * 就被静默改掉了。
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

/**
 * 继承条目 → 覆盖表单：名字锁定为该条目名，其余字段填**生效值**。
 *
 * 填生效值而不是留空：留空的 `base_url` 会以空串写进本团队文件，把继承来的地址
 * 冲成类型默认地址 —— 覆盖之后行为就变了。
 */
export function formForOverride(provider: ProviderInfo): FormState {
  return { ...formFromProvider(provider), setDefault: false };
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
 * 只覆盖「为空或仍是上一个类型默认值」的字段 —— 用户手改过的地址/模型
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
    // api 档位是类型特有的，新类型没有该档位就退回「跟随默认」
    api: next.api_modes.includes(form.api) ? form.api : "",
  };
}

/** 表单 → `PUT .../providers` 请求体。 */
export function toUpsertRequest(form: FormState): UpsertProviderRequest {
  const req: UpsertProviderRequest = {
    name: form.name.trim() || form.providerType,
    type: form.providerType,
    // 空串 = 用该类型的默认地址（后端把空串当「清空该字段」）
    base_url: form.baseUrl.trim(),
    api: form.api,
    default_model: form.model.trim(),
  };
  // 省略 api_key = 保留已存密钥；新建时省略等价于「没有密钥」，语义一致。
  // 不能传空串：后端把空串解释为「清空该字段」，编辑时留空会静默删掉已存密钥。
  if (form.apiKey.trim()) req.api_key = form.apiKey.trim();
  if (form.setDefault) req.set_default = true;
  return req;
}

/** 表单 → `POST .../providers/test` 请求体。草稿没有已存值可回退，字段直传。 */
export function toTestRequest(form: FormState): TestDraftRequest {
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
 * 密钥与模型只在新建时必填：编辑时两者留空都表示「保持已存值」，而历史配置本就
 * 可以没有默认模型 —— 强制填会让「只想换个密钥」的编辑保存不出去。
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

// ── 组件 ─────────────────────────────────────────────────────────────────

/** 密钥状态的展示文案：不能解析时必须点名是哪个变量没设。 */
function keyStatusLabel(provider: ProviderInfo, status: ApiKeyStatus): string {
  if (status === "env_unresolved") {
    return `密钥引用的 ${provider.api_key_env ?? "环境变量"} 未设置`;
  }
  return status === "missing" ? "未配置密钥" : "已配置密钥";
}

/** 变量没设时的可操作指向：两条路都不需要用户去找文件。 */
function envHint(env: string | null | undefined): string {
  const name = env ?? "环境变量";
  return `给 daemon 的环境变量 ${name} 赋一个非空值后重启 daemon；或在本层改填字面量密钥（同名条目即覆盖）。`;
}

/**
 * 这一条是不是当前范围自己能改的。
 *
 * 「可写」与「已占用的名字」必须是同一个谓词：系统范围下沿用「team 层才算自己
 * 写的」，本层重名校验会恒空 —— 保存就变成静默覆盖。
 */
export function writable(
  provider: ProviderInfo,
  scope: ProviderScope,
): boolean {
  return (
    providerSource(provider) === (scope.kind === "system" ? "system" : "team")
  );
}

export function ProviderSection({ scope }: { scope: ProviderScope }) {
  const queryClient = useQueryClient();
  const providersQuery = useQuery({
    queryKey: providersQueryKey(scope),
    queryFn: () => listProvidersFor(scope),
  });
  // 目录是一份编译期常量，按 scope 维度缓存，别为它反复打网络。
  const typesQuery = useQuery({
    queryKey: providerTypesQueryKey(scope),
    queryFn: () => listProviderTypesFor(scope),
    staleTime: 5 * 60_000,
  });

  const providers = providersQuery.data ?? [];
  const types = typesQuery.data ?? [];
  const loadError = providersQuery.error ?? typesQuery.error;
  const loading = providersQuery.isPending || typesQuery.isPending;

  const [dialogOpen, setDialogOpen] = useState(false);
  const [editing, setEditing] = useState<ProviderInfo | null>(null);
  /** 正在被覆盖的继承条目：名字锁定，保存后成为本团队的同名条目。 */
  const [overrideTarget, setOverrideTarget] = useState<ProviderInfo | null>(
    null,
  );
  const [form, setForm] = useState<FormState>(() => blankForm([]));
  const [formError, setFormError] = useState<string | null>(null);
  /** 草稿试连结论；带上载荷指纹，表单改过之后旧结论就不再展示。 */
  const [draftTest, setDraftTest] = useState<{
    signature: string;
    result: TestResponse;
  } | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<ProviderInfo | null>(null);
  /** 已保存条目最近一次试连的结论，挂在对应行下方。 */
  const [tested, setTested] = useState<{
    name: string;
    ok: boolean;
    message: string;
  } | null>(null);

  // 前缀失效：系统层改动会改掉所有团队的生效并集，只失效当前 scope 会让别的
  // 团队页留着旧值。
  const refreshProviders = () =>
    queryClient.invalidateQueries({ queryKey: ["providers"] });

  const save = useMutation({
    mutationFn: (body: UpsertProviderRequest) => upsertProviderFor(scope, body),
    onSuccess: (res) => {
      void refreshProviders();
      setDialogOpen(false);
      toast.success(res.message ?? "Provider 已保存");
    },
  });

  const remove = useMutation({
    mutationFn: (name: string) => deleteProviderFor(scope, name),
    onSuccess: (res) => {
      void refreshProviders();
      setDeleteTarget(null);
      toast.success(res.message ?? "Provider 已删除");
    },
  });

  const testSaved = useMutation({
    mutationFn: (name: string) => testProviderConnectionFor(scope, name),
    onSuccess: (res, name) =>
      setTested({ name, ok: res.success, message: res.message }),
    onError: (error, name) =>
      setTested({ name, ok: false, message: errorText(error) }),
  });

  const testDraftMutation = useMutation({
    mutationFn: (input: { signature: string; body: TestDraftRequest }) =>
      testProviderDraftFor(scope, input.body),
    onSuccess: (result, input) =>
      setDraftTest({ signature: input.signature, result }),
    onError: (error, input) =>
      setDraftTest({
        signature: input.signature,
        result: {
          success: false,
          message: errorText(error),
          provider_type: input.body.type,
          model: input.body.default_model,
        },
      }),
  });

  const update = (patch: Partial<FormState>) =>
    setForm((prev) => ({ ...prev, ...patch }));

  // 只有本范围写的名字算占用：继承条目正是「覆盖」的目标，算进去就没有可用的覆盖名了。
  const ownNames = providers
    .filter((provider) => writable(provider, scope))
    .map((provider) => provider.name);
  const nameLocked = editing !== null || overrideTarget !== null;

  const openCreate = () => {
    setEditing(null);
    setOverrideTarget(null);
    setForm(blankForm(types, ownNames));
    setFormError(null);
    setDraftTest(null);
    save.reset();
    setDialogOpen(true);
  };

  const openEdit = (provider: ProviderInfo) => {
    setEditing(provider);
    setOverrideTarget(null);
    setForm(formFromProvider(provider));
    setFormError(null);
    setDraftTest(null);
    save.reset();
    setDialogOpen(true);
  };

  const openOverride = (provider: ProviderInfo) => {
    setEditing(null);
    setOverrideTarget(provider);
    setForm(formForOverride(provider));
    setFormError(null);
    setDraftTest(null);
    save.reset();
    setDialogOpen(true);
  };

  const closeDialog = () => {
    setDialogOpen(false);
    setOverrideTarget(null);
    setFormError(null);
    save.reset();
  };

  const closeDelete = () => {
    setDeleteTarget(null);
    remove.reset();
  };

  const changeType = (nextType: string) => {
    setForm((prev) =>
      formForTypeChange(
        prev,
        typeMeta(types, prev.providerType),
        typeMeta(types, nextType),
        { isNew: !nameLocked, taken: ownNames },
      ),
    );
  };

  const submit = () => {
    const invalid = validateForm(form, !editing);
    if (invalid) {
      setFormError(invalid);
      return;
    }
    setFormError(null);
    save.mutate(toUpsertRequest(form));
  };

  const testRequest = toTestRequest(form);
  const draftSignature = JSON.stringify(testRequest);
  const draftResult =
    draftTest && draftTest.signature === draftSignature
      ? draftTest.result
      : null;

  const meta = typeMeta(types, form.providerType);
  // 探针要真的发一次请求：缺密钥或模型时后端只会回「不能为空」类结论。
  const draftTestBlocked = !form.apiKey.trim()
    ? editing
      ? editing.has_api_key
        ? "已存密钥不回显：填入密钥可先测草稿，或保存后用列表里的「试连」验证"
        : "这个 provider 还没有密钥，填入 API Key 后可试连"
      : "填入 API Key 后可试连"
    : !form.model.trim()
      ? "填入默认模型后可试连"
      : null;
  const nameTaken = !nameLocked && ownNames.includes(form.name.trim());
  // 密钥解析不出来的条目当默认 = 每一轮对话都失败，先修密钥再说
  const defaultBlocked =
    editing !== null && apiKeyStatus(editing) === "env_unresolved";

  return (
    <section className="space-y-3">
      <header className="flex h-8 items-center gap-2 border-b border-line">
        <h2 className="text-sm font-semibold text-ink">Provider</h2>
        <span className="tabular-nums text-2xs text-ink-faint">
          {providers.length}
        </span>
        <span className="min-w-0 truncate text-caption font-medium text-ink-faint">
          {scope.kind === "system"
            ? "所有团队未配置的 provider 都在这里兜底；这一层不属于任何团队"
            : "providers.toml · 名称即 agent.md 里 llm.provider 引用的 key"}
        </span>
        <Button
          size="sm"
          className="ml-auto shrink-0"
          disabled={types.length === 0}
          onClick={openCreate}
        >
          <Plus />
          新建 Provider
        </Button>
      </header>

      {loadError !== null && (
        <ErrorBanner
          message={errorText(loadError)}
          onRetry={() => {
            void providersQuery.refetch();
            void typesQuery.refetch();
          }}
        />
      )}

      {loading && (
        <div className="space-y-px">
          {[0, 1, 2].map((row) => (
            <Skeleton key={row} className="h-10 w-full" />
          ))}
        </div>
      )}

      {!loading && !loadError && providers.length === 0 && (
        <EmptyState
          title={
            scope.kind === "system"
              ? "系统层还没有配置 provider：没配 provider 的团队会在对话时报错"
              : "这个团队还没有配置 provider，对话会走系统层默认值"
          }
        />
      )}

      <ul>
        {providers.map((provider) => {
          const providerMeta = typeMeta(types, provider.provider_type);
          const outcome = tested?.name === provider.name ? tested : null;
          const testing =
            testSaved.isPending && testSaved.variables === provider.name;
          const source = providerSource(provider);
          const inherited = !writable(provider, scope);
          const keyStatus = apiKeyStatus(provider);
          return (
            <li key={provider.name} className="border-b border-line">
              <div className="flex flex-col justify-center gap-0.5 px-3 py-1 @min-[760px]/work:h-10 @min-[760px]/work:flex-row @min-[760px]/work:items-center @min-[760px]/work:gap-2 @min-[760px]/work:py-0">
                <div className="flex min-w-0 items-center gap-2 @min-[760px]/work:w-56 @min-[760px]/work:shrink-0">
                  <span className="min-w-0 truncate font-mono text-sm text-ink">
                    {provider.name}
                  </span>
                  {inherited && (
                    <span className="shrink-0 rounded-sm bg-line/60 px-1 text-caption font-medium text-ink-faint">
                      {source === "peco"
                        ? "继承自 Peco"
                        : "继承自 系统（本机）"}
                    </span>
                  )}
                  {provider.is_default && (
                    <span className="shrink-0 rounded-sm bg-line/60 px-1 text-caption font-medium text-ink-faint">
                      默认
                    </span>
                  )}
                  <span
                    data-testid="key-status"
                    className={cn(
                      "shrink-0 text-caption font-medium",
                      keyStatus === "env_unresolved" || keyStatus === "missing"
                        ? "text-danger"
                        : "text-ink-faint",
                    )}
                  >
                    {keyStatusLabel(provider, keyStatus)}
                  </span>
                </div>
                <span className="truncate text-caption font-medium text-ink-muted @min-[760px]/work:w-24 @min-[760px]/work:shrink-0">
                  {providerMeta?.display_name ?? provider.provider_type}
                </span>
                <span className="truncate font-mono text-caption font-medium text-ink-faint @min-[760px]/work:flex-1">
                  {provider.base_url ??
                    (providerMeta
                      ? `${providerMeta.default_base_url}（类型默认）`
                      : "类型默认地址")}
                </span>
                <span className="truncate text-caption font-medium text-ink-faint @min-[760px]/work:w-40 @min-[760px]/work:shrink-0">
                  {provider.default_model ?? "未设默认模型"}
                  {provider.api ? ` · ${provider.api}` : ""}
                </span>
                <div className="flex shrink-0 items-center gap-1">
                  {inherited ? (
                    // 覆盖 = 在本层文件里写下同名条目；上层文件不在这里，改不到。
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2"
                      onClick={() => openOverride(provider)}
                    >
                      覆盖
                    </Button>
                  ) : (
                    <>
                      {/* 试连是一次真实计费请求：并发的第二个没有意义。 */}
                      <Button
                        variant="ghost"
                        size="sm"
                        className="h-7 px-2"
                        disabled={testSaved.isPending}
                        onClick={() => testSaved.mutate(provider.name)}
                      >
                        <Wifi />
                        {testing ? "试连中…" : "试连"}
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        className="text-ink-faint"
                        aria-label={`编辑 ${provider.name}`}
                        onClick={() => openEdit(provider)}
                      >
                        <Pencil />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        className="text-ink-faint hover:text-danger"
                        aria-label={`删除 ${provider.name}`}
                        onClick={() => {
                          setDeleteTarget(provider);
                          remove.reset();
                        }}
                      >
                        <Trash2 />
                      </Button>
                    </>
                  )}
                </div>
              </div>
              {keyStatus === "env_unresolved" && (
                <p className="border-t border-line px-3 py-1 text-caption font-medium text-danger">
                  {envHint(provider.api_key_env)}
                </p>
              )}
              {outcome && (
                <p
                  className={cn(
                    "border-t border-line px-3 py-1 text-caption font-medium",
                    outcome.ok ? "text-ink-muted" : "text-danger",
                  )}
                >
                  {outcome.ok ? "试连成功 · " : "试连失败 · "}
                  {outcome.message}
                </p>
              )}
            </li>
          );
        })}
      </ul>

      <Dialog
        open={dialogOpen}
        onOpenChange={(open) => {
          if (!open) closeDialog();
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {editing
                ? `编辑「${editing.name}」`
                : overrideTarget
                  ? `覆盖「${overrideTarget.name}」`
                  : "新建 Provider"}
            </DialogTitle>
            <DialogDescription>
              {editing
                ? "留空的字段保持已存值；密钥不回读，只能覆盖不能查看。"
                : overrideTarget
                  ? scope.kind === "system"
                    ? "同名条目会写进系统层，所有团队都受影响；它是继承链的最底层，没有上一层可继承。"
                    : "同名条目会写进本团队的 providers.toml，从此这一层说了算；未填的字段继续继承上一层。"
                  : "类型选好后地址与模型已按默认值填好，填入 API Key 即可。"}
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-3">
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <div className="space-y-1">
                <span className="text-caption font-medium text-ink-faint">
                  类型
                </span>
                <Select
                  value={form.providerType}
                  onValueChange={changeType}
                  disabled={save.isPending}
                >
                  <SelectTrigger
                    className="h-8 w-full"
                    aria-label="Provider 类型"
                  >
                    <SelectValue placeholder="选择类型" />
                  </SelectTrigger>
                  <SelectContent>
                    {types.map((type) => (
                      <SelectItem
                        key={type.provider_type}
                        value={type.provider_type}
                      >
                        {type.display_name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>

              <div className="space-y-1">
                <label className="block space-y-1">
                  <span className="text-caption font-medium text-ink-faint">
                    名称
                  </span>
                  <Input
                    value={form.name}
                    onChange={(event) => update({ name: event.target.value })}
                    placeholder={form.providerType || "deepseek"}
                    disabled={nameLocked || save.isPending}
                    autoComplete="off"
                    className="h-8 font-mono text-xs"
                  />
                </label>
                <p className="text-caption font-medium text-ink-faint">
                  {editing
                    ? "名称是 agent.md 里 llm.provider 引用的 key，创建后不可修改"
                    : overrideTarget
                      ? "名称锁定为该条目名：覆盖靠同名条目生效"
                      : nameTaken
                        ? "已存在同名 Provider，保存将更新它的配置（不是新建）"
                        : "同名端点请另起别名"}
                </p>
              </div>

              <div className="space-y-1">
                <label className="block space-y-1">
                  <span className="text-2xs text-ink-faint">API Key</span>
                  <Input
                    type="password"
                    value={form.apiKey}
                    onChange={(event) => update({ apiKey: event.target.value })}
                    placeholder={
                      editing
                        ? "留空则保留已存密钥"
                        : form.providerType === "deepseek"
                          ? "${DEEPSEEK_API_KEY}"
                          : "${ENV_VAR} 或直接填入密钥"
                    }
                    autoComplete="off"
                    className="h-8 font-mono text-xs"
                  />
                </label>
                <p className="text-caption font-medium text-ink-faint">
                  {editing
                    ? editing.has_api_key
                      ? "已配置密钥；留空表示不修改"
                      : "这个 provider 还没有密钥；留空保存不会凭空生成"
                    : "可填 ${ENV_VAR}，由服务端从环境变量读取"}
                </p>
              </div>

              <div className="space-y-1">
                <label className="block space-y-1">
                  <span className="text-2xs text-ink-faint">Base URL</span>
                  <Input
                    value={form.baseUrl}
                    onChange={(event) =>
                      update({ baseUrl: event.target.value })
                    }
                    placeholder={meta?.default_base_url ?? "https://…"}
                    autoComplete="off"
                    className="h-8 font-mono text-xs"
                  />
                </label>
                <p className="text-caption font-medium text-ink-faint">
                  {meta
                    ? `留空则用 ${meta.default_base_url}；自建兼容端点可改成自己的地址`
                    : "留空则用该类型的默认地址"}
                </p>
              </div>

              <div className="space-y-1">
                <label className="block space-y-1">
                  <span className="text-caption font-medium text-ink-faint">
                    默认模型
                  </span>
                  <Input
                    list="provider-model-options"
                    value={form.model}
                    onChange={(event) => update({ model: event.target.value })}
                    placeholder={meta?.default_model ?? "deepseek-flash"}
                    autoComplete="off"
                    className="h-8 font-mono text-xs"
                  />
                </label>
                <datalist id="provider-model-options">
                  {meta?.suggested_models.map((model) => (
                    <option key={model} value={model} />
                  ))}
                </datalist>
                <p className="text-caption font-medium text-ink-faint">
                  agent.md 未指定 llm.model 时用它
                </p>
              </div>

              <div className="space-y-1">
                <span className="text-caption font-medium text-ink-faint">
                  API 风格
                </span>
                <Select
                  value={form.api || API_DEFAULT}
                  onValueChange={(value) =>
                    update({ api: value === API_DEFAULT ? "" : value })
                  }
                  disabled={save.isPending}
                >
                  <SelectTrigger className="h-8 w-full" aria-label="API 风格">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={API_DEFAULT}>
                      {meta
                        ? `跟随类型默认（${meta.api_modes[0]}）`
                        : "跟随类型默认"}
                    </SelectItem>
                    {meta?.api_modes.map((mode) => (
                      <SelectItem key={mode} value={mode}>
                        {mode}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-caption font-medium text-ink-faint">
                  两种风格的请求体不同；不确定就跟随默认
                </p>
              </div>
            </div>

            <div className="flex items-center gap-2 border-t border-line pt-2">
              <Switch
                id="provider-default"
                checked={form.setDefault}
                onCheckedChange={(checked) => update({ setDefault: checked })}
                disabled={
                  editing?.is_default === true ||
                  save.isPending ||
                  defaultBlocked
                }
              />
              <label htmlFor="provider-default" className="text-xs text-ink">
                设为默认 provider
              </label>
              <span className="text-caption font-medium text-ink-faint">
                {editing?.is_default
                  ? "它已经是默认 provider"
                  : defaultBlocked
                    ? "它的密钥解析不出来，先修好密钥再设为默认"
                    : "agent.md 未指定 provider 的 agent 走它"}
              </span>
            </div>

            {draftResult && (
              <p
                className={cn(
                  "whitespace-pre-wrap break-words border-l-2 pl-2 text-caption font-medium",
                  draftResult.success
                    ? "border-line-strong text-ink-muted"
                    : "border-danger text-danger",
                )}
              >
                {draftResult.success ? "试连成功 · " : "试连失败 · "}
                {draftResult.message}
              </p>
            )}

            {formError && (
              <p className="text-caption font-medium text-danger">
                {formError}
              </p>
            )}

            {save.error !== null && (
              <ErrorBanner message={errorText(save.error)} />
            )}
          </div>

          <DialogFooter className="sm:justify-between">
            <div className="flex flex-col gap-1">
              <Button
                type="button"
                variant="ghost"
                size="sm"
                disabled={
                  testDraftMutation.isPending || draftTestBlocked !== null
                }
                onClick={() =>
                  testDraftMutation.mutate({
                    signature: draftSignature,
                    body: testRequest,
                  })
                }
              >
                <Wifi />
                {testDraftMutation.isPending ? "试连中…" : "试连"}
              </Button>
              {draftTestBlocked && (
                <span className="text-caption font-medium text-ink-faint">
                  {draftTestBlocked}
                </span>
              )}
            </div>
            <div className="flex flex-col-reverse gap-2 sm:flex-row">
              <Button
                variant="ghost"
                size="sm"
                disabled={save.isPending}
                onClick={closeDialog}
              >
                取消
              </Button>
              <Button size="sm" disabled={save.isPending} onClick={submit}>
                {save.isPending ? "保存中…" : "保存"}
              </Button>
            </div>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) closeDelete();
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>删除 Provider「{deleteTarget?.name}」？</DialogTitle>
            <DialogDescription>
              {scope.kind === "system"
                ? "系统层里的这条配置会被删掉，不可恢复；所有团队都受影响；若删的是默认条目，默认会自动改成剩余里字典序最小的一条。"
                : "providers.toml 里的这条配置会被删掉，不可恢复；引用它的 agent.md 会退回默认 provider。"}
            </DialogDescription>
          </DialogHeader>
          {remove.error !== null && (
            <ErrorBanner message={errorText(remove.error)} />
          )}
          <DialogFooter>
            <Button
              variant="ghost"
              size="sm"
              disabled={remove.isPending}
              onClick={closeDelete}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              size="sm"
              disabled={remove.isPending}
              onClick={() => {
                if (deleteTarget) remove.mutate(deleteTarget.name);
              }}
            >
              {remove.isPending ? "删除中…" : "确认删除"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}
