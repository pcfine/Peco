// provider 配置的两个写入口：团队能力根（`/teams/{id}/providers`）与系统层
// （`/providers`，全机一份、所有团队继承）。两者请求体与响应形状完全一致，只差
// 一个根路径，路径里已带归属，与「当前空间」无关，所以这些请求不带 `X-Peco-Space`。

import { api } from "@/lib/api";

/** `GET .../providers` 的条目。密钥永不回读，只暴露是否已配置。 */
export interface ProviderInfo {
  /** providers.toml 的 key，也是 agent.md `llm.provider` 引用它的名字。 */
  name: string;
  provider_type: string;
  /** `null` = 用该类型的默认地址。 */
  base_url: string | null;
  /** `null` = 没设默认模型，模型由 agent.md 决定。 */
  default_model: string | null;
  /** API 风格；`null` = 用该类型的默认风格。 */
  api: string | null;
  /** 密钥能不能解析出非空值；`false` 时 `api_key_status` 说明差在哪一步。 */
  has_api_key: boolean;
  /**
   * 密钥状态：`missing` 没写、`literal` 字面量、`env_resolved` 变量解得出来、
   * `env_unresolved` 引用的变量没设或为空。旧响应没有这个字段。
   */
  api_key_status?: ApiKeyStatus;
  /** `env_*` 两档引用的变量名 —— 只有变量名，永远不会是值。 */
  api_key_env?: string | null;
  is_default: boolean;
  /**
   * 这条配置来自哪一层：`team` 是本团队文件里写的（可改），`peco` 与 `system`
   * 是继承来的（只读）。
   */
  source?: ProviderSource;
}

/** 生效 provider 的层来源；低 → 高为 `system` → `peco` → `team`。 */
export type ProviderSource = "team" | "peco" | "system";

/** 密钥状态；见 {@link ProviderInfo.api_key_status}。 */
export type ApiKeyStatus =
  "missing" | "literal" | "env_resolved" | "env_unresolved";

/** 条目来自哪一层。字段缺失按 `team` 处理：整列表一律只读比误判更糟。 */
export function providerSource(provider: ProviderInfo): ProviderSource {
  return provider.source ?? "team";
}

/** 密钥状态兜底：旧响应只有 `has_api_key` 时按「已配置字面量 / 没配」二分。 */
export function apiKeyStatus(provider: ProviderInfo): ApiKeyStatus {
  return (
    provider.api_key_status ?? (provider.has_api_key ? "literal" : "missing")
  );
}

/**
 * 配置范围：系统层（本机一份，所有团队继承）或某个团队。
 *
 * 两个 scope 写的是两个文件，能改的条目也各是各的，所以「范围」是这一页的
 * 一等状态，而不是一个可选的 teamId。
 */
export type ProviderScope =
  { kind: "system" } | { kind: "team"; teamId: string };

/**
 * scope → 查询键里的稳定片段。
 *
 * 系统层不能直接用 `"system"`：合法团队 id 可以就叫 `system`，两者会撞进同一份
 * 缓存。`__system__` 不可能被团队 id 占用（团队 id 必须以字母开头）。
 */
export const SYSTEM_SCOPE_KEY = "__system__";

export function scopeKey(scope: ProviderScope): string {
  return scope.kind === "system" ? SYSTEM_SCOPE_KEY : scope.teamId;
}

/** 生效 provider 列表的查询键。 */
export function providersQueryKey(scope: ProviderScope) {
  return ["providers", scopeKey(scope)] as const;
}

/** provider 类型目录的查询键。 */
export function providerTypesQueryKey(scope: ProviderScope) {
  return ["provider-types", scopeKey(scope)] as const;
}

/** provider 类型目录 —— 表单默认值的唯一来源，前端不另存一份映射表。 */
export interface ProviderTypeInfo {
  provider_type: string;
  display_name: string;
  default_base_url: string;
  /** 合法 API 风格，首项为该类型的默认风格。 */
  api_modes: string[];
  /** 常用模型名，仅作候选提示。 */
  suggested_models: string[];
  default_model: string;
}

/**
 * `PUT .../providers` 请求体：字段级部分更新 —— 省略表示保留已存值，空串表示清空。
 *
 * `api_key` 无法回读，编辑时必须省略而不是传空串，否则会清掉已存密钥。
 */
export interface UpsertProviderRequest {
  /** 省略时服务端回退为 `type`。 */
  name?: string;
  type: string;
  api_key?: string;
  base_url?: string;
  api?: string;
  default_model?: string;
  set_default?: boolean;
}

/** `POST .../providers/test` 请求体：草稿没有已存值可回退，字段直传。 */
export interface TestDraftRequest {
  name?: string;
  type: string;
  api_key: string;
  base_url: string;
  api: string;
  default_model: string;
}

/** 连接探针结论；`provider_type` / `model` 是实际探到的值，用于定位。 */
export interface TestResponse {
  success: boolean;
  message: string;
  provider_type: string;
  model: string;
}

export interface ProviderMutationResponse {
  success: boolean;
  message?: string;
}

function providersRoot(scope: ProviderScope): string {
  return scope.kind === "system"
    ? "/providers"
    : `/teams/${encodeURIComponent(scope.teamId)}/providers`;
}

/** 某个 scope 自己写的那份配置（系统层只有系统层的条目，不含继承）。 */
export async function listProvidersFor(
  scope: ProviderScope,
): Promise<ProviderInfo[]> {
  const res = await api.get<ProviderInfo[]>(providersRoot(scope));
  return res.data;
}

export async function listProviderTypesFor(
  scope: ProviderScope,
): Promise<ProviderTypeInfo[]> {
  const res = await api.get<ProviderTypeInfo[]>(
    `${providersRoot(scope)}/types`,
  );
  return res.data;
}

export async function upsertProviderFor(
  scope: ProviderScope,
  body: UpsertProviderRequest,
): Promise<ProviderMutationResponse> {
  const res = await api.put<ProviderMutationResponse>(
    providersRoot(scope),
    body,
  );
  return res.data;
}

export async function deleteProviderFor(
  scope: ProviderScope,
  name: string,
): Promise<ProviderMutationResponse> {
  const res = await api.delete<ProviderMutationResponse>(
    `${providersRoot(scope)}/${encodeURIComponent(name)}`,
  );
  return res.data;
}

/** 用**表单当前值**测一次真实请求：不落盘，保存前可测。 */
export async function testProviderDraftFor(
  scope: ProviderScope,
  body: TestDraftRequest,
): Promise<TestResponse> {
  const res = await api.post<TestResponse>(
    `${providersRoot(scope)}/test`,
    body,
  );
  return res.data;
}

/** 用**已保存**的配置测一次真实请求。 */
export async function testProviderConnectionFor(
  scope: ProviderScope,
  name: string,
): Promise<TestResponse> {
  const res = await api.post<TestResponse>(
    `${providersRoot(scope)}/${encodeURIComponent(name)}/test`,
    undefined,
  );
  return res.data;
}

// 只按团队寻址的调用方（模型选择器等）保留 teamId 签名，行为与路径不变。

export function listProviders(teamId: string): Promise<ProviderInfo[]> {
  return listProvidersFor({ kind: "team", teamId });
}

export function listProviderTypes(teamId: string): Promise<ProviderTypeInfo[]> {
  return listProviderTypesFor({ kind: "team", teamId });
}

export function upsertProvider(
  teamId: string,
  body: UpsertProviderRequest,
): Promise<ProviderMutationResponse> {
  return upsertProviderFor({ kind: "team", teamId }, body);
}

export function deleteProvider(
  teamId: string,
  name: string,
): Promise<ProviderMutationResponse> {
  return deleteProviderFor({ kind: "team", teamId }, name);
}

export function testProviderDraft(
  teamId: string,
  body: TestDraftRequest,
): Promise<TestResponse> {
  return testProviderDraftFor({ kind: "team", teamId }, body);
}

export function testProviderConnection(
  teamId: string,
  name: string,
): Promise<TestResponse> {
  return testProviderConnectionFor({ kind: "team", teamId }, name);
}
